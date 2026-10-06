//! Predicate evaluation.
//!
//! Comparison rules (documented in docs/query-language.md):
//! * A missing field (or JSON `null`) only satisfies `= null` and `!= <value>`.
//! * Strings compare exactly (case-sensitive); `contains` is case-insensitive.
//! * Numbers compare numerically; numeric strings coerce (`"123" = 123`).
//! * `"true"`/`"false"` strings compare equal to booleans.
//! * An array matches if any element matches.
//! * `level` understands level names and their order (`level >= Warning`).
//!
//! Equality is defined via `storage::index::keys` canonical forms so that
//! the Bloom filters built at write time can never disagree with it.

use std::cmp::Ordering;

use storage::index::keys::{CanonNum, bool_from_str, canonical_number, canonical_str_number};
use storage::segment::ColumnSet;
use telemetry::*;

use crate::ast::{Comparison, Expr, Literal, Op};
use crate::fields::{FieldRef, resolve};

/// A compiled comparison: field resolved once, literal pre-processed.
#[derive(Debug, Clone)]
pub struct Cmp {
    pub field: FieldRef,
    pub op: Op,
    pub lit: Literal,
    /// Lowercased literal text for `contains`.
    needle: String,
    level: Option<Level>,
    time: Option<Timestamp>,
    id_text: String,
    canon: Option<CanonNum>,
}

#[derive(Debug, Clone)]
pub enum CExpr {
    And(Box<CExpr>, Box<CExpr>),
    Or(Box<CExpr>, Box<CExpr>),
    Not(Box<CExpr>),
    Cmp(Cmp),
}

pub fn compile(e: &Expr, signal: Signal) -> CExpr {
    match e {
        Expr::And(a, b) => CExpr::And(Box::new(compile(a, signal)), Box::new(compile(b, signal))),
        Expr::Or(a, b) => CExpr::Or(Box::new(compile(a, signal)), Box::new(compile(b, signal))),
        Expr::Not(a) => CExpr::Not(Box::new(compile(a, signal))),
        Expr::Comparison(c) => CExpr::Cmp(compile_cmp(c, signal)),
    }
}

pub fn compile_cmp(c: &Comparison, signal: Signal) -> Cmp {
    let field = resolve(&c.field, signal);
    let text = c.value.as_text();
    Cmp {
        needle: text.to_lowercase(),
        level: match &c.value {
            Literal::String(s) => Level::parse(s),
            _ => None,
        },
        time: match &c.value {
            Literal::String(s) => Timestamp::parse_rfc3339(s),
            Literal::Int(i) => Timestamp::from_unix_number(*i as f64),
            Literal::Float(f) => Timestamp::from_unix_number(*f),
            _ => None,
        },
        id_text: normalize_id(text.trim()),
        canon: canonical_number(&c.value.to_value()),
        field,
        op: c.op,
        lit: c.value.clone(),
    }
}

impl CExpr {
    /// Columns needed to evaluate the whole expression.
    pub fn columns(&self) -> ColumnSet {
        match self {
            CExpr::And(a, b) | CExpr::Or(a, b) => a.columns().union(b.columns()),
            CExpr::Not(a) => a.columns(),
            CExpr::Cmp(c) => c.field.columns(),
        }
    }

    pub fn needs_body(&self) -> bool {
        self.columns().contains(storage::segment::Column::Body)
    }
}

/// The fields of an event the evaluator can see. Implemented by full
/// events and by lightweight rows decoded from segment columns.
pub trait Row {
    fn timestamp(&self) -> Timestamp;
    fn level(&self) -> Option<Level>;
    fn service(&self) -> Option<&str>;
    fn environment(&self) -> Option<&str>;
    fn message(&self) -> &str;
    fn trace_id(&self) -> Option<&str>;
    fn span_id(&self) -> Option<&str>;
    /// Full event, when body columns were decoded.
    fn event(&self) -> Option<&TelemetryEvent>;
}

