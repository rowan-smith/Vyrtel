#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    Ident(String),
    String(String),
    Number(f64),
    Bool(bool),
    Eq,
    Neq,
    Gt,
    Gte,
    Lt,
    Lte,
    And,
    Or,
    Not,
    Contains,
    By,
    LParen,
    RParen,
    Pipe,
    Eof,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub start: usize,
    pub end: usize,
}

pub struct Lexer<'a> {
    input: &'a str,
    chars: Vec<(usize, char)>,
    pos: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(input: &'a str) -> Self {
        Self {
            input,
            chars: input.char_indices().collect(),
            pos: 0,
        }
    }

    pub fn tokenize(&mut self) -> Result<Vec<Token>, LexError> {
        let mut tokens = Vec::new();
        loop {
            let tok = self.next_token()?;
            let is_eof = tok.kind == TokenKind::Eof;
            tokens.push(tok);
            if is_eof {
                break;
            }
        }
        Ok(tokens)
    }

    fn peek(&self) -> Option<(usize, char)> {
        self.chars.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<(usize, char)> {
        let c = self.peek()?;
        self.pos += 1;
        Some(c)
    }

    fn next_token(&mut self) -> Result<Token, LexError> {
        self.skip_whitespace();
        let Some((start, ch)) = self.peek() else {
            return Ok(Token {
                kind: TokenKind::Eof,
                start: self.input.len(),
                end: self.input.len(),
            });
        };

        match ch {
            '(' => {
                self.bump();
                Ok(Token {
                    kind: TokenKind::LParen,
                    start,
                    end: start + 1,
                })
            }
            ')' => {
                self.bump();
                Ok(Token {
                    kind: TokenKind::RParen,
                    start,
                    end: start + 1,
                })
            }
            '|' => {
                self.bump();
                Ok(Token {
                    kind: TokenKind::Pipe,
                    start,
                    end: start + 1,
                })
            }
            '=' => {
                self.bump();
                Ok(Token {
                    kind: TokenKind::Eq,
                    start,
                    end: start + 1,
                })
            }
            '!' => {
                self.bump();
                if matches!(self.peek(), Some((_, '='))) {
                    self.bump();
                    Ok(Token {
                        kind: TokenKind::Neq,
                        start,
                        end: start + 2,
                    })
                } else {
                    Err(LexError {
                        message: "Expected '=' after '!'".into(),
                        position: start,
                    })
                }
            }
            '>' => {
                self.bump();
                if matches!(self.peek(), Some((_, '='))) {
                    self.bump();
                    Ok(Token {
                        kind: TokenKind::Gte,
                        start,
                        end: start + 2,
                    })
                } else {
                    Ok(Token {
                        kind: TokenKind::Gt,
                        start,
                        end: start + 1,
                    })
                }
            }
            '<' => {
                self.bump();
                if matches!(self.peek(), Some((_, '='))) {
                    self.bump();
                    Ok(Token {
                        kind: TokenKind::Lte,
                        start,
                        end: start + 2,
                    })
                } else {
                    Ok(Token {
                        kind: TokenKind::Lt,
                        start,
                        end: start + 1,
                    })
                }
            }
            '"' | '\'' => self.read_string(),
            c if c.is_ascii_digit() || (c == '-' && self.peek_is_digit()) => self.read_number(),
            c if is_ident_start(c) => self.read_ident_or_keyword(),
            _ => Err(LexError {
                message: format!("Unexpected character '{ch}'"),
                position: start,
            }),
        }
    }

    fn peek_is_digit(&self) -> bool {
        self.chars
            .get(self.pos + 1)
            .map(|(_, c)| c.is_ascii_digit())
            .unwrap_or(false)
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some((_, c)) if c.is_whitespace()) {
            self.bump();
        }
    }

    fn read_string(&mut self) -> Result<Token, LexError> {
        let (start, quote) = self.bump().unwrap();
        let mut value = String::new();
        loop {
            match self.bump() {
                Some((_, c)) if c == quote => {
                    return Ok(Token {
                        kind: TokenKind::String(value),
                        start,
                        end: self.chars.get(self.pos).map(|(i, _)| *i).unwrap_or(self.input.len()),
                    });
                }
                Some((_, '\\')) => {
                    if let Some((_, escaped)) = self.bump() {
                        value.push(match escaped {
                            'n' => '\n',
                            't' => '\t',
                            'r' => '\r',
                            '"' => '"',
                            '\'' => '\'',
                            '\\' => '\\',
                            other => other,
                        });
                    }
                }
                Some((_, c)) => value.push(c),
                None => {
                    return Err(LexError {
                        message: "Unterminated string".into(),
                        position: start,
                    });
                }
            }
        }
    }

    fn read_number(&mut self) -> Result<Token, LexError> {
        let (start, _) = self.peek().unwrap();
        let mut s = String::new();
        if matches!(self.peek(), Some((_, '-'))) {
            s.push(self.bump().unwrap().1);
        }
        while matches!(self.peek(), Some((_, c)) if c.is_ascii_digit() || c == '.') {
            s.push(self.bump().unwrap().1);
        }
        // Interval unit suffix: 5m, 1h, 1d
        if matches!(self.peek(), Some((_, c)) if c.is_ascii_alphabetic()) {
            while matches!(self.peek(), Some((_, c)) if c.is_ascii_alphanumeric()) {
                s.push(self.bump().unwrap().1);
            }
            let end = self.chars.get(self.pos).map(|(i, _)| *i).unwrap_or(self.input.len());
            return Ok(Token {
                kind: TokenKind::Ident(s),
                start,
                end,
            });
        }
        let end = self.chars.get(self.pos).map(|(i, _)| *i).unwrap_or(self.input.len());
        let n: f64 = s.parse().map_err(|_| LexError {
            message: format!("Invalid number '{s}'"),
            position: start,
        })?;
        Ok(Token {
            kind: TokenKind::Number(n),
            start,
            end,
        })
    }

    fn read_ident_or_keyword(&mut self) -> Result<Token, LexError> {
        let (start, _) = self.peek().unwrap();
        let mut s = String::new();
        while matches!(self.peek(), Some((_, c)) if is_ident_continue(c)) {
            s.push(self.bump().unwrap().1);
        }
        let end = self.chars.get(self.pos).map(|(i, _)| *i).unwrap_or(self.input.len());
        let kind = match s.to_ascii_lowercase().as_str() {
            "and" => TokenKind::And,
            "or" => TokenKind::Or,
            "not" => TokenKind::Not,
            "contains" => TokenKind::Contains,
            "by" => TokenKind::By,
            "true" => TokenKind::Bool(true),
            "false" => TokenKind::Bool(false),
            _ => TokenKind::Ident(s),
        };
        Ok(Token { kind, start, end })
    }
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || c == '.'
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-'
}

#[derive(Debug, Clone, PartialEq)]
pub struct LexError {
    pub message: String,
    pub position: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_comparison() {
        let mut lexer = Lexer::new(r#"service = "api" and level >= error"#);
        let tokens = lexer.tokenize().unwrap();
        assert!(matches!(tokens[0].kind, TokenKind::Ident(ref s) if s == "service"));
        assert_eq!(tokens[1].kind, TokenKind::Eq);
        assert!(matches!(tokens[2].kind, TokenKind::String(ref s) if s == "api"));
        assert_eq!(tokens[3].kind, TokenKind::And);
    }

    #[test]
    fn tokenizes_pipe() {
        let mut lexer = Lexer::new("level = error | count by service");
        let tokens = lexer.tokenize().unwrap();
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Pipe));
        assert!(tokens.iter().any(|t| t.kind == TokenKind::By));
    }
}
