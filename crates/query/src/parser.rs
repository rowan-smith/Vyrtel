use crate::ast::{
    Aggregation, ComparisonOperator, Expression, ParsedQuery, QueryValue, TimeInterval,
};
use crate::lexer::{LexError, Lexer, Token, TokenKind};

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub code: String,
    pub message: String,
    pub position: usize,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ParseError {}

impl From<LexError> for ParseError {
    fn from(e: LexError) -> Self {
        Self {
            code: "invalid_query".into(),
            message: e.message,
            position: e.position,
        }
    }
}

pub fn parse_query(input: &str) -> Result<ParsedQuery, ParseError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(ParsedQuery {
            filter: None,
            aggregation: None,
        });
    }

    // Free-text shortcut: no operators present
    if is_free_text(trimmed) {
        return Ok(ParsedQuery {
            filter: Some(Expression::FreeText(trimmed.to_string())),
            aggregation: None,
        });
    }

    let mut lexer = Lexer::new(trimmed);
    let tokens = lexer.tokenize()?;
    let mut parser = Parser {
        tokens,
        pos: 0,
        source: trimmed,
    };
    parser.parse_pipeline()
}

fn is_free_text(s: &str) -> bool {
    let operators = ["=", "!=", ">=", "<=", ">", "<", "|", "(", ")"];
    if operators.iter().any(|op| s.contains(op)) {
        return false;
    }
    let lower = s.to_ascii_lowercase();
    // Keywords that indicate structured query
    if lower.contains(" and ")
        || lower.contains(" or ")
        || lower.starts_with("not ")
        || lower.contains(" contains ")
    {
        return false;
    }
    true
}

struct Parser<'a> {
    tokens: Vec<Token>,
    pos: usize,
    source: &'a str,
}