impl Row for TelemetryEvent {
    fn timestamp(&self) -> Timestamp {
        self.timestamp
    }
    fn level(&self) -> Option<Level> {
        TelemetryEvent::level(self)
    }
    fn service(&self) -> Option<&str> {
        self.service.as_deref()
    }
    fn environment(&self) -> Option<&str> {
        self.environment.as_deref()
    }
    fn message(&self) -> &str {
        TelemetryEvent::message(self)
    }
    fn trace_id(&self) -> Option<&str> {
        self.trace_id.as_ref().map(TraceId::as_str)
    }
    fn span_id(&self) -> Option<&str> {
        self.span_id.as_ref().map(SpanId::as_str)
    }
    fn event(&self) -> Option<&TelemetryEvent> {
        Some(self)
    }
}

pub fn eval(e: &CExpr, row: &dyn Row) -> bool {
    match e {
        CExpr::And(a, b) => eval(a, row) && eval(b, row),
        CExpr::Or(a, b) => eval(a, row) || eval(b, row),
        CExpr::Not(a) => !eval(a, row),
        CExpr::Cmp(c) => eval_cmp(c, row),
    }
}

pub fn eval_opt(e: Option<&CExpr>, row: &dyn Row) -> bool {
    e.is_none_or(|e| eval(e, row))
}

