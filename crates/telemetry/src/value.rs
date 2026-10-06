//! Dynamically typed structured values.
//!
//! Telemetry is messy: the same property can be an integer in one event and a
//! string in the next. `Value` keeps whatever type arrived; the query layer is
//! responsible for coercion. Object fields keep insertion order so events are
//! displayed the way the producer wrote them.

use serde::de::{Deserialize, Deserializer};
use serde::ser::{Serialize, SerializeMap, SerializeSeq, Serializer};

#[derive(Debug, Clone, PartialEq, Default)]
pub enum Value {
    #[default]
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Array(Vec<Value>),
    Object(Fields),
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&Fields> {
        match self {
            Value::Object(f) => Some(f),
            _ => None,
        }
    }

    /// Numeric view used by comparisons and zone maps. Strings holding a
    /// number (`"123"`, `" 4.5 "`) coerce; everything else does not.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) if f.is_finite() => Some(*f),
            Value::String(s) => parse_number(s),
            _ => None,
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        }
    }

    /// Rough heap footprint, used for memory accounting of in-memory buffers.
    pub fn approx_size(&self) -> usize {
        std::mem::size_of::<Value>()
            + match self {
                Value::String(s) => s.len(),
                Value::Array(a) => a.iter().map(Value::approx_size).sum(),
                Value::Object(f) => f.approx_size(),
                _ => 0,
            }
    }

    /// Render a scalar for display / grouping. Composite values render as JSON.
    pub fn to_display_string(&self) -> String {
        match self {
            Value::Null => "null".into(),
            Value::Bool(b) => b.to_string(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => format_float(*f),
            Value::String(s) => s.clone(),
            other => serde_json::to_string(other).unwrap_or_default(),
        }
    }
}

/// Parse a string as a number, accepting surrounding whitespace.
pub fn parse_number(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    // Reject things Rust parses but people would not consider numbers.
    if t.eq_ignore_ascii_case("nan") || t.to_ascii_lowercase().contains("inf") {
        return None;
    }
    t.parse::<f64>().ok().filter(|f| f.is_finite())
}

pub fn format_float(f: f64) -> String {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e15 { format!("{f:.1}") } else { f.to_string() }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::String(s.to_string())
    }
}
impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::String(s)
    }
}
impl From<i64> for Value {
    fn from(i: i64) -> Self {
        Value::Int(i)
    }
}
impl From<f64> for Value {
    fn from(f: f64) -> Self {
        Value::Float(f)
    }
}
impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}

impl From<serde_json::Value> for Value {
    fn from(v: serde_json::Value) -> Self {
        match v {
            serde_json::Value::Null => Value::Null,
            serde_json::Value::Bool(b) => Value::Bool(b),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Value::Int(i)
                } else {
                    // u64 above i64::MAX and real floats both land here.
                    Value::Float(n.as_f64().unwrap_or(0.0))
                }
            }
            serde_json::Value::String(s) => Value::String(s),
            serde_json::Value::Array(a) => Value::Array(a.into_iter().map(Value::from).collect()),
            serde_json::Value::Object(o) => {
                let mut f = Fields::with_capacity(o.len());
                for (k, v) in o {
                    f.push(k, Value::from(v));
                }
                Value::Object(f)
            }
        }
    }
}

impl Serialize for Value {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Value::Null => s.serialize_unit(),
            Value::Bool(b) => s.serialize_bool(*b),
            Value::Int(i) => s.serialize_i64(*i),
            Value::Float(f) => {
                if f.is_finite() {
                    s.serialize_f64(*f)
                } else {
                    s.serialize_unit()
                }
            }
            Value::String(v) => s.serialize_str(v),
            Value::Array(a) => {
                let mut seq = s.serialize_seq(Some(a.len()))?;
                for v in a {
                    seq.serialize_element(v)?;
                }
                seq.end()
            }
            Value::Object(f) => f.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for Value {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        serde_json::Value::deserialize(d).map(Value::from)
    }
}

/// Ordered key/value collection. Keys are unique when inserted through
/// [`Fields::insert`]; decoders use [`Fields::push`] to restore exactly what
/// was stored.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Fields(Vec<(String, Value)>);

impl Fields {
    pub fn new() -> Self {
        Self(Vec::new())
    }