impl<'a> Parser<'a> {
    fn current(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn bump(&mut self) -> &Token {
        let tok = &self.tokens[self.pos];
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        tok
    }

    fn parse_pipeline(&mut self) -> Result<ParsedQuery, ParseError> {
        let filter = if matches!(self.current().kind, TokenKind::Pipe | TokenKind::Eof) {
            None
        } else {
            Some(self.parse_or()?)
        };

        let aggregation = if matches!(self.current().kind, TokenKind::Pipe) {
            self.bump();
            Some(self.parse_aggregation()?)
        } else {
            None
        };

        if !matches!(self.current().kind, TokenKind::Eof) {
            return Err(ParseError {
                code: "invalid_query".into(),
                message: format!("Unexpected token near '{}'", &self.source[self.current().start..self.current().end.min(self.source.len())]),
                position: self.current().start,
            });
        }

        Ok(ParsedQuery {
            filter,
            aggregation,
        })
    }

    fn parse_or(&mut self) -> Result<Expression, ParseError> {
        let mut left = self.parse_and()?;
        while matches!(self.current().kind, TokenKind::Or) {
            self.bump();
            let right = self.parse_and()?;
            left = Expression::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expression, ParseError> {
        let mut left = self.parse_not()?;
        while matches!(self.current().kind, TokenKind::And) {
            self.bump();
            let right = self.parse_not()?;
            left = Expression::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_not(&mut self) -> Result<Expression, ParseError> {
        if matches!(self.current().kind, TokenKind::Not) {
            self.bump();
            let inner = self.parse_not()?;
            return Ok(Expression::Not(Box::new(inner)));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Expression, ParseError> {
        match &self.current().kind {
            TokenKind::LParen => {
                self.bump();
                let expr = self.parse_or()?;
                if !matches!(self.current().kind, TokenKind::RParen) {
                    return Err(ParseError {
                        code: "invalid_query".into(),
                        message: "Expected ')'".into(),
                        position: self.current().start,
                    });
                }
                self.bump();
                Ok(expr)
            }
            TokenKind::Ident(_) => self.parse_comparison_or_contains(),
            _ => Err(ParseError {
                code: "invalid_query".into(),
                message: "Expected expression".into(),
                position: self.current().start,
            }),
        }
    }

    fn parse_comparison_or_contains(&mut self) -> Result<Expression, ParseError> {
        let field = match &self.bump().kind {
            TokenKind::Ident(s) => s.clone(),
            _ => unreachable!(),
        };

        if matches!(self.current().kind, TokenKind::Contains) {
            self.bump();
            let value = self.parse_string_value()?;
            return Ok(Expression::Contains { field, value });
        }

        let operator = match &self.current().kind {
            TokenKind::Eq => ComparisonOperator::Eq,
            TokenKind::Neq => ComparisonOperator::Neq,
            TokenKind::Gt => ComparisonOperator::Gt,
            TokenKind::Gte => ComparisonOperator::Gte,
            TokenKind::Lt => ComparisonOperator::Lt,
            TokenKind::Lte => ComparisonOperator::Lte,
            _ => {
                return Err(ParseError {
                    code: "invalid_query".into(),
                    message: format!("Expected operator after field '{field}'"),
                    position: self.current().start,
                });
            }
        };
        self.bump();

        let value = self.parse_value()?;
        Ok(Expression::Comparison {
            field,
            operator,
            value,
        })
    }

    fn parse_value(&mut self) -> Result<QueryValue, ParseError> {
        let tok = self.current().clone();
        match tok.kind {
            TokenKind::String(s) => {
                self.bump();
                Ok(QueryValue::String(s))
            }
            TokenKind::Number(n) => {
                self.bump();
                Ok(QueryValue::Number(n))
            }
            TokenKind::Bool(b) => {
                self.bump();
                Ok(QueryValue::Bool(b))
            }
            TokenKind::Ident(s) => {
                // Bare identifier treated as string (e.g. level = error)
                self.bump();
                Ok(QueryValue::String(s))
            }
            _ => Err(ParseError {
                code: "invalid_query".into(),
                message: "Expected a value after operator".into(),
                position: tok.start,
            }),
        }
    }

    fn parse_string_value(&mut self) -> Result<String, ParseError> {
        match self.parse_value()? {
            QueryValue::String(s) => Ok(s),
            other => Err(ParseError {
                code: "invalid_query".into(),
                message: format!("Expected string, got {other:?}"),
                position: self.current().start,
            }),
        }
    }

    fn parse_aggregation(&mut self) -> Result<Aggregation, ParseError> {
        let name = match &self.current().kind {
            TokenKind::Ident(s) => {
                let s = s.to_ascii_lowercase();
                self.bump();
                s
            }
            _ => {
                return Err(ParseError {
                    code: "invalid_query".into(),
                    message: "Expected aggregation after '|'".into(),
                    position: self.current().start,
                });
            }
        };

        match name.as_str() {
            "count" => {
                if matches!(self.current().kind, TokenKind::By) {
                    self.bump();
                    self.parse_count_by()
                } else {
                    Ok(Aggregation::Count)
                }
            }
            "avg" | "sum" | "min" | "max" => {
                let field = self.parse_agg_field_arg()?;
                Ok(match name.as_str() {
                    "avg" => Aggregation::Avg { field },
                    "sum" => Aggregation::Sum { field },
                    "min" => Aggregation::Min { field },
                    "max" => Aggregation::Max { field },
                    _ => unreachable!(),
                })
            }
            other => Err(ParseError {
                code: "invalid_query".into(),
                message: format!("Unknown aggregation '{other}'"),
                position: self.current().start,
            }),
        }
    }

    fn parse_count_by(&mut self) -> Result<Aggregation, ParseError> {
        // count by time(5m)  OR  count by service
        match &self.current().kind {
            TokenKind::Ident(s) if s.eq_ignore_ascii_case("time") => {
                self.bump();
                if !matches!(self.current().kind, TokenKind::LParen) {
                    return Err(ParseError {
                        code: "invalid_query".into(),
                        message: "Expected '(' after time".into(),
                        position: self.current().start,
                    });
                }
                self.bump();
                let interval_str = match &self.current().kind {
                    TokenKind::Ident(s) => s.clone(),
                    TokenKind::String(s) => s.clone(),
                    _ => {
                        return Err(ParseError {
                            code: "invalid_query".into(),
                            message: "Expected time interval".into(),
                            position: self.current().start,
                        });
                    }
                };
                self.bump();
                if !matches!(self.current().kind, TokenKind::RParen) {
                    return Err(ParseError {
                        code: "invalid_query".into(),
                        message: "Expected ')'".into(),
                        position: self.current().start,
                    });
                }
                self.bump();
                let interval = TimeInterval::parse(&interval_str).ok_or_else(|| ParseError {
                    code: "invalid_query".into(),
                    message: format!("Unsupported interval '{interval_str}'"),
                    position: self.current().start,
                })?;
                Ok(Aggregation::CountByTime { interval })
            }
            TokenKind::Ident(s) => {
                let field = s.clone();
                self.bump();
                Ok(Aggregation::CountBy { field })
            }
            _ => Err(ParseError {
                code: "invalid_query".into(),
                message: "Expected field or time(...) after 'by'".into(),
                position: self.current().start,
            }),
        }
    }

    fn parse_agg_field_arg(&mut self) -> Result<String, ParseError> {
        // avg(duration_ns) or avg duration_ns
        if matches!(self.current().kind, TokenKind::LParen) {
            self.bump();
            let field = match &self.current().kind {
                TokenKind::Ident(s) => {
                    let s = s.clone();
                    self.bump();
                    s
                }
                _ => {
                    return Err(ParseError {
                        code: "invalid_query".into(),
                        message: "Expected field name".into(),
                        position: self.current().start,
                    });
                }
            };
            if !matches!(self.current().kind, TokenKind::RParen) {
                return Err(ParseError {
                    code: "invalid_query".into(),
                    message: "Expected ')'".into(),
                    position: self.current().start,
                });
            }
            self.bump();
            Ok(field)
        } else if let TokenKind::Ident(s) = &self.current().kind {
            let s = s.clone();
            self.bump();
            Ok(s)
        } else {
            Err(ParseError {
                code: "invalid_query".into(),
                message: "Expected field name".into(),
                position: self.current().start,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_or() {
        let q = parse_query(r#"(service = "api" or service = "billing") and level = error"#).unwrap();
        assert!(q.filter.is_some());
        assert!(q.aggregation.is_none());
    }

    #[test]
    fn parses_free_text() {
        let q = parse_query("database timeout").unwrap();
        assert_eq!(
            q.filter,
            Some(Expression::FreeText("database timeout".into()))
        );
    }

    #[test]
    fn parses_contains() {
        let q = parse_query(r#"message contains "timeout""#).unwrap();
        assert!(matches!(
            q.filter,
            Some(Expression::Contains { ref field, ref value })
                if field == "message" && value == "timeout"
        ));
    }

    #[test]
    fn parses_aggregation() {
        let q = parse_query("level = error | count by service").unwrap();
        assert!(matches!(
            q.aggregation,
            Some(Aggregation::CountBy { ref field }) if field == "service"
        ));
    }

    #[test]
    fn parses_count_by_time() {
        let q = parse_query("level = error | count by time(5m)").unwrap();
        assert!(matches!(
            q.aggregation,
            Some(Aggregation::CountByTime { interval: TimeInterval::FiveMinutes })
        ));
    }

    #[test]
    fn error_on_missing_value() {
        let err = parse_query("service =").unwrap_err();
        assert_eq!(err.code, "invalid_query");
    }
}