/// Borrowed scalar view used by comparisons.
#[derive(Clone, Copy)]
enum V<'a> {
    Missing,
    Str(&'a str),
    Int(i64),
    Float(f64),
    Bool(bool),
    Arr(&'a [Value]),
    Obj,
}

impl<'a> V<'a> {
    fn of(v: Option<&'a Value>) -> V<'a> {
        match v {
            None | Some(Value::Null) => V::Missing,
            Some(Value::String(s)) => V::Str(s),
            Some(Value::Int(i)) => V::Int(*i),
            Some(Value::Float(f)) => V::Float(*f),
            Some(Value::Bool(b)) => V::Bool(*b),
            Some(Value::Array(a)) => V::Arr(a),
            Some(Value::Object(_)) => V::Obj,
        }
    }

    fn text(s: Option<&'a str>) -> V<'a> {
        s.map_or(V::Missing, V::Str)
    }

    fn canon(self) -> Option<CanonNum> {
        match self {
            V::Str(s) => canonical_str_number(s),
            V::Int(i) => Some(CanonNum::Int(i)),
            V::Float(f) => storage::index::keys::canonical_f64(f),
            _ => None,
        }
    }

    fn as_f64(self) -> Option<f64> {
        match self {
            V::Str(s) => parse_number(s),
            V::Int(i) => Some(i as f64),
            V::Float(f) if f.is_finite() => Some(f),
            _ => None,
        }
    }
}

fn eval_cmp(c: &Cmp, row: &dyn Row) -> bool {
    match &c.field {
        FieldRef::Timestamp => cmp_time(row.timestamp(), c),
        FieldRef::Level => cmp_level(row.level(), c),
        FieldRef::Service => cmp_v(V::text(row.service()), c),
        FieldRef::Environment => cmp_v(V::text(row.environment()), c),
        FieldRef::Message => cmp_v(V::Str(row.message()), c),
        FieldRef::TraceId => cmp_id(row.trace_id(), c),
        FieldRef::SpanId => cmp_id(row.span_id(), c),
        other => {
            let Some(e) = row.event() else {
                debug_assert!(false, "body field evaluated without the event body");
                return false;
            };
            eval_body_field(other, e, c)
        }
    }
}

fn eval_body_field(field: &FieldRef, e: &TelemetryEvent, c: &Cmp) -> bool {
    match field {
        FieldRef::MessageTemplate => cmp_v(V::text(e.as_log().and_then(|l| l.message_template.as_deref())), c),
        FieldRef::ExceptionType => cmp_v(V::text(exception(e).and_then(|x| x.kind.as_deref())), c),
        FieldRef::ExceptionMessage => cmp_v(V::text(exception(e).and_then(|x| x.message.as_deref())), c),
        FieldRef::StackTrace => cmp_v(V::text(exception(e).and_then(|x| x.stack_trace.as_deref())), c),
        FieldRef::ParentSpanId => cmp_id(e.as_span().and_then(|s| s.parent_span_id.as_ref()).map(SpanId::as_str), c),
        FieldRef::DurationMs => match e.as_span() {
            Some(s) => cmp_v(V::Float(s.duration_ms()), c),
            None => cmp_v(V::Missing, c),
        },
        FieldRef::Status => cmp_v(V::text(e.as_span().map(|s| s.status.code.as_str())), c),
        FieldRef::SpanKind => cmp_v(V::text(e.as_span().map(|s| s.kind.as_str())), c),
        FieldRef::MetricValue => match e.as_metric().map(|m| &m.value) {
            Some(MetricValue::Number(v)) => cmp_v(V::Float(*v), c),
            _ => cmp_v(V::Missing, c),
        },
        FieldRef::Unit => cmp_v(V::text(e.as_metric().and_then(|m| m.unit.as_deref())), c),
        FieldRef::MetricKind => cmp_v(V::text(e.as_metric().map(|m| m.kind.as_str())), c),
        FieldRef::Attr(p) => cmp_v(V::of(e.attributes.lookup_path(p).or_else(|| e.resource.lookup_path(p))), c),
        FieldRef::AttrOnly(p) => cmp_v(V::of(e.attributes.lookup_path(p)), c),
        FieldRef::Resource(p) => cmp_v(V::of(e.resource.lookup_path(p)), c),
        // Handled in eval_cmp.
        FieldRef::Timestamp
        | FieldRef::Level
        | FieldRef::Service
        | FieldRef::Environment
        | FieldRef::Message
        | FieldRef::TraceId
        | FieldRef::SpanId => false,
    }
}

fn exception(e: &TelemetryEvent) -> Option<&Exception> {
    e.as_log().and_then(|l| l.exception.as_ref())
}

/// Semantics for a missing / null value.
fn missing(c: &Cmp) -> bool {
    match c.op {
        Op::Eq => c.lit == Literal::Null,
        Op::Ne => c.lit != Literal::Null,
        _ => false,
    }
}

fn cmp_v(v: V, c: &Cmp) -> bool {
    match v {
        V::Missing => missing(c),
        _ => match c.op {
            Op::Eq => v_eq(v, c),
            Op::Ne => c.lit == Literal::Null || !v_eq(v, c),
            Op::Contains => v_contains(v, &c.needle),
            op => v_order(v, c, op),
        },
    }
}

fn v_eq(v: V, c: &Cmp) -> bool {
    if let V::Arr(items) = v {
        return items.iter().any(|i| v_eq(V::of(Some(i)), c));
    }
    match &c.lit {
        Literal::String(t) => match v {
            V::Str(s) => s == t,
            V::Int(_) | V::Float(_) => v.canon().is_some() && v.canon() == c.canon,
            V::Bool(b) => bool_from_str(t) == Some(b),
            _ => false,
        },
        Literal::Int(_) | Literal::Float(_) => c.canon.is_some() && v.canon() == c.canon,
        Literal::Bool(b) => match v {
            V::Bool(x) => x == *b,
            V::Str(s) => bool_from_str(s) == Some(*b),
            _ => false,
        },
        Literal::Null => false,
    }
}

fn v_order(v: V, c: &Cmp, op: Op) -> bool {
    if let V::Arr(items) = v {
        return items.iter().any(|i| v_order(V::of(Some(i)), c, op));
    }
    let ord = match &c.lit {
        Literal::Int(_) | Literal::Float(_) => {
            let (Some(a), Some(b)) = (v.as_f64(), c.lit.as_f64()) else {
                return false;
            };
            a.partial_cmp(&b)
        }
        Literal::String(t) => match (parse_number(t), v.as_f64(), v) {
            (Some(b), Some(a), _) => a.partial_cmp(&b),
            (_, _, V::Str(s)) => Some(s.cmp(t.as_str())),
            _ => None,
        },
        _ => None,
    };
    ord.is_some_and(|o| check(o, op))
}

fn check(o: Ordering, op: Op) -> bool {
    match op {
        Op::Gt => o == Ordering::Greater,
        Op::Ge => o != Ordering::Less,
        Op::Lt => o == Ordering::Less,
        Op::Le => o != Ordering::Greater,
        Op::Eq => o == Ordering::Equal,
        Op::Ne => o != Ordering::Equal,
        Op::Contains => false,
    }
}

fn v_contains(v: V, needle: &str) -> bool {
    match v {
        V::Str(s) => contains_ci(s, needle),
        V::Int(i) => i.to_string().contains(needle),
        V::Float(f) => format_float(f).contains(needle),
        V::Bool(b) => b.to_string().contains(needle),
        V::Arr(items) => items.iter().any(|i| v_contains(V::of(Some(i)), needle)),
        V::Missing | V::Obj => false,
    }
}

/// Case-insensitive substring search; `needle` is already lowercase.
pub fn contains_ci(hay: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    if hay.is_ascii() && needle.is_ascii() {
        let h = hay.as_bytes();
        let n = needle.as_bytes();
        if n.len() > h.len() {
            return false;
        }
        return h.windows(n.len()).any(|w| w.iter().zip(n).all(|(a, b)| a.to_ascii_lowercase() == *b));
    }
    hay.to_lowercase().contains(needle)
}

fn cmp_level(l: Option<Level>, c: &Cmp) -> bool {
    let Some(l) = l else {
        return missing(c);
    };
    if c.lit == Literal::Null {
        return c.op == Op::Ne;
    }
    match c.op {
        Op::Contains => contains_ci(l.as_str(), &c.needle),
        Op::Eq => c.level == Some(l),
        Op::Ne => c.level != Some(l),
        op => c.level.is_some_and(|x| check(l.cmp(&x), op)),
    }
}

fn cmp_time(t: Timestamp, c: &Cmp) -> bool {
    match (c.op, c.time) {
        (Op::Contains, _) => contains_ci(&t.to_rfc3339(), &c.needle),
        (Op::Ne, None) => true,
        (_, None) => false,
        (op, Some(x)) => check(t.cmp(&x), op),
    }
}

fn cmp_id(id: Option<&str>, c: &Cmp) -> bool {
    let Some(id) = id else {
        return missing(c);
    };
    if c.lit == Literal::Null {
        return c.op == Op::Ne;
    }
    match c.op {
        Op::Eq => id == c.id_text,
        Op::Ne => id != c.id_text,
        Op::Contains => contains_ci(id, &c.needle),
        op => check(id.cmp(c.id_text.as_str()), op),
    }
}

/// Literal comparison against a level code (0 = no level). Used by the
/// planner to evaluate predicates against bitmap index entries with exactly
/// the evaluator's semantics.
pub fn level_matches(code: u8, c: &Cmp) -> bool {
    cmp_level(Level::from_code(code), c)
}

/// Literal comparison against a dictionary string (service/environment).
pub fn text_matches(s: Option<&str>, c: &Cmp) -> bool {
    cmp_v(V::text(s), c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    fn event() -> TelemetryEvent {
        let attrs: Value = serde_json::from_str(
            r#"{
                "customerId": 481,
                "paymentProvider": "stripe",
                "amount": 71.50,
                "code": "123",
                "flag": "TRUE",
                "tags": ["blue", "green"],
                "http": {"statusCode": 500},
                "nothing": null
            }"#,
        )
        .unwrap();
        let res: Value = serde_json::from_str(r#"{"host.name": "web-1", "customerId": 999}"#).unwrap();
        TelemetryEvent {
            id: EventId(1),
            timestamp: Timestamp::parse_rfc3339("2026-10-06T10:20:13.485Z").unwrap(),
            observed_timestamp: Timestamp(0),
            service: Some("payments".into()),
            environment: Some("production".into()),
            trace_id: TraceId::new("ABC123"),
            span_id: None,
            resource: res.as_object().unwrap().clone(),
            attributes: attrs.as_object().unwrap().clone(),
            payload: TelemetryPayload::Log(LogEvent {
                level: Level::Error,
                message: "Payment provider timed out".into(),
                message_template: Some("Payment provider {Provider} timed out".into()),
                exception: Some(Exception {
                    kind: Some("TimeoutException".into()),
                    message: Some("Provider timed out".into()),
                    stack_trace: Some("at Pay()".into()),
                }),
            }),
        }
    }

    fn m(q: &str) -> bool {
        let e = parse(q).unwrap().unwrap();
        eval(&compile(&e, Signal::Logs), &event())
    }

    #[test]
    fn spec_examples() {
        assert!(m(r#"level = "Error""#));
        assert!(m(r#"service = "payments""#));
        assert!(m(r#"level = "Error" and service = "payments""#));
        assert!(!m(r#"message contains "timeout""#));
        assert!(m(r#"message contains "timed out""#));
        assert!(m("customerId = 481"));
        assert!(m(r#"traceId = "abc123""#));
        assert!(m("http.statusCode = 500"));
        assert!(!m("amount > 500"));
        assert!(m("amount > 70"));
    }

    #[test]
    fn level_semantics() {
        assert!(m("level = error"));
        assert!(m("level = ERR"));
        assert!(m("level >= Warning"));
        assert!(!m("level < Warning"));
        assert!(m("level != Information"));
        assert!(!m("level = bogus"));
        assert!(m("level != bogus"));
        assert!(m("level contains err"));
        assert!(!m("level = null"));
        assert!(m("level != null"));
    }

    #[test]
    fn coercion_rules() {
        assert!(m("code = 123"));
        assert!(m(r#"customerId = "481""#));
        assert!(m("customerId = 481.0"));
        assert!(m("code > 100"));
        assert!(m("flag = true"));
        assert!(!m("flag = false"));
        assert!(m(r#"paymentProvider = "stripe""#));
        assert!(!m(r#"paymentProvider = "Stripe""#));
        assert!(m(r#"paymentProvider contains "STR""#));
        assert!(!m("paymentProvider > 5"));
        assert!(m(r#"paymentProvider > "a""#));
    }

    #[test]
    fn missing_and_null() {
        assert!(m("missing = null"));
        assert!(m("nothing = null"));
        assert!(!m("missing = 1"));
        assert!(m("missing != 1"));
        assert!(!m("missing != null"));
        assert!(!m("missing > 1"));
        assert!(!m(r#"missing contains "x""#));
        assert!(m("customerId != null"));
        assert!(!m("customerId = null"));
    }

    #[test]
    fn arrays_match_any_element() {
        assert!(m(r#"tags = "green""#));
        assert!(!m(r#"tags = "red""#));
        assert!(!m(r#"tags != "green""#));
        assert!(m(r#"tags contains "BLU""#));
    }

    #[test]
    fn resource_fallback_and_prefixes() {
        assert!(m(r#"host.name = "web-1""#));
        // Attributes win over resource for the same key.
        assert!(m("customerId = 481"));
        assert!(m("resource.customerId = 999"));
        assert!(m("attributes.customerId = 481"));
        assert!(!m("attributes.customerId = 999"));
    }

    #[test]
    fn boolean_operators() {
        assert!(m(r#"not level = Information"#));
        assert!(m(r#"level = Debug or service = "payments""#));
        assert!(!m(r#"level = Debug and service = "payments""#));
        assert!(m(r#"(level = Debug or level = Error) and not service = "orders""#));
    }

    #[test]
    fn exception_and_template_fields() {
        assert!(m(r#"exception.type = "TimeoutException""#));
        assert!(m(r#"exception.message contains "timed""#));
        assert!(m(r#"stackTrace contains "pay()""#));
        assert!(m(r#"messageTemplate = "Payment provider {Provider} timed out""#));
    }

    #[test]
    fn timestamps() {
        assert!(m(r#"timestamp > "2026-10-06T10:00:00Z""#));
        assert!(!m(r#"timestamp < "2026-10-06T10:00:00Z""#));
        assert!(m("timestamp >= 1791282013"));
        assert!(!m(r#"timestamp = "garbage""#));
    }

    #[test]
    fn free_text_search() {
        assert!(m(r#""provider timed""#));
        assert!(!m(r#""database""#));
    }

    #[test]
    fn contains_ci_unicode_and_ascii() {
        assert!(contains_ci("Hello World", "world"));
        assert!(contains_ci("ÉCOLE", "école"));
        assert!(!contains_ci("abc", "abcd"));
        assert!(contains_ci("abc", ""));
    }
}
