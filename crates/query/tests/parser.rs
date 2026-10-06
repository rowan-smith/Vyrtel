//! Parser and lexer behaviour through the public API.

use query::{
    is_well_known_field, parse_query, resolve_field, Aggregation, ComparisonOperator, Expression,
    Lexer, QueryValue, TimeInterval, TokenKind,
};

fn filter(q: &str) -> Expression {
    parse_query(q).unwrap().filter.unwrap()
}

fn cmp(field: &str, operator: ComparisonOperator, value: QueryValue) -> Expression {
    Expression::Comparison { field: field.into(), operator, value }
}

fn kinds(input: &str) -> Vec<TokenKind> {
    Lexer::new(input).tokenize().unwrap().into_iter().map(|t| t.kind).collect()
}

#[test]
fn empty_and_whitespace_queries_have_no_filter() {
    for q in ["", "   ", "\n\t"] {
        let p = parse_query(q).unwrap();
        assert!(p.filter.is_none() && p.aggregation.is_none(), "{q:?}");
    }
}

#[test]
fn every_comparison_operator() {
    use ComparisonOperator::*;
    for (src, op) in [("=", Eq), ("!=", Neq), (">", Gt), (">=", Gte), ("<", Lt), ("<=", Lte)] {
        assert_eq!(filter(&format!("x {src} 1")), cmp("x", op, QueryValue::Number(1.0)), "{src}");
    }
}

