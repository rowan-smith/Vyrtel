//! arbitrary valid query AST → serialise → parse → equivalent AST

use proptest::prelude::*;
use query::ast::{Expr, Literal, Op};
use query::parse;

fn field() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("level".to_string()),
        Just("service".to_string()),
        Just("http.statusCode".to_string()),
        Just("@t".to_string()),
        // Generated names must not collide with keywords.
        "[a-z_][a-zA-Z0-9_]{0,8}(\\.[a-zA-Z_][a-zA-Z0-9_]{0,5}){0,2}".prop_filter("keyword", |s| !matches!(
            s.to_ascii_lowercase().as_str(),
            "and" | "or" | "not" | "contains" | "true" | "false" | "null"
        )),
    ]
}

fn literal() -> impl Strategy<Value = Literal> {
    prop_oneof![
        any::<String>().prop_map(Literal::String),
        any::<i64>().prop_map(Literal::Int),
        any::<f64>().prop_filter("finite", |f| f.is_finite()).prop_map(Literal::Float),
        any::<bool>().prop_map(Literal::Bool),
        Just(Literal::Null),
    ]
}

fn op() -> impl Strategy<Value = Op> {
    proptest::sample::select(vec![Op::Eq, Op::Ne, Op::Gt, Op::Ge, Op::Lt, Op::Le, Op::Contains])
}

fn expr() -> impl Strategy<Value = Expr> {
    let leaf = (field(), op(), literal())
        .prop_filter("contains null is invalid", |(_, o, l)| !(*o == Op::Contains && *l == Literal::Null))
        .prop_map(|(f, o, l)| Expr::cmp(&f, o, l));
    leaf.prop_recursive(5, 32, 2, |inner| {
        prop_oneof![
            (inner.clone(), inner.clone()).prop_map(|(a, b)| Expr::and(a, b)),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| Expr::or(a, b)),
            inner.prop_map(Expr::not),
        ]
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    #[test]
    fn display_then_parse_is_identity(e in expr()) {
        let text = e.to_string();
        let back = parse(&text).unwrap_or_else(|err| panic!("{text}: {err}")).unwrap();
        prop_assert_eq!(back, e, "{}", text);
    }

    /// Parsing never panics on arbitrary input.
    #[test]
    fn parse_never_panics(s in any::<String>()) {
        let _ = parse(&s);
    }

    #[test]
    fn parse_never_panics_on_query_like_input(s in "[a-z =!<>()\"'.0-9]{0,40}") {
        let _ = parse(&s);
    }
}
