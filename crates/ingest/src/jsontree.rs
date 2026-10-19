//! `serde_json::Value` trees built under a [`Budget`].
//!
//! The same values `serde_json::from_slice` builds, but every allocation is
//! charged before it is made: strings by length, arrays as they grow (see
//! [`limits::push`]), objects as an entry list that becomes the map once
//! complete. Nesting deeper than the context allows is refused while
//! parsing, never after.

use std::fmt;

use serde::de::{DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value as Json};

use crate::IngestError;
use crate::limits::{self, Budget};

/// One value, held inline by its parent.
pub(crate) const NODE: usize = size_of::<Json>();
/// One object entry while it is being read.
const PAIR: usize = size_of::<(String, Json)>();
/// One object entry in the finished map (indexmap: hash, key, value).
const ENTRY: usize = size_of::<usize>() + size_of::<String>() + NODE;

/// Heap of an object's hash index for `n` entries: hashbrown's power-of-two
/// buckets at 7/8 load, a `usize` and a control byte each, plus one group
/// of trailing control bytes.
fn index_bytes(n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    let buckets = if n < 4 {
        4
    } else if n < 8 {
        8
    } else {
        (n.saturating_mul(8) / 7).next_power_of_two()
    };
    buckets * (size_of::<usize>() + 1) + 16
}

pub(crate) struct Ctx<'b> {
    pub budget: &'b mut Budget,
    /// Containers deeper than this are refused (the outermost value is at
    /// depth 0).
    pub max_depth: usize,
    /// Bytes charged for arrays and objects (not strings) since the last
    /// reset: what converting the tree into another representation will
    /// allocate again while the tree is still held.
    pub containers: usize,
}

impl<'b> Ctx<'b> {
    pub fn new(budget: &'b mut Budget, max_depth: usize) -> Self {
        Self { budget, max_depth, containers: 0 }
    }

    fn charge_container<E: serde::de::Error>(&mut self, bytes: usize) -> Result<(), E> {
        self.containers += bytes;
        self.budget.charge(bytes).map_err(|e| self.budget.fail(e))
    }

    /// Parse one value from `de`, at nesting `depth`.
    pub fn value<'de, D: Deserializer<'de>>(&mut self, de: D, depth: usize) -> Result<Json, D::Error> {
        self.seed(depth).deserialize(de)
    }

    /// A seed for one value at nesting `depth`, for `next_element_seed`.
    pub fn seed(&mut self, depth: usize) -> Seed<'_, 'b> {
        Seed { ctx: self, depth }
    }
}

/// Nesting depth of the deepest container in `v`, the outermost at 0;
/// `None` for a scalar.
pub(crate) fn depth(v: &Json) -> Option<usize> {
    let inner = match v {
        Json::Array(a) => a.iter().filter_map(depth).max(),
        Json::Object(o) => o.values().filter_map(depth).max(),
        _ => return None,
    };
    Some(inner.map_or(0, |d| d + 1))
}

/// Parse a complete JSON document.
pub(crate) fn from_slice(body: &[u8], ctx: &mut Ctx) -> Result<Json, IngestError> {
    let mut de = serde_json::Deserializer::from_slice(body);
    let parsed = ctx.value(&mut de, 0).and_then(|v| de.end().map(|()| v));
    parsed.map_err(|e| match ctx.budget.take_failure() {
        Some(limit) => limit,
        None => syntax_error(&e, ctx.budget),
    })
}

/// A parse error that no limit of ours caused. serde_json refuses more than
/// 128 levels of nesting on its own; that is reported as the depth limit.
pub(crate) fn syntax_error(e: &serde_json::Error, budget: &Budget) -> IngestError {
    if e.to_string().starts_with("recursion limit exceeded") {
        IngestError::TooDeep { limit: budget.limits().max_depth }
    } else {
        IngestError::Malformed(format!("invalid JSON: {e}"))
    }
}

pub(crate) struct Seed<'c, 'b> {
    ctx: &'c mut Ctx<'b>,
    depth: usize,
}

impl<'de> DeserializeSeed<'de> for Seed<'_, '_> {
    type Value = Json;

    fn deserialize<D: Deserializer<'de>>(self, de: D) -> Result<Json, D::Error> {
        de.deserialize_any(self)
    }
}

impl Seed<'_, '_> {
    fn enter<E: serde::de::Error>(&mut self) -> Result<(), E> {
        if self.depth > self.ctx.max_depth {
            let limit = self.ctx.budget.limits().max_depth;
            return Err(self.ctx.budget.fail(IngestError::TooDeep { limit }));
        }
        Ok(())
    }

    fn string<E: serde::de::Error>(self, s: String) -> Result<Json, E> {
        self.ctx.budget.charge(s.capacity()).map_err(|e| self.ctx.budget.fail(e))?;
        Ok(Json::String(s))
    }
}