#[test]
fn value_types() {
    use ComparisonOperator::Eq;
    assert_eq!(filter(r#"a = "s""#), cmp("a", Eq, QueryValue::String("s".into())));
    assert_eq!(filter("a = 's'"), cmp("a", Eq, QueryValue::String("s".into())));
    assert_eq!(filter("a = 1.5"), cmp("a", Eq, QueryValue::Number(1.5)));
    assert_eq!(filter("a = -3"), cmp("a", Eq, QueryValue::Number(-3.0)));
    assert_eq!(filter("a = true"), cmp("a", Eq, QueryValue::Bool(true)));
    assert_eq!(filter("a = FALSE"), cmp("a", Eq, QueryValue::Bool(false)));
    assert_eq!(filter("a = bare"), cmp("a", Eq, QueryValue::String("bare".into())));
}

#[test]
fn dotted_and_dashed_identifiers() {
    assert_eq!(
        filter("http.status_code = 500"),
        cmp("http.status_code", ComparisonOperator::Eq, QueryValue::Number(500.0))
    );
    assert_eq!(
        filter("service = my-service"),
        cmp("service", ComparisonOperator::Eq, QueryValue::String("my-service".into()))
    );
}

#[test]
fn string_escapes() {
    assert_eq!(
        filter(r#"m = "a \"quoted\" \\ line\nnext\ttab""#),
        cmp("m", ComparisonOperator::Eq, QueryValue::String("a \"quoted\" \\ line\nnext\ttab".into()))
    );
    assert_eq!(
        filter(r"m = 'it\'s'"),
        cmp("m", ComparisonOperator::Eq, QueryValue::String("it's".into()))
    );
}

#[test]
fn keywords_are_case_insensitive() {
    let lower = filter("a = 1 and b = 2 or not c = 3");
    let upper = filter("a = 1 AND b = 2 OR NOT c = 3");
    assert_eq!(lower, upper);
}

#[test]
fn precedence_not_and_or() {
    use ComparisonOperator::Eq;
    let a = || cmp("a", Eq, QueryValue::Number(1.0));
    let b = || cmp("b", Eq, QueryValue::Number(2.0));
    let c = || cmp("c", Eq, QueryValue::Number(3.0));

    assert_eq!(
        filter("a = 1 or b = 2 and c = 3"),
        Expression::Or(Box::new(a()), Box::new(Expression::And(Box::new(b()), Box::new(c()))))
    );
    assert_eq!(
        filter("(a = 1 or b = 2) and c = 3"),
        Expression::And(Box::new(Expression::Or(Box::new(a()), Box::new(b()))), Box::new(c()))
    );
    assert_eq!(
        filter("not a = 1 and b = 2"),
        Expression::And(Box::new(Expression::Not(Box::new(a()))), Box::new(b()))
    );
    assert_eq!(
        filter("not not a = 1"),
        Expression::Not(Box::new(Expression::Not(Box::new(a()))))
    );
}

#[test]
fn and_or_chains_are_left_associative() {
    use ComparisonOperator::Eq;
    let n = |f: &str, v: f64| cmp(f, Eq, QueryValue::Number(v));
    assert_eq!(
        filter("a = 1 and b = 2 and c = 3"),
        Expression::And(
            Box::new(Expression::And(Box::new(n("a", 1.0)), Box::new(n("b", 2.0)))),
            Box::new(n("c", 3.0))
        )
    );
}

#[test]
fn free_text_detection() {
    assert_eq!(filter("database timeout"), Expression::FreeText("database timeout".into()));
    assert_eq!(filter("  padded  "), Expression::FreeText("padded".into()));
    // Words that merely contain keywords are still free text.
    assert_eq!(filter("android order"), Expression::FreeText("android order".into()));
    // Operators or keyword phrases switch to structured parsing.
    assert!(!matches!(filter("a = 1"), Expression::FreeText(_)));
    assert!(!matches!(filter(r#"message contains "x""#), Expression::FreeText(_)));
}

#[test]
fn contains_requires_string_value() {
    let err = parse_query("message contains 5").unwrap_err();
    assert!(err.message.contains("Expected string"), "{}", err.message);
    // Bare identifiers are accepted as strings.
    assert!(matches!(
        filter("message contains timeout"),
        Expression::Contains { ref value, .. } if value == "timeout"
    ));
}

#[test]
fn aggregations() {
    let agg = |q: &str| parse_query(q).unwrap().aggregation.unwrap();
    assert_eq!(agg("| count"), Aggregation::Count);
    assert_eq!(agg("| COUNT"), Aggregation::Count);
    assert_eq!(agg("| count by service"), Aggregation::CountBy { field: "service".into() });
    assert_eq!(agg("| avg(duration_ns)"), Aggregation::Avg { field: "duration_ns".into() });
    assert_eq!(agg("| sum amount"), Aggregation::Sum { field: "amount".into() });
    assert_eq!(agg("| min(x)"), Aggregation::Min { field: "x".into() });
    assert_eq!(agg("| max(x)"), Aggregation::Max { field: "x".into() });
    assert_eq!(
        agg(r#"| count by time("1h")"#),
        Aggregation::CountByTime { interval: TimeInterval::OneHour }
    );
}

#[test]
fn aggregation_without_filter() {
    let p = parse_query("| count").unwrap();
    assert!(p.filter.is_none());
    assert_eq!(p.aggregation, Some(Aggregation::Count));
}

#[test]
fn every_time_interval() {
    for s in ["1m", "5m", "15m", "30m", "1h", "6h", "1d"] {
        let p = parse_query(&format!("| count by time({s})")).unwrap();
        let Some(Aggregation::CountByTime { interval }) = p.aggregation else { panic!("{s}") };
        assert_eq!(interval.as_str(), s);
        assert_eq!(TimeInterval::parse(s), Some(interval));
    }
    assert_eq!(TimeInterval::OneMinute.as_secs(), 60);
    assert_eq!(TimeInterval::OneDay.as_secs(), 86_400);
}

#[test]
fn parse_errors_have_code_and_position() {
    let cases = [
        ("service =", "Expected a value"),
        ("service", ""), // bare word is free text, not an error — checked separately below
        ("(a = 1", "Expected ')'"),
        ("a = 1)", "Unexpected token"),
        ("a = 1 |", "Expected aggregation"),
        ("a = 1 | median(x)", "Unknown aggregation"),
        ("a = 1 | count by", "Expected field"),
        ("a = 1 | count by time(7m)", "Unsupported interval"),
        ("a = 1 | count by time 5m", "Expected '('"),
        ("a = 1 | count by time(5m", "Expected ')'"),
        ("a = 1 | avg()", "Expected field name"),
        ("a = 1 | avg(x", "Expected ')'"),
        ("a = 1 and", "Expected expression"),
        ("a = 1 and b ! 2", "Expected '=' after '!'"),
        (r#"a = "unterminated"#, "Unterminated string"),
        ("a = 1 # 2", "Unexpected character"),
        ("a 1 = 2", "Expected operator"),
    ];
    for (q, expected) in cases {
        if expected.is_empty() {
            assert!(parse_query(q).is_ok());
            continue;
        }
        let err = parse_query(q).expect_err(q);
        assert_eq!(err.code, "invalid_query", "{q}");
        assert!(err.message.contains(expected), "{q}: got {:?}", err.message);
        assert!(err.position <= q.len(), "{q}: position {} out of range", err.position);
    }
}

#[test]
fn error_position_points_at_problem() {
    let err = parse_query("a = 1 and b = 'x").unwrap_err();
    assert_eq!(err.position, 14);
    let err = parse_query("a = 1 # 2").unwrap_err();
    assert_eq!(err.position, 6);
}

#[test]
fn non_ascii_input_does_not_panic() {
    // Multibyte characters inside strings are fine; outside they are rejected cleanly.
    assert_eq!(
        filter(r#"message = "café ☕""#),
        cmp("message", ComparisonOperator::Eq, QueryValue::String("café ☕".into()))
    );
    assert!(parse_query("a = 1 and ☕ = 2").is_err());
}

#[test]
fn lexer_tokens() {
    assert_eq!(
        kinds("a>=1 b<2|count by x"),
        vec![
            TokenKind::Ident("a".into()),
            TokenKind::Gte,
            TokenKind::Number(1.0),
            TokenKind::Ident("b".into()),
            TokenKind::Lt,
            TokenKind::Number(2.0),
            TokenKind::Pipe,
            TokenKind::Ident("count".into()),
            TokenKind::By,
            TokenKind::Ident("x".into()),
            TokenKind::Eof,
        ]
    );
    // Numbers followed by letters lex as identifiers (interval literals).
    assert_eq!(kinds("5m")[0], TokenKind::Ident("5m".into()));
}

#[test]
fn lexer_token_spans_are_byte_offsets() {
    let tokens = Lexer::new(r#"svc = "é" x"#).tokenize().unwrap();
    assert_eq!((tokens[0].start, tokens[0].end), (0, 3));
    assert_eq!((tokens[2].start, tokens[2].end), (6, 10)); // "é" is 2 bytes + 2 quotes
    assert_eq!(tokens[3].start, 11);
}

#[test]
fn field_resolution() {
    for f in ["level", "service", "message", "trace_id", "duration_ns", "event_type"] {
        assert!(is_well_known_field(f));
        assert_eq!(resolve_field(f), f);
    }
    assert!(!is_well_known_field("customerId"));
    assert_eq!(resolve_field("customerId"), "attributes.customerId");
    assert_eq!(resolve_field("http.method"), "attributes.http.method");
    assert_eq!(resolve_field("attributes.customerId"), "attributes.customerId");
}
