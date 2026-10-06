use axum::extract::{Path, State};

use axum::routing::{delete, get};

use axum::{Extension, Json, Router};

use metadata::{Account, ApiKeyInfo, CreatedApiKey};

use serde::Deserialize;



use crate::api::auth_mw::AuthUser;

use crate::api::errors::ApiError;

use crate::state::AppState;



pub fn router() -> Router<AppState> {

    Router::new()

        .route("/api/accounts", get(list_accounts).post(create_account))

        .route("/api/accounts/api-keys", get(list_keys).post(create_key))

        .route("/api/accounts/api-keys/{id}", delete(delete_key))

}



#[derive(Deserialize)]

#[serde(rename_all = "camelCase")]

struct CreateAccountBody {

    username: String,

    password: String,

    #[serde(default)]

    display_name: Option<String>,

}



#[derive(Deserialize)]

#[serde(rename_all = "camelCase")]

struct CreateKeyBody {

    name: String,

}



async fn list_accounts(

    State(state): State<AppState>,

    Extension(_user): Extension<AuthUser>,

) -> Result<Json<Vec<Account>>, ApiError> {

    state

        .metadata

        .list_accounts()

        .await

        .map(Json)

        .map_err(|e| {

            ApiError::new(

                axum::http::StatusCode::INTERNAL_SERVER_ERROR,

                "db_error",

                e.to_string(),

            )

        })

}



async fn create_account(

    State(state): State<AppState>,

    Extension(_user): Extension<AuthUser>,

    Json(body): Json<CreateAccountBody>,

) -> Result<Json<Account>, ApiError> {

    let username = body.username.trim();

    if username.is_empty() || body.password.is_empty() {

        return Err(ApiError::new(

            axum::http::StatusCode::BAD_REQUEST,

            "invalid_request",

            "Username and password are required",

        ));

    }

    if body.password.len() < 4 {

        return Err(ApiError::new(

            axum::http::StatusCode::BAD_REQUEST,

            "invalid_request",

            "Password must be at least 4 characters",

        ));

    }

    let display = body

        .display_name

        .as_deref()

        .map(str::trim)

        .filter(|s| !s.is_empty())

        .unwrap_or(username);



    if state

        .metadata

        .find_account_by_username(username)

        .await

        .map_err(|e| {

            ApiError::new(

                axum::http::StatusCode::INTERNAL_SERVER_ERROR,

                "db_error",

                e.to_string(),

            )

        })?

        .is_some()

    {

        return Err(ApiError::new(

            axum::http::StatusCode::CONFLICT,

            "conflict",

            "Username already exists",

        ));

    }



    state

        .metadata

        .create_account(username, &body.password, display)

        .await

        .map(Json)

        .map_err(|e| {

            ApiError::new(

                axum::http::StatusCode::INTERNAL_SERVER_ERROR,

                "db_error",

                e.to_string(),

            )

        })

}



async fn list_keys(

    State(state): State<AppState>,

    Extension(_user): Extension<AuthUser>,

) -> Result<Json<Vec<ApiKeyInfo>>, ApiError> {

    state

        .metadata

        .list_all_api_keys()

        .await

        .map(Json)

        .map_err(|e| {

            ApiError::new(

                axum::http::StatusCode::INTERNAL_SERVER_ERROR,

                "db_error",

                e.to_string(),

            )

        })

}



async fn create_key(

    State(state): State<AppState>,

    Extension(user): Extension<AuthUser>,

    Json(body): Json<CreateKeyBody>,

) -> Result<Json<CreatedApiKey>, ApiError> {

    let name = body.name.trim();

    if name.is_empty() {

        return Err(ApiError::new(

            axum::http::StatusCode::BAD_REQUEST,

            "invalid_request",

            "Name is required",

        ));

    }

    state

        .metadata

        .create_api_key(&user.0.id, name)

        .await

        .map(Json)

        .map_err(|e| {

            ApiError::new(

                axum::http::StatusCode::INTERNAL_SERVER_ERROR,

                "db_error",

                e.to_string(),

            )

        })

}



async fn delete_key(

    State(state): State<AppState>,

    Extension(_user): Extension<AuthUser>,

    Path(id): Path<String>,

) -> Result<axum::http::StatusCode, ApiError> {

    state.metadata.delete_api_key(&id).await.map_err(|e| {

        ApiError::new(

            axum::http::StatusCode::INTERNAL_SERVER_ERROR,

            "db_error",

            e.to_string(),

        )

    })?;

    Ok(axum::http::StatusCode::NO_CONTENT)

}