impl<'de> Visitor<'de> for Seed<'_, '_> {
    type Value = Json;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_bool<E>(self, v: bool) -> Result<Json, E> {
        Ok(Json::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Json, E> {
        Ok(Json::Number(v.into()))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Json, E> {
        Ok(Json::Number(v.into()))
    }

    fn visit_f64<E>(self, v: f64) -> Result<Json, E> {
        Ok(Number::from_f64(v).map_or(Json::Null, Json::Number))
    }

    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Json, E> {
        // Checked before the copy is made.
        self.ctx.budget.charge(v.len()).map_err(|e| self.ctx.budget.fail(e))?;
        Ok(Json::String(v.to_owned()))
    }

    fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Json, E> {
        self.string(v)
    }

    fn visit_unit<E>(self) -> Result<Json, E> {
        Ok(Json::Null)
    }

    fn visit_none<E>(self) -> Result<Json, E> {
        Ok(Json::Null)
    }

    fn visit_some<D: Deserializer<'de>>(self, de: D) -> Result<Json, D::Error> {
        self.deserialize(de)
    }

    fn visit_seq<A: SeqAccess<'de>>(mut self, mut seq: A) -> Result<Json, A::Error> {
        self.enter()?;
        let mut items: Vec<Json> = Vec::new();
        // Grown only once another element is known to exist, so `[]`
        // allocates nothing and a full array does not grow past its end.
        while let Some(v) = seq.next_element_seed(Seed { ctx: &mut *self.ctx, depth: self.depth + 1 })? {
            if items.len() == items.capacity() {
                let cap = limits::grown(items.capacity());
                self.ctx.charge_container((cap - items.capacity()) * NODE)?;
                items.reserve_exact(cap - items.len());
            }
            items.push(v);
        }
        Ok(Json::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(mut self, mut map: A) -> Result<Json, A::Error> {
        self.enter()?;
        let mut pairs: Vec<(String, Json)> = Vec::new();
        while let Some(key) = map.next_key::<String>()? {
            self.ctx.budget.charge(key.capacity()).map_err(|e| self.ctx.budget.fail(e))?;
            if pairs.len() == pairs.capacity() {
                let cap = limits::grown(pairs.capacity());
                // Transient: credited back once the map is built.
                self.ctx.budget.charge((cap - pairs.capacity()) * PAIR).map_err(|e| self.ctx.budget.fail(e))?;
                pairs.reserve_exact(cap - pairs.len());
            }
            let value = map.next_value_seed(Seed { ctx: &mut *self.ctx, depth: self.depth + 1 })?;
            pairs.push((key, value));
        }
        let n = pairs.len();
        self.ctx.charge_container(n * ENTRY + index_bytes(n))?;
        let scratch = pairs.capacity() * PAIR;
        let object: Map<String, Json> = pairs.into_iter().collect();
        self.ctx.budget.release(scratch);
        Ok(Json::Object(object))
    }
}

fn trim_start(b: &[u8]) -> &[u8] {
    let n = b.iter().take_while(|b| b.is_ascii_whitespace()).count();
    &b[n..]
}

/// Elements of a JSON array body, by a structural scan that skips strings
/// and allocates nothing. Nothing is validated: on malformed input the
/// count may be wrong, and the parse that follows reports the error. `None`
/// when the body is not an array.
pub(crate) fn count_array_elements(body: &[u8]) -> Option<usize> {
    scan_array(trim_start(body)).map(|(n, _)| n)
}

/// The array of a `{"events": [...]}` body with nothing else in it, found
/// by the same kind of scan; `None` for any other body.
pub(crate) fn envelope(body: &[u8]) -> Option<&[u8]> {
    let rest = trim_start(trim_start(body).strip_prefix(b"{")?).strip_prefix(b"\"events\"")?;
    let array = trim_start(trim_start(rest).strip_prefix(b":")?);
    let (_, end) = scan_array(array)?;
    let tail = trim_start(&array[end..]).strip_prefix(b"}")?;
    trim_start(tail).is_empty().then_some(&array[..end])
}

/// The element count of the array `body` starts with, and the offset just
/// past its closing bracket (the end of `body` when it is unterminated).
fn scan_array(body: &[u8]) -> Option<(usize, usize)> {
    if body.first() != Some(&b'[') {
        return None;
    }
    let (mut depth, mut commas, mut any, mut end) = (0usize, 0usize, false, body.len());
    let (mut in_string, mut escaped) = (false, false);
    for (i, &b) in body.iter().enumerate() {
        if in_string {
            match (escaped, b) {
                (true, _) => escaped = false,
                (false, b'\\') => escaped = true,
                (false, b'"') => in_string = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => {
                in_string = true;
                any |= depth == 1;
            }
            b'[' | b'{' => {
                any |= depth == 1;
                depth += 1;
            }
            b']' | b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    end = i + 1;
                    break;
                }
            }
            b',' if depth == 1 => commas += 1,
            b if b.is_ascii_whitespace() => {}
            _ => any |= depth == 1,
        }
    }
    Some((if any { commas + 1 } else { 0 }, end))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::Limits;

    fn parse(body: &str, limits: Limits, max_depth: usize) -> (Result<Json, IngestError>, usize) {
        let mut budget = Budget::new(limits);
        let r = from_slice(body.as_bytes(), &mut Ctx::new(&mut budget, max_depth));
        (r, budget.peak())
    }

    #[test]
    fn builds_what_serde_json_builds() {
        for body in [
            r#"{"a":1,"b":[true,null,-2,3.5,1e300],"c":{"d":"eé","a":2},"a":3}"#,
            r#"[18446744073709551615, -9223372036854775808, "", {}, []]"#,
            r#""just a string""#,
            "null",
        ] {
            let expected: Json = serde_json::from_str(body).unwrap_or(Json::Null);
            let (got, _) = parse(body, Limits::UNBOUNDED, 64);
            assert_eq!(got.unwrap(), expected, "{body}");
        }
    }

    #[test]
    fn errors_are_reported_like_serde_json() {
        let (r, _) = parse("{\"a\":", Limits::UNBOUNDED, 64);
        assert!(matches!(r, Err(IngestError::Malformed(m)) if m.starts_with("invalid JSON")));
        let (r, _) = parse("[1] 2", Limits::UNBOUNDED, 64);
        assert!(matches!(r, Err(IngestError::Malformed(_))));
    }

    #[test]
    fn depth_is_refused_while_parsing() {
        let nested = |n: usize| format!("{}{}", "[".repeat(n), "]".repeat(n));
        // The outermost container is at depth 0.
        assert!(parse(&nested(33), Limits::UNBOUNDED, 32).0.is_ok());
        assert_eq!(parse(&nested(34), Limits::UNBOUNDED, 32).0, Err(IngestError::TooDeep { limit: 32 }));
    }

    // Arrays are grown only for elements that exist: nothing for `[]`, and
    // exactly 4, 8 or 16 slots for arrays of that length.
    #[test]
    fn arrays_are_charged_for_the_slots_they_use() {
        for (n, slots) in [(0, 0), (1, 4), (4, 4), (5, 8), (8, 8), (9, 16), (16, 16), (17, 32)] {
            let body = format!("[{}]", vec!["0"; n].join(","));
            let (r, peak) = parse(&body, Limits::UNBOUNDED, 64);
            assert_eq!(r.unwrap().as_array().unwrap().len(), n);
            assert_eq!(peak, slots * NODE, "[{n} elements]");
        }
        let (_, peak) = parse("[[],[],[],[]]", Limits::UNBOUNDED, 64);
        assert_eq!(peak, 4 * NODE, "empty inner arrays allocate nothing");
    }

    #[test]
    fn allocation_stops_at_the_byte_limit() {
        let limits = Limits { max_decoded_bytes: 4096, ..Limits::UNBOUNDED };
        let body = format!("[{}0]", "0,".repeat(10_000));
        let (r, peak) = parse(&body, limits, 64);
        assert_eq!(r, Err(IngestError::TooLarge { limit_bytes: 4096 }));
        // Refused before the growth that would have passed the limit.
        assert!(peak <= 4096 + 4096, "peak {peak}");
        let (r, _) = parse(&format!("{{\"k\":\"{}\"}}", "x".repeat(5000)), limits, 64);
        assert_eq!(r, Err(IngestError::TooLarge { limit_bytes: 4096 }));
    }

    #[test]
    fn counts_array_elements() {
        for (body, n) in [
            ("[]", Some(0)),
            (" [ ] ", Some(0)),
            ("[1]", Some(1)),
            (r#"[{"a":[1,2,{"b":","}]},"x,\"y",3]"#, Some(3)),
            ("{\"events\":[1,2]}", None),
            ("", None),
        ] {
            assert_eq!(count_array_elements(body.as_bytes()), n, "{body}");
        }
    }

    #[test]
    fn finds_the_envelope_array() {
        for (body, array) in [
            (r#"{"events":[{"a":"]"}]}"#, Some(r#"[{"a":"]"}]"#)),
            (" { \"events\" : [ ] } \n", Some("[ ]")),
            (r#"{"events":[1],"x":2}"#, None),
            (r#"{"x":2,"events":[1]}"#, None),
            (r#"{"events":{}}"#, None),
            (r#"{"events":[1]"#, None),
            ("[1]", None),
        ] {
            assert_eq!(envelope(body.as_bytes()), array.map(str::as_bytes), "{body}");
        }
    }
}