    pub fn with_capacity(n: usize) -> Self {
        Self(Vec::with_capacity(n))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Insert or replace a key.
    pub fn insert(&mut self, key: impl Into<String>, value: Value) {
        let key = key.into();
        if let Some(slot) = self.0.iter_mut().find(|(k, _)| *k == key) {
            slot.1 = value;
        } else {
            self.0.push((key, value));
        }
    }

    /// Append without checking for duplicates.
    pub fn push(&mut self, key: impl Into<String>, value: Value) {
        self.0.push((key.into(), value));
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn remove(&mut self, key: &str) -> Option<Value> {
        let idx = self.0.iter().position(|(k, _)| k == key)?;
        Some(self.0.remove(idx).1)
    }

    pub fn into_vec(self) -> Vec<(String, Value)> {
        self.0
    }

    /// Resolve a dotted path such as `http.statusCode`.
    ///
    /// OpenTelemetry attributes are flat with dotted keys (`http.status_code`)
    /// while JSON producers nest objects (`{"http": {"statusCode": 500}}`).
    /// We first try the whole path as a literal key, then every split point
    /// into nested objects, so both shapes answer the same query.
    pub fn lookup_path(&self, path: &str) -> Option<&Value> {
        if let Some(v) = self.get(path) {
            return Some(v);
        }
        for (i, _) in path.match_indices('.') {
            let (head, rest) = (&path[..i], &path[i + 1..]);
            if let Some(Value::Object(inner)) = self.get(head)
                && let Some(v) = inner.lookup_path(rest)
            {
                return Some(v);
            }
        }
        None
    }

    /// Visit every scalar leaf with its dotted path. Array elements are
    /// visited under the array's path. This is the exact set of
    /// (path, value) pairs that [`Fields::lookup_path`] can return, which is
    /// what makes Bloom filters built from it free of false negatives.
    pub fn for_each_leaf(&self, f: &mut dyn FnMut(&str, &Value)) {
        let mut path = String::new();
        self.leaf_walk(&mut path, f, 0);
    }

    fn leaf_walk(&self, path: &mut String, f: &mut dyn FnMut(&str, &Value), depth: usize) {
        for (k, v) in &self.0 {
            let len = path.len();
            if !path.is_empty() {
                path.push('.');
            }
            path.push_str(k);
            visit_value(path, v, f, depth);
            path.truncate(len);
        }
    }

    pub fn approx_size(&self) -> usize {
        self.0.iter().map(|(k, v)| k.len() + 24 + v.approx_size()).sum()
    }
}

fn visit_value(path: &mut String, v: &Value, f: &mut dyn FnMut(&str, &Value), depth: usize) {
    // Depth guard: decoders already limit nesting, this is belt and braces.
    if depth > 64 {
        return;
    }
    match v {
        Value::Object(inner) => inner.leaf_walk(path, f, depth + 1),
        Value::Array(items) => {
            for item in items {
                visit_value(path, item, f, depth + 1);
            }
        }
        scalar => f(path, scalar),
    }
}

impl FromIterator<(String, Value)> for Fields {
    fn from_iter<I: IntoIterator<Item = (String, Value)>>(iter: I) -> Self {
        let mut f = Fields::new();
        for (k, v) in iter {
            f.insert(k, v);
        }
        f
    }
}

impl Serialize for Fields {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(self.0.len()))?;
        for (k, v) in &self.0 {
            map.serialize_entry(k, v)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Fields {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match Value::deserialize(d)? {
            Value::Object(f) => Ok(f),
            Value::Null => Ok(Fields::new()),
            other => Err(serde::de::Error::custom(format!("expected an object, found {}", other.type_name()))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nested() -> Fields {
        let json = serde_json::json!({
            "http": { "statusCode": 500, "method": "GET" },
            "db.system": "postgres",
            "tags": ["a", "b"],
            "a.b": { "c": 1 }
        });
        Value::from(json).as_object().unwrap().clone()
    }

    #[test]
    fn lookup_nested_and_dotted_keys() {
        let f = nested();
        assert_eq!(f.lookup_path("http.statusCode"), Some(&Value::Int(500)));
        assert_eq!(f.lookup_path("db.system"), Some(&Value::from("postgres")));
        assert_eq!(f.lookup_path("a.b.c"), Some(&Value::Int(1)));
        assert_eq!(f.lookup_path("http.missing"), None);
        assert_eq!(f.lookup_path("nope"), None);
    }

    #[test]
    fn leaves_cover_lookup_results() {
        let f = nested();
        let mut leaves = Vec::new();
        f.for_each_leaf(&mut |p, v| leaves.push((p.to_string(), v.clone())));
        assert!(leaves.contains(&("http.statusCode".into(), Value::Int(500))));
        assert!(leaves.contains(&("tags".into(), Value::from("a"))));
        assert!(leaves.contains(&("tags".into(), Value::from("b"))));
        assert!(leaves.contains(&("a.b.c".into(), Value::Int(1))));
    }

    #[test]
    fn json_round_trip_preserves_order() {
        let json = r#"{"z":1,"a":{"y":true,"b":null},"m":[1.5,"x"]}"#;
        let v: Value = serde_json::from_str(json).unwrap();
        assert_eq!(serde_json::to_string(&v).unwrap(), json);
    }

    #[test]
    fn numeric_coercion() {
        assert_eq!(Value::from("123").as_f64(), Some(123.0));
        assert_eq!(Value::from(" 4.5 ").as_f64(), Some(4.5));
        assert_eq!(Value::from("abc").as_f64(), None);
        assert_eq!(Value::from("NaN").as_f64(), None);
        assert_eq!(Value::from("inf").as_f64(), None);
        assert_eq!(Value::Bool(true).as_f64(), None);
        assert_eq!(Value::Float(f64::NAN).as_f64(), None);
    }

    #[test]
    fn insert_replaces() {
        let mut f = Fields::new();
        f.insert("a", Value::Int(1));
        f.insert("a", Value::Int(2));
        assert_eq!(f.len(), 1);
        assert_eq!(f.get("a"), Some(&Value::Int(2)));
    }

    #[test]
    fn big_unsigned_becomes_float() {
        let v: Value = serde_json::from_str("18446744073709551615").unwrap();
        assert!(matches!(v, Value::Float(_)));
    }
}
