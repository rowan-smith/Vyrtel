//! Recursive-descent parser.
//!
//! ```text
//! query      := expr? EOF
//! expr       := or
//! or         := and ("or" and)*
//! and        := unary ("and" unary)*
//! unary      := "not" unary | primary
//! primary    := "(" expr ")" | STRING | comparison
//! comparison := FIELD op value
//! op         := "=" | "!=" | ">" | ">=" | "<" | "<=" | "contains"
//! value      := STRING | NUMBER | "true" | "false" | "null" | WORD
//! ```
//!
//! A bare string is shorthand for `message contains "<string>"`.

use crate::ast::{Comparison, Expr, Literal, Op};
use crate::error::ParseError;
use crate::lexer::{Tok, Token, tokenize};

/// Maximum nesting depth (parentheses / `not`), to bound recursion.
const MAX_DEPTH: usize = 64;

/// Parse a query. An empty (or whitespace-only) query matches everything
/// and returns `None`.
pub fn parse(input: &str) -> Result<Option<Expr>, ParseError> {
    let tokens = tokenize(input)?;
    if tokens.len() == 1 {
        return Ok(None);
    }
    let mut p = Parser { tokens, pos: 0, depth: 0 };
    let e = p.expr()?;
    let t = p.peek();
    if t.tok != Tok::Eof {
        let msg = match &t.tok {
            Tok::RParen => "Unexpected ')' without matching '('".to_string(),
            Tok::Ident(_) | Tok::Str(_) | Tok::LParen | Tok::Not => {
                format!("Expected 'and' or 'or' before {}", t.tok.describe())
            }
            other => format!("Unexpected {}", other.describe()),
        };
        return Err(ParseError::new(msg, t.pos));
    }
    Ok(Some(e))
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    depth: usize,
}

impl Parser {
    fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn next(&mut self) -> Token {
        let t = self.tokens[self.pos].clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        t
    }

