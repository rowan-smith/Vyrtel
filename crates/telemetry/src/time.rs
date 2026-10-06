//! Timestamps are nanoseconds since the Unix epoch in an `i64`, which covers
//! the years 1677–2262. Values outside that range are clamped at ingest.

use std::fmt;

use chrono::{DateTime, SecondsFormat, Utc};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Timestamp(pub i64);

pub const NANOS_PER_MILLI: i64 = 1_000_000;
pub const NANOS_PER_SEC: i64 = 1_000_000_000;

impl Timestamp {
    pub const MIN: Timestamp = Timestamp(i64::MIN);
    pub const MAX: Timestamp = Timestamp(i64::MAX);

    pub fn now() -> Self {
        let now = Utc::now();
        Timestamp(now.timestamp_nanos_opt().unwrap_or(i64::MAX))
    }

    pub fn from_nanos(n: i64) -> Self {
        Timestamp(n)
    }

    pub fn from_millis(ms: i64) -> Self {
        Timestamp(ms.saturating_mul(NANOS_PER_MILLI))
    }

    pub fn from_secs(s: i64) -> Self {
        Timestamp(s.saturating_mul(NANOS_PER_SEC))
    }

    pub fn nanos(self) -> i64 {
        self.0
    }

    pub fn millis(self) -> i64 {
        self.0.div_euclid(NANOS_PER_MILLI)
    }

    pub fn saturating_add_nanos(self, n: i64) -> Self {
        Timestamp(self.0.saturating_add(n))
    }

    pub fn saturating_sub_nanos(self, n: i64) -> Self {
        Timestamp(self.0.saturating_sub(n))
    }

    /// Parse an RFC 3339 timestamp (`2026-10-06T10:20:13.485Z`).
    pub fn parse_rfc3339(s: &str) -> Option<Self> {
        let dt = DateTime::parse_from_rfc3339(s.trim()).ok()?;
        dt.timestamp_nanos_opt().map(Timestamp)
    }

    /// Interpret a bare number as a Unix timestamp, guessing the unit from
    /// its magnitude (seconds, milliseconds, microseconds or nanoseconds).
    /// Producers disagree wildly about this, so we accept all of them.
    pub fn from_unix_number(n: f64) -> Option<Self> {
        if !n.is_finite() {
            return None;
        }
        let abs = n.abs();
        let nanos = if abs >= 1e17 {
            n
        } else if abs >= 1e14 {
            n * 1e3
        } else if abs >= 1e11 {
            n * 1e6
        } else {
            n * 1e9
        };
        if nanos >= i64::MIN as f64 && nanos <= i64::MAX as f64 { Some(Timestamp(nanos as i64)) } else { None }
    }

    /// Integer variant of [`Timestamp::from_unix_number`] that keeps full
    /// precision (floats cannot represent nanosecond epochs exactly).
    pub fn from_unix_int(n: i64) -> Self {
        let abs = n.unsigned_abs();
        Timestamp(if abs >= 100_000_000_000_000_000 {
            n
        } else if abs >= 100_000_000_000_000 {
            n.saturating_mul(1_000)
        } else if abs >= 100_000_000_000 {
            n.saturating_mul(1_000_000)
        } else {
            n.saturating_mul(1_000_000_000)
        })
    }

    pub fn to_rfc3339(self) -> String {
        DateTime::<Utc>::from_timestamp_nanos(self.0).to_rfc3339_opts(SecondsFormat::AutoSi, true)
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_rfc3339())
    }
}

impl serde::Serialize for Timestamp {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_rfc3339())
    }
}

impl<'de> serde::Deserialize<'de> for Timestamp {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        parse_json_timestamp(&v).ok_or_else(|| serde::de::Error::custom("invalid timestamp"))
    }
}

/// Accept either an RFC 3339 string or a Unix number (any common unit).
pub fn parse_json_timestamp(v: &serde_json::Value) -> Option<Timestamp> {
    match v {
        serde_json::Value::String(s) => Timestamp::parse_rfc3339(s).or_else(|| {
            let t = s.trim();
            t.parse::<i64>()
                .ok()
                .map(Timestamp::from_unix_int)
                .or_else(|| t.parse::<f64>().ok().and_then(Timestamp::from_unix_number))
        }),
        serde_json::Value::Number(n) => match n.as_i64() {
            Some(i) => Some(Timestamp::from_unix_int(i)),
            None => n.as_f64().and_then(Timestamp::from_unix_number),
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_round_trip() {
        let t = Timestamp::parse_rfc3339("2026-10-06T10:20:13.485Z").unwrap();
        assert_eq!(t.to_rfc3339(), "2026-10-06T10:20:13.485Z");
        let t = Timestamp::parse_rfc3339("2026-10-06T10:20:13.485123456+02:00").unwrap();
        assert_eq!(t.to_rfc3339(), "2026-10-06T08:20:13.485123456Z");
    }

    #[test]
    fn unix_unit_guessing() {
        let secs = Timestamp::from_unix_number(1_791_282_013.0).unwrap();
        let millis = Timestamp::from_unix_number(1_791_282_013_000.0).unwrap();
        let micros = Timestamp::from_unix_number(1_791_282_013_000_000.0).unwrap();
        let nanos = Timestamp::from_unix_number(1_791_282_013_000_000_000.0).unwrap();
        assert_eq!(secs, millis);
        assert_eq!(millis, micros);
        assert_eq!(micros, nanos);
    }

    #[test]
    fn integer_epochs_keep_precision() {
        assert_eq!(Timestamp::from_unix_int(1_791_282_013_485), Timestamp(1_791_282_013_485_000_000));
        assert_eq!(Timestamp::from_unix_int(1_791_282_013_485_123_456), Timestamp(1_791_282_013_485_123_456));
        assert_eq!(parse_json_timestamp(&serde_json::json!("1791282013")), Some(Timestamp::from_secs(1_791_282_013)));
    }

    #[test]
    fn rejects_garbage() {
        assert!(Timestamp::parse_rfc3339("yesterday").is_none());
        assert!(Timestamp::from_unix_number(f64::NAN).is_none());
        assert!(parse_json_timestamp(&serde_json::json!(true)).is_none());
    }

    #[test]
    fn millis_floor_for_negative() {
        assert_eq!(Timestamp(-1).millis(), -1);
        assert_eq!(Timestamp(1_999_999).millis(), 1);
    }
}
