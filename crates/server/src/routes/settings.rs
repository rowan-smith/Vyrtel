//! Admin-managed settings (SET-04, #253): see [`crate::settings`] and
//! docs/api.md#settings. Every route is admin-scoped ([`super::OPERATIONS`]).

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Extension, State};
use axum::http::{HeaderMap, StatusCode, header};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::Principal;
use crate::error::{ApiError, ApiJson, ApiPath, ApiResult};
use crate::settings::{Edit, SettingsError};
use crate::state::SharedState;

impl From<SettingsError> for ApiError {
    fn from(e: SettingsError) -> Self {
        let message = e.to_string();
        match e {
            SettingsError::UnknownGroup(_) => ApiError::new(StatusCode::NOT_FOUND, "unknown_settings_group", message),
            SettingsError::Unknown(_) => ApiError::new(StatusCode::BAD_REQUEST, "unknown_setting", message),
            SettingsError::Invalid(_) => ApiError::new(StatusCode::BAD_REQUEST, "invalid_setting", message),
            SettingsError::Deployment { key, .. } => {
                ApiError::new(StatusCode::CONFLICT, "setting_managed_by_deployment", message)
                    .with_details(json!({ "key": key }))
            }
            SettingsError::Stale { group, expected, current } => {
                ApiError::new(StatusCode::CONFLICT, "settings_conflict", message)
                    .with_details(json!({ "group": group, "expected": expected, "current": current }))
            }
            SettingsError::ConfirmationRequired(fields) => {
                ApiError::new(StatusCode::PRECONDITION_REQUIRED, "confirmation_required", message)
                    .with_details(json!({ "fields": fields }))
            }
            SettingsError::Store(e) => {
                tracing::error!("saving settings failed: {e}");
                ApiError::from(e)
            }
        }
    }
}

/// Who made a change, for its record: never a credential.
fn actor(principal: &Principal) -> String {
    match principal {
        Principal::User(name) => format!("user:{name}"),
        Principal::ApiKey { id, .. } => format!("api-key:{id}"),
    }
}

/// A change made with a session cookie must come from Vyrtel's own pages:
/// a browser that says the request is cross-site or same-site (another
/// origin), or whose `Origin` is not this host, is refused. API keys are
/// never sent by a browser on its own, so they are not checked.
fn same_origin(principal: &Principal, headers: &HeaderMap) -> ApiResult<()> {
    if !matches!(principal, Principal::User(_)) {
        return Ok(());
    }
    let refuse = || ApiError::forbidden("settings can only be changed from Vyrtel's own pages");
    if let Some(site) = headers.get("sec-fetch-site") {
        return match site.to_str().unwrap_or("") {
            "same-origin" | "none" => Ok(()),
            _ => Err(refuse()),
        };
    }
    if let Some(origin) = headers.get(header::ORIGIN) {
        let origin = origin.to_str().unwrap_or("");
        let authority = origin.split_once("://").map(|(_, a)| a).unwrap_or("");
        let host = headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("");
        if authority.is_empty() || !authority.eq_ignore_ascii_case(host) {
            return Err(refuse());
        }
    }
    Ok(())
}

pub async fn show(State(state): State<SharedState>) -> Json<Value> {
    Json(state.settings.document(&state.config))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveBody {
    /// The group's revision these changes are based on.
    pub revision: u64,
    /// Field name → new value, or `null` to go back to the default.
    pub values: BTreeMap<String, Option<Value>>,
    /// Required when a dangerous field changes.
    #[serde(default)]
    pub confirm: bool,
}

pub async fn save(
    State(state): State<SharedState>,
    Extension(principal): Extension<Principal>,
    headers: HeaderMap,
    ApiPath(group): ApiPath<String>,
    ApiJson(body): ApiJson<SaveBody>,
) -> ApiResult<Json<Value>> {
    same_origin(&principal, &headers)?;
    let edit = Edit::Save { group, revision: body.revision, values: body.values, confirm: body.confirm };
    state.settings.change(&state.metadata, &state.config, actor(&principal), edit).await?;
    Ok(Json(state.settings.document(&state.config)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelBody {
    pub revision: u64,
}

pub async fn cancel(
    State(state): State<SharedState>,
    Extension(principal): Extension<Principal>,
    headers: HeaderMap,
    ApiPath(group): ApiPath<String>,
    ApiJson(body): ApiJson<CancelBody>,
) -> ApiResult<Json<Value>> {
    same_origin(&principal, &headers)?;
    let edit = Edit::Cancel { group, revision: body.revision };
    state.settings.change(&state.metadata, &state.config, actor(&principal), edit).await?;
    Ok(Json(state.settings.document(&state.config)))
}

pub async fn export(State(state): State<SharedState>) -> Json<Value> {
    Json(state.settings.export())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportBody {
    /// Every group's revision the import is based on (missing: 0).
    pub revisions: BTreeMap<String, u64>,
    /// A document from `GET /api/v1/settings/export`.
    pub export: Value,
    #[serde(default)]
    pub confirm: bool,
}

pub async fn import(
    State(state): State<SharedState>,
    Extension(principal): Extension<Principal>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<ImportBody>,
) -> ApiResult<Json<Value>> {
    same_origin(&principal, &headers)?;
    if body.export.to_string().len() > crate::settings::MAX_IMPORT_BYTES {
        return Err(ApiError::bad_request("the settings export is too large"));
    }
    let values = crate::settings::import_values(&body.export)?;
    let edit = Edit::Import { revisions: Some(body.revisions), values, confirm: body.confirm };
    state.settings.change(&state.metadata, &state.config, actor(&principal), edit).await?;
    Ok(Json(state.settings.document(&state.config)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    /// Cookie writes must be same-origin; key writes are not browser-made.
    #[test]
    fn session_writes_must_come_from_the_same_origin() {
        let user = Principal::User("admin".into());
        let key = Principal::ApiKey { id: 1, scope: "admin".into() };
        let host = ("host", "vyrtel.local:8080");
        for ok in [
            vec![],
            vec![host],
            vec![host, ("sec-fetch-site", "same-origin")],
            vec![host, ("sec-fetch-site", "none")],
            vec![host, ("origin", "http://vyrtel.local:8080")],
            vec![host, ("origin", "https://VYRTEL.local:8080")],
        ] {
            same_origin(&user, &headers(&ok)).unwrap_or_else(|e| panic!("{ok:?}: {}", e.message));
        }
        for bad in [
            vec![host, ("sec-fetch-site", "cross-site")],
            vec![host, ("sec-fetch-site", "same-site")],
            vec![host, ("origin", "http://evil.example")],
            vec![host, ("origin", "http://vyrtel.local:9090")],
            vec![host, ("origin", "null")],
            vec![("origin", "http://vyrtel.local:8080")],
        ] {
            let e = same_origin(&user, &headers(&bad)).unwrap_err();
            assert_eq!(e.status, StatusCode::FORBIDDEN, "{bad:?}");
        }
        same_origin(&key, &headers(&[host, ("sec-fetch-site", "cross-site")])).unwrap();
    }
}
