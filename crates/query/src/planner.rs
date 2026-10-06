use event::LogLevel;
use tantivy::query::{
    AllQuery, BooleanQuery, Occur, PhraseQuery, Query, RangeQuery, TermQuery,
};
use tantivy::schema::{Field, IndexRecordOption, Term};
use tantivy::Index;
use thiserror::Error;

use crate::ast::{
    resolve_field, ComparisonOperator, Expression, QueryValue,
};
use crate::ParseError;

#[derive(Debug, Error)]
pub enum PlannerError {
    #[error("{0}")]
    Parse(#[from] ParseError),
    #[error("query planning error: {0}")]
    Message(String),
    #[error("tantivy error: {0}")]
    Tantivy(#[from] tantivy::TantivyError),
}

/// Convert a filter expression into a Tantivy query.
pub fn plan_filter(index: &Index, expr: Option<&Expression>) -> Result<Box<dyn Query>, PlannerError> {
    match expr {
        None => Ok(Box::new(AllQuery)),
        Some(e) => expression_to_query(index, e),
    }
}

fn expression_to_query(index: &Index, expr: &Expression) -> Result<Box<dyn Query>, PlannerError> {
    match expr {
        Expression::And(a, b) => Ok(Box::new(BooleanQuery::new(vec![
            (Occur::Must, expression_to_query(index, a)?),
            (Occur::Must, expression_to_query(index, b)?),
        ]))),
        Expression::Or(a, b) => Ok(Box::new(BooleanQuery::new(vec![
            (Occur::Should, expression_to_query(index, a)?),
            (Occur::Should, expression_to_query(index, b)?),
        ]))),
        Expression::Not(inner) => Ok(Box::new(BooleanQuery::new(vec![
            (Occur::Must, Box::new(AllQuery) as Box<dyn Query>),
            (Occur::MustNot, expression_to_query(index, inner)?),
        ]))),
        Expression::FreeText(text) => {
            let schema = index.schema();
            let message = schema.get_field("message")?;
            let template = schema.get_field("message_template")?;
            let stacktrace = schema.get_field("stacktrace")?;
            let mut clauses = Vec::new();
            for term_text in text.split_whitespace() {
                let lower = term_text.to_ascii_lowercase();
                for field in [message, template, stacktrace] {
                    let t = Term::from_field_text(field, &lower);
                    clauses.push((
                        Occur::Should,
                        Box::new(TermQuery::new(t, IndexRecordOption::WithFreqs)) as Box<dyn Query>,
                    ));
                }
            }
            if clauses.is_empty() {
                Ok(Box::new(AllQuery))
            } else {
                Ok(Box::new(BooleanQuery::new(clauses)))
            }
        }
        Expression::Contains { field, value } => {
            let resolved = resolve_field(field);
            contains_query(index, &resolved, value)
        }
        Expression::Comparison {
            field,
            operator,
            value,
        } => {
            let resolved = resolve_field(field);
            comparison_query(index, &resolved, *operator, value)
        }
    }
}

fn contains_query(
    index: &Index,
    field_name: &str,
    value: &str,
) -> Result<Box<dyn Query>, PlannerError> {
    let schema = index.schema();

    if let Some(path) = field_name.strip_prefix("attributes.") {
        let field = schema.get_field("attributes")?;
        let term = json_string_term(field, path, &value.to_ascii_lowercase())?;
        return Ok(Box::new(TermQuery::new(term, IndexRecordOption::WithFreqs)));
    }

    let field = schema.get_field(field_name).map_err(|_| {
        PlannerError::Message(format!("Unknown field '{field_name}'"))
    })?;

    let lower = value.to_ascii_lowercase();
    let terms: Vec<Term> = lower
        .split_whitespace()
        .map(|t| Term::from_field_text(field, t))
        .collect();
    if terms.is_empty() {
        return Ok(Box::new(AllQuery));
    }
    if terms.len() == 1 {
        return Ok(Box::new(TermQuery::new(
            terms[0].clone(),
            IndexRecordOption::WithFreqs,
        )));
    }
    Ok(Box::new(PhraseQuery::new(terms)))
}

fn comparison_query(
    index: &Index,
    field_name: &str,
    op: ComparisonOperator,
    value: &QueryValue,
) -> Result<Box<dyn Query>, PlannerError> {
    let schema = index.schema();

    if let Some(path) = field_name.strip_prefix("attributes.") {
        return attribute_comparison(index, path, op, value);
    }

    // Special handling for level comparisons with ordering
    if field_name == "level" {
        return level_comparison(index, op, value);
    }

    if field_name == "duration_ns" {
        return duration_comparison(index, op, value);
    }

    let field = schema.get_field(field_name).map_err(|_| {
        PlannerError::Message(format!("Unknown field '{field_name}'"))
    })?;

    let text = value_as_string(value);
    // Normalise level-like bare values for string fields
    let text = if field_name == "level" {
        LogLevel::parse(&text)
            .map(|l| l.as_str().to_string())
            .unwrap_or(text)
    } else {
        text
    };

    match op {
        ComparisonOperator::Eq => {
            let term = Term::from_field_text(field, &text);
            Ok(Box::new(TermQuery::new(term, IndexRecordOption::Basic)))
        }
        ComparisonOperator::Neq => {
            let term = Term::from_field_text(field, &text);
            Ok(Box::new(BooleanQuery::new(vec![
                (Occur::Must, Box::new(AllQuery) as Box<dyn Query>),
                (
                    Occur::MustNot,
                    Box::new(TermQuery::new(term, IndexRecordOption::Basic)) as Box<dyn Query>,
                ),
            ])))
        }
        ComparisonOperator::Gt
        | ComparisonOperator::Gte
        | ComparisonOperator::Lt
        | ComparisonOperator::Lte => {
            // String range on STRING fields
            string_range(field, field_name, op, &text)
        }
    }
}

fn level_comparison(
    index: &Index,
    op: ComparisonOperator,
    value: &QueryValue,
) -> Result<Box<dyn Query>, PlannerError> {
    let schema = index.schema();
    let field = schema.get_field("level")?;
    let text = value_as_string(value);
    let target = LogLevel::parse(&text).ok_or_else(|| {
        PlannerError::Message(format!("Invalid log level '{text}'"))
    })?;

    match op {
        ComparisonOperator::Eq => {
            let term = Term::from_field_text(field, target.as_str());
            Ok(Box::new(TermQuery::new(term, IndexRecordOption::Basic)))
        }
        ComparisonOperator::Neq => {
            let term = Term::from_field_text(field, target.as_str());
            Ok(Box::new(BooleanQuery::new(vec![
                (Occur::Must, Box::new(AllQuery) as Box<dyn Query>),
                (
                    Occur::MustNot,
                    Box::new(TermQuery::new(term, IndexRecordOption::Basic)) as Box<dyn Query>,
                ),
            ])))
        }
        ComparisonOperator::Gte | ComparisonOperator::Gt | ComparisonOperator::Lte | ComparisonOperator::Lt => {
            let all_levels = [
                LogLevel::Trace,
                LogLevel::Debug,
                LogLevel::Information,
                LogLevel::Warning,
                LogLevel::Error,
                LogLevel::Fatal,
            ];
            let rank = target.rank();
            let mut clauses = Vec::new();
            for level in all_levels {
                let include = match op {
                    ComparisonOperator::Gte => level.rank() >= rank,
                    ComparisonOperator::Gt => level.rank() > rank,
                    ComparisonOperator::Lte => level.rank() <= rank,
                    ComparisonOperator::Lt => level.rank() < rank,
                    _ => false,
                };
                if include {
                    let term = Term::from_field_text(field, level.as_str());
                    clauses.push((
                        Occur::Should,
                        Box::new(TermQuery::new(term, IndexRecordOption::Basic)) as Box<dyn Query>,
                    ));
                }
            }
            if clauses.is_empty() {
                // Match nothing
                Ok(Box::new(BooleanQuery::new(vec![(
                    Occur::Must,
                    Box::new(TermQuery::new(
                        Term::from_field_text(field, "__none__"),
                        IndexRecordOption::Basic,
                    )) as Box<dyn Query>,
                )])))
            } else {
                Ok(Box::new(BooleanQuery::new(clauses)))
            }
        }
    }
}

fn duration_comparison(
    index: &Index,
    op: ComparisonOperator,
    value: &QueryValue,
) -> Result<Box<dyn Query>, PlannerError> {
    let n = match value {
        QueryValue::Number(n) => *n as u64,
        QueryValue::String(s) => s.parse::<u64>().map_err(|_| {
            PlannerError::Message(format!("Expected number for duration_ns, got '{s}'"))
        })?,
        _ => {
            return Err(PlannerError::Message(
                "Expected number for duration_ns".into(),
            ))
        }
    };

    let (lower, upper) = match op {
        ComparisonOperator::Eq => (
            std::ops::Bound::Included(n),
            std::ops::Bound::Included(n),
        ),
        ComparisonOperator::Neq => {
            let field = index.schema().get_field("duration_ns")?;
            let term = Term::from_field_u64(field, n);
            return Ok(Box::new(BooleanQuery::new(vec![
                (Occur::Must, Box::new(AllQuery) as Box<dyn Query>),
                (
                    Occur::MustNot,
                    Box::new(TermQuery::new(term, IndexRecordOption::Basic)) as Box<dyn Query>,
                ),
            ])));
        }
        ComparisonOperator::Gt => (std::ops::Bound::Excluded(n), std::ops::Bound::Unbounded),
        ComparisonOperator::Gte => (std::ops::Bound::Included(n), std::ops::Bound::Unbounded),
        ComparisonOperator::Lt => (std::ops::Bound::Unbounded, std::ops::Bound::Excluded(n)),
        ComparisonOperator::Lte => (std::ops::Bound::Unbounded, std::ops::Bound::Included(n)),
    };

    Ok(Box::new(RangeQuery::new_u64_bounds(
        "duration_ns".to_string(),
        lower,
        upper,
    )))
}

fn attribute_comparison(
    index: &Index,
    path: &str,
    op: ComparisonOperator,
    value: &QueryValue,
) -> Result<Box<dyn Query>, PlannerError> {
    let schema = index.schema();
    let field = schema.get_field("attributes")?;

    match value {
        QueryValue::Number(n)
            if n.fract() == 0.0 && *n >= i64::MIN as f64 && *n <= i64::MAX as f64 =>
        {
            let i = *n as i64;
            match op {
                ComparisonOperator::Eq => {
                    let term = json_i64_term(field, path, i)?;
                    Ok(Box::new(TermQuery::new(term, IndexRecordOption::Basic)))
                }
                ComparisonOperator::Neq => {
                    let term = json_i64_term(field, path, i)?;
                    Ok(Box::new(BooleanQuery::new(vec![
                        (Occur::Must, Box::new(AllQuery) as Box<dyn Query>),
                        (
                            Occur::MustNot,
                            Box::new(TermQuery::new(term, IndexRecordOption::Basic))
                                as Box<dyn Query>,
                        ),
                    ])))
                }
                ComparisonOperator::Gt
                | ComparisonOperator::Gte
                | ComparisonOperator::Lt
                | ComparisonOperator::Lte => {
                    // Approximate ranges by fetching candidates via existence of path+post-filter
                    // isn't available easily; emit a boolean of discrete nearby values is wrong.
                    // Use inverted index term enumeration via a wide AllQuery is too broad.
                    // Practical MVP: encode as Eq-style bound terms using Term range on JSON
                    // by building lower/upper JSON i64 terms — Tantivy RangeQuery needs schema fields.
                    // Fall back: match all and rely on caller post-filter — instead, reject with
                    // a clear message only if we can't do better. We'll scan with AllQuery and
                    // note that attribute ranges are applied in aggregation/search post-path.
                    // For search, use a custom approach: return AllQuery and document limitation —
                    // Better: use multiple Should terms is impossible for open ranges.
                    // Use JsonTermWriter bounds via inverted index range — not exposed.
                    // Simplest working approach for MVP ranges on attrs: AllQuery + mark for
                    // post-filter in storage. To keep planner pure, return a TermQuery that
                    // matches nothing useful... 
                    // Actually we'll implement post-filter attribute ranges in the app layer later.
                    // For now support only Eq/Neq for numbers via exact term, and for inequalities
                    // use a BooleanQuery that ORs many values — no.
                    // Use RangeQuery on a dedicated approach: store duration-like attrs separately? No.
                    Err(PlannerError::Message(format!(
                        "Attribute range queries on '{path}' are limited; use equality or filter in UI"
                    )))
                }
            }
        }
        _ => {
            let term = json_term(field, path, value)?;
            match op {
                ComparisonOperator::Eq => {
                    Ok(Box::new(TermQuery::new(term, IndexRecordOption::Basic)))
                }
                ComparisonOperator::Neq => Ok(Box::new(BooleanQuery::new(vec![
                    (Occur::Must, Box::new(AllQuery) as Box<dyn Query>),
                    (
                        Occur::MustNot,
                        Box::new(TermQuery::new(term, IndexRecordOption::Basic)) as Box<dyn Query>,
                    ),
                ]))),
                _ => Err(PlannerError::Message(format!(
                    "Range comparison requires an integer for '{path}'"
                ))),
            }
        }
    }
}

fn json_term(field: Field, path: &str, value: &QueryValue) -> Result<Term, PlannerError> {
    match value {
        QueryValue::String(s) => json_string_term(field, path, &s.to_ascii_lowercase()),
        QueryValue::Number(n) if n.fract() == 0.0 => json_i64_term(field, path, *n as i64),
        QueryValue::Number(n) => json_string_term(field, path, &n.to_string()),
        QueryValue::Bool(b) => json_bool_term(field, path, *b),
        QueryValue::Null => json_string_term(field, path, "null"),
    }
}

fn json_string_term(field: Field, path: &str, value: &str) -> Result<Term, PlannerError> {
    let mut term = Term::with_capacity(path.len() + value.len() + 16);
    {
        let mut writer =
            tantivy::json_utils::JsonTermWriter::from_field_and_json_path(field, path, true, &mut term);
        writer.set_str(value);
    }
    Ok(term)
}

fn json_i64_term(field: Field, path: &str, value: i64) -> Result<Term, PlannerError> {
    let mut term = Term::with_capacity(path.len() + 32);
    {
        let mut writer =
            tantivy::json_utils::JsonTermWriter::from_field_and_json_path(field, path, true, &mut term);
        writer.close_path_and_set_type(tantivy::schema::Type::I64);
    }
    let encoded = Term::from_field_i64(Field::from_field_id(0), value);
    term.append_bytes(encoded.serialized_value_bytes());
    Ok(term)
}

fn json_bool_term(field: Field, path: &str, value: bool) -> Result<Term, PlannerError> {
    let mut term = Term::with_capacity(path.len() + 16);
    {
        let mut writer =
            tantivy::json_utils::JsonTermWriter::from_field_and_json_path(field, path, true, &mut term);
        writer.close_path_and_set_type(tantivy::schema::Type::Bool);
    }
    let encoded = Term::from_field_bool(Field::from_field_id(0), value);
    term.append_bytes(encoded.serialized_value_bytes());
    Ok(term)
}

fn string_range(
    field: Field,
    field_name: &str,
    op: ComparisonOperator,
    value: &str,
) -> Result<Box<dyn Query>, PlannerError> {
    let _ = field;
    let (lower, upper): (std::ops::Bound<&str>, std::ops::Bound<&str>) = match op {
        ComparisonOperator::Gt => (std::ops::Bound::Excluded(value), std::ops::Bound::Unbounded),
        ComparisonOperator::Gte => (std::ops::Bound::Included(value), std::ops::Bound::Unbounded),
        ComparisonOperator::Lt => (std::ops::Bound::Unbounded, std::ops::Bound::Excluded(value)),
        ComparisonOperator::Lte => (std::ops::Bound::Unbounded, std::ops::Bound::Included(value)),
        _ => unreachable!(),
    };
    Ok(Box::new(RangeQuery::new_str_bounds(
        field_name.to_string(),
        lower,
        upper,
    )))
}

fn value_as_string(value: &QueryValue) -> String {
    match value {
        QueryValue::String(s) => s.clone(),
        QueryValue::Number(n) => {
            if n.fract() == 0.0 {
                format!("{}", *n as i64)
            } else {
                n.to_string()
            }
        }
        QueryValue::Bool(b) => b.to_string(),
        QueryValue::Null => "null".into(),
    }
}
