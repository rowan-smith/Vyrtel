//! Canonical index keys.
//!
//! Bloom filters only work if the writer and the query planner hash exactly
//! the same bytes for values that the query evaluator considers equal. This
//! module is the single definition of that equivalence. The query crate's
//! equality semantics (see `query::eval`) are written in terms of
//! [`canonical_number`] and [`bool_from_str`] so the two cannot drift apart.
//!
//! Equality rules (also in docs/query-language.md):
//! * string = string: exact, case-sensitive
//! * number = number: numeric (`1` = `1.0`)
//! * string = number: the string is parsed as a number
//! * bool = bool; strings "true"/"false" (any case) compare as booleans
//!
//! Writers insert, for each stored value, every key under which a literal
//! could match it. The planner probes with every key a literal could match
//! under; a block may contain a match iff any probe key is present.

use telemetry::Value;
use xxhash_rust::xxh3::Xxh3;

/// A number in canonical form: integral values are integers no matter
/// whether they arrived as `1`, `1.0` or `"1"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CanonNum {
    Int(i64),
    /// Bit pattern of a finite, non-integral f64.
    Float(u64),
}

pub fn canonical_f64(f: f64) -> Option<CanonNum> {
    if !f.is_finite() {
        return None;
    }
    // i64::MAX as f64 rounds up to 2^63, so use a strict bound.
    if f.fract() == 0.0 && (-9.223_372_036_854_776e18..9.223_372_036_854_776e18).contains(&f) {
        Some(CanonNum::Int(f as i64))
    } else {
        Some(CanonNum::Float(f.to_bits()))
    }
}

pub fn canonical_str_number(s: &str) -> Option<CanonNum> {
    let t = s.trim();
    // Integers first so large values (> 2^53) keep full precision.
    if let Ok(i) = t.parse::<i64>() {
        return Some(CanonNum::Int(i));
    }
    telemetry::parse_number(t).and_then(canonical_f64)
}

/// Canonical numeric form of a stored or literal value, if it has one.
pub fn canonical_number(v: &Value) -> Option<CanonNum> {
    match v {
        Value::Int(i) => Some(CanonNum::Int(*i)),
        Value::Float(f) => canonical_f64(*f),
        Value::String(s) => canonical_str_number(s),
        _ => None,
    }
}

pub fn bool_from_str(s: &str) -> Option<bool> {
    let t = s.trim();
    if t.eq_ignore_ascii_case("true") {
        Some(true)
    } else if t.eq_ignore_ascii_case("false") {
        Some(false)
    } else {
        None
    }
}

fn hash(path: &str, tag: u8, bytes: &[u8]) -> u64 {
    let mut h = Xxh3::new();
    h.update(path.as_bytes());
    // Separator byte that cannot appear in UTF-8, so ("a", "bc") and
    // ("ab", "c") never collide by construction.
    h.update(&[0xff, tag]);
    h.update(bytes);
    h.digest()
}

pub fn string_key(path: &str, s: &str) -> u64 {
    hash(path, b's', s.as_bytes())
}

pub fn number_key(path: &str, n: CanonNum) -> u64 {
    match n {
        CanonNum::Int(i) => hash(path, b'i', &i.to_le_bytes()),
        CanonNum::Float(bits) => hash(path, b'f', &bits.to_le_bytes()),
    }
}

pub fn bool_key(path: &str, b: bool) -> u64 {
    hash(path, b'b', &[b as u8])
}

/// Keys to insert for a stored value at `path`.
pub fn stored_keys(path: &str, v: &Value, out: &mut dyn FnMut(u64)) {
    match v {
        Value::String(s) => {
            out(string_key(path, s));
            if let Some(n) = canonical_str_number(s) {
                out(number_key(path, n));
            }
            if let Some(b) = bool_from_str(s) {
                out(bool_key(path, b));
            }
        }
        Value::Int(_) | Value::Float(_) => {
            if let Some(n) = canonical_number(v) {
                out(number_key(path, n));
            }
        }
        Value::Bool(b) => out(bool_key(path, *b)),
        // Null, arrays and objects are never equality-matched directly
        // (arrays are flattened into their elements by the caller).
        _ => {}
    }
}

/// Keys to probe for an equality literal. Returns `None` when the literal
/// cannot be answered from the Bloom filter (e.g. `null`).
pub fn probe_keys(path: &str, literal: &Value) -> Option<Vec<u64>> {
    let mut keys = Vec::with_capacity(3);
    match literal {
        Value::String(s) => {
            keys.push(string_key(path, s));
            if let Some(n) = canonical_str_number(s) {
                keys.push(number_key(path, n));
            }
            if let Some(b) = bool_from_str(s) {
                keys.push(bool_key(path, b));
            }
        }
        Value::Int(_) | Value::Float(_) => keys.push(number_key(path, canonical_number(literal)?)),
        Value::Bool(b) => keys.push(bool_key(path, *b)),
        _ => return None,
    }
    Some(keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(path: &str, v: &Value) -> Vec<u64> {
        let mut out = Vec::new();
        stored_keys(path, v, &mut |h| out.push(h));
        out
    }

    fn matches(stored_v: &Value, literal: &Value) -> bool {
        let s = stored("f", stored_v);
        probe_keys("f", literal).unwrap().iter().any(|k| s.contains(k))
    }

    #[test]
    fn numeric_equivalences_share_keys() {
        assert!(matches(&Value::Int(123), &Value::Int(123)));
        assert!(matches(&Value::Float(123.0), &Value::Int(123)));
        assert!(matches(&Value::from("123"), &Value::Int(123)));
        assert!(matches(&Value::Int(123), &Value::from("123")));
        assert!(matches(&Value::Float(1.5), &Value::from("1.5")));
        assert!(!matches(&Value::Int(124), &Value::Int(123)));
    }

    #[test]
    fn bool_equivalences_share_keys() {
        assert!(matches(&Value::Bool(true), &Value::Bool(true)));
        assert!(matches(&Value::from("TRUE"), &Value::Bool(true)));
        assert!(matches(&Value::Bool(false), &Value::from("false")));
        assert!(!matches(&Value::Bool(false), &Value::Bool(true)));
    }

    #[test]
    fn strings_are_case_sensitive_and_path_scoped() {
        assert!(matches(&Value::from("abc"), &Value::from("abc")));
        assert!(!matches(&Value::from("ABC"), &Value::from("abc")));
        assert_ne!(string_key("a", "bc"), string_key("ab", "c"));
    }

    #[test]
    fn big_integers_keep_precision() {
        let big = 9_007_199_254_740_993i64; // 2^53 + 1
        assert_eq!(canonical_str_number(&big.to_string()), Some(CanonNum::Int(big)));
        assert_ne!(canonical_str_number(&big.to_string()), canonical_str_number(&(big - 1).to_string()));
    }

    #[test]
    fn null_is_not_probeable() {
        assert!(probe_keys("f", &Value::Null).is_none());
        assert_eq!(canonical_f64(f64::NAN), None);
        assert_eq!(canonical_f64(-0.0), Some(CanonNum::Int(0)));
    }
}