    fn expr(&mut self) -> Result<Expr, ParseError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(ParseError::new("Query is nested too deeply", self.peek().pos));
        }
        let r = self.or();
        self.depth -= 1;
        r
    }

    fn or(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.and()?;
        while self.peek().tok == Tok::Or {
            self.next();
            self.expect_operand("or")?;
            let right = self.and()?;
            left = Expr::or(left, right);
        }
        Ok(left)
    }

    fn and(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.unary()?;
        while self.peek().tok == Tok::And {
            self.next();
            self.expect_operand("and")?;
            let right = self.unary()?;
            left = Expr::and(left, right);
        }
        Ok(left)
    }

    fn expect_operand(&self, after: &str) -> Result<(), ParseError> {
        match self.peek().tok {
            Tok::Eof | Tok::RParen | Tok::And | Tok::Or => {
                Err(ParseError::new(format!("Expected expression after '{after}'"), self.peek().pos))
            }
            _ => Ok(()),
        }
    }

    fn unary(&mut self) -> Result<Expr, ParseError> {
        if self.peek().tok == Tok::Not {
            self.next();
            self.expect_operand("not")?;
            self.depth += 1;
            if self.depth > MAX_DEPTH {
                return Err(ParseError::new("Query is nested too deeply", self.peek().pos));
            }
            let inner = self.unary();
            self.depth -= 1;
            return Ok(Expr::not(inner?));
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Expr, ParseError> {
        let t = self.next();
        match t.tok {
            Tok::LParen => {
                if self.peek().tok == Tok::RParen {
                    return Err(ParseError::new("Empty parentheses", self.peek().pos));
                }
                let e = self.expr()?;
                let close = self.next();
                if close.tok != Tok::RParen {
                    return Err(ParseError::new(format!("Expected ')' but found {}", close.tok.describe()), close.pos));
                }
                Ok(e)
            }
            Tok::Str(s) => Ok(Expr::cmp("message", Op::Contains, Literal::String(s))),
            Tok::Ident(field) => {
                let op_tok = self.next();
                let op = match op_tok.tok {
                    Tok::Eq => Op::Eq,
                    Tok::Ne => Op::Ne,
                    Tok::Gt => Op::Gt,
                    Tok::Ge => Op::Ge,
                    Tok::Lt => Op::Lt,
                    Tok::Le => Op::Le,
                    Tok::Contains => Op::Contains,
                    other => {
                        return Err(ParseError::new(
                            format!("Expected an operator after '{field}' but found {}", other.describe()),
                            op_tok.pos,
                        ));
                    }
                };
                let v = self.next();
                let value = match v.tok {
                    Tok::Str(s) => Literal::String(s),
                    Tok::Int(i) => Literal::Int(i),
                    Tok::Float(f) => Literal::Float(f),
                    Tok::True => Literal::Bool(true),
                    Tok::False => Literal::Bool(false),
                    Tok::Null => Literal::Null,
                    // Unquoted words are accepted as strings: `level = Error`.
                    Tok::Ident(w) => Literal::String(w),
                    other => {
                        return Err(ParseError::new(
                            format!("Expected a value after '{}' but found {}", op.as_str(), other.describe()),
                            v.pos,
                        ));
                    }
                };
                if op == Op::Contains && value == Literal::Null {
                    return Err(ParseError::new("'contains' needs a text value", v.pos));
                }
                Ok(Expr::Comparison(Comparison { field, op, value }))
            }
            other => {
                Err(ParseError::new(format!("Expected a field, string or '(' but found {}", other.describe()), t.pos))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Expr {
        parse(s).unwrap().unwrap()
    }

    fn err(s: &str) -> ParseError {
        parse(s).unwrap_err()
    }

    #[test]
    fn spec_examples_parse() {
        for q in [
            r#"level = "Error""#,
            r#"service = "payments""#,
            r#"level = "Error" and service = "payments""#,
            "durationMs > 500",
            r#"message contains "timeout""#,
            "customerId = 123",
            r#"traceId = "abc123""#,
            "http.statusCode = 500",
        ] {
            assert!(parse(q).unwrap().is_some(), "{q}");
        }
    }

    #[test]
    fn empty_query_matches_all() {
        assert_eq!(parse("").unwrap(), None);
        assert_eq!(parse("   ").unwrap(), None);
    }

    #[test]
    fn precedence_and_binds_tighter_than_or() {
        assert_eq!(
            p("a = 1 or b = 2 and c = 3"),
            Expr::or(
                Expr::cmp("a", Op::Eq, Literal::Int(1)),
                Expr::and(Expr::cmp("b", Op::Eq, Literal::Int(2)), Expr::cmp("c", Op::Eq, Literal::Int(3)))
            )
        );
    }

    #[test]
    fn not_binds_tighter_than_and() {
        assert_eq!(
            p("not a = 1 and b = 2"),
            Expr::and(Expr::not(Expr::cmp("a", Op::Eq, Literal::Int(1))), Expr::cmp("b", Op::Eq, Literal::Int(2)))
        );
    }

    #[test]
    fn parentheses_override_precedence() {
        assert_eq!(
            p("(a = 1 or b = 2) and c = 3"),
            Expr::and(
                Expr::or(Expr::cmp("a", Op::Eq, Literal::Int(1)), Expr::cmp("b", Op::Eq, Literal::Int(2))),
                Expr::cmp("c", Op::Eq, Literal::Int(3))
            )
        );
    }

    #[test]
    fn left_associative() {
        assert_eq!(
            p("a = 1 and b = 2 and c = 3"),
            Expr::and(
                Expr::and(Expr::cmp("a", Op::Eq, Literal::Int(1)), Expr::cmp("b", Op::Eq, Literal::Int(2))),
                Expr::cmp("c", Op::Eq, Literal::Int(3))
            )
        );
    }

    #[test]
    fn literals() {
        assert_eq!(p("a = null"), Expr::cmp("a", Op::Eq, Literal::Null));
        assert_eq!(p("a != true"), Expr::cmp("a", Op::Ne, Literal::Bool(true)));
        assert_eq!(p("a <= 1.5"), Expr::cmp("a", Op::Le, Literal::Float(1.5)));
        assert_eq!(p("level = Error"), Expr::cmp("level", Op::Eq, Literal::String("Error".into())));
    }

    #[test]
    fn bare_string_is_message_contains() {
        assert_eq!(
            p(r#""timeout" and level = Error"#),
            Expr::and(
                Expr::cmp("message", Op::Contains, Literal::String("timeout".into())),
                Expr::cmp("level", Op::Eq, Literal::String("Error".into()))
            )
        );
    }

    #[test]
    fn helpful_errors() {
        let e = err(r#"level = "Error" and"#);
        assert_eq!(e.message, "Expected expression after 'and'");
        assert_eq!(e.position, 19);
        assert_eq!(err("level =").message, "Expected a value after '=' but found end of query");
        assert!(err("level").message.starts_with("Expected an operator after 'level'"));
        assert!(err("(a = 1").message.starts_with("Expected ')'"));
        assert!(err("a = 1)").message.contains("')'"));
        assert!(err("a = 1 b = 2").message.starts_with("Expected 'and' or 'or'"));
        assert_eq!(err("()").message, "Empty parentheses");
        assert!(err("= 1").message.starts_with("Expected a field"));
        assert_eq!(err("not").message, "Expected expression after 'not'");
        assert!(err("a contains null").message.contains("contains"));
    }

    #[test]
    fn deep_nesting_is_rejected_not_overflowed() {
        let q = format!("{}a = 1{}", "(".repeat(500), ")".repeat(500));
        assert!(err(&q).message.contains("nested"));
        let q = format!("{}a = 1", "not ".repeat(500));
        assert!(err(&q).message.contains("nested"));
    }
}
