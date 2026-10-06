use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum QueryValue {
    String(String),
    Number(f64),
    Bool(bool),
    Null,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ComparisonOperator {
    Eq,
    Neq,
    Gt,
    Gte,
    Lt,
    Lte,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Expression {
    Comparison {
        field: String,
        operator: ComparisonOperator,
        value: QueryValue,
    },
    And(Box<Expression>, Box<Expression>),
    Or(Box<Expression>, Box<Expression>),
    Not(Box<Expression>),
    Contains {
        field: String,
        value: String,
    },
    FreeText(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Aggregation {
    Count,
    CountBy { field: String },
    CountByTime { interval: TimeInterval },
    Avg { field: String },
    Sum { field: String },
    Min { field: String },
    Max { field: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeInterval {
    OneMinute,
    FiveMinutes,
    FifteenMinutes,
    ThirtyMinutes,
    OneHour,
    SixHours,
    OneDay,
}

impl TimeInterval {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "1m" => Some(Self::OneMinute),
            "5m" => Some(Self::FiveMinutes),
            "15m" => Some(Self::FifteenMinutes),
            "30m" => Some(Self::ThirtyMinutes),
            "1h" => Some(Self::OneHour),
            "6h" => Some(Self::SixHours),
            "1d" => Some(Self::OneDay),
            _ => None,
        }
    }

    pub fn as_secs(&self) -> i64 {
        match self {
            Self::OneMinute => 60,
            Self::FiveMinutes => 300,
            Self::FifteenMinutes => 900,
            Self::ThirtyMinutes => 1800,
            Self::OneHour => 3600,
            Self::SixHours => 21600,
            Self::OneDay => 86400,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::OneMinute => "1m",
            Self::FiveMinutes => "5m",
            Self::FifteenMinutes => "15m",
            Self::ThirtyMinutes => "30m",
            Self::OneHour => "1h",
            Self::SixHours => "6h",
            Self::OneDay => "1d",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParsedQuery {
    pub filter: Option<Expression>,
    pub aggregation: Option<Aggregation>,
}

/// Well-known top-level fields (everything else maps to attributes.*).
pub fn is_well_known_field(field: &str) -> bool {
    matches!(
        field,
        "level"
            | "service"
            | "environment"
            | "message"
            | "message_template"
            | "stacktrace"
            | "trace_id"
            | "span_id"
            | "parent_span_id"
            | "event_type"
            | "duration_ns"
            | "timestamp"
            | "id"
    )
}

pub fn resolve_field(field: &str) -> String {
    if is_well_known_field(field) || field.starts_with("attributes.") {
        field.to_string()
    } else {
        format!("attributes.{field}")
    }
}
