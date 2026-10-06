//! Tokenizer for the query language.

use crate::error::ParseError;

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    /// Field name, possibly dotted: `http.statusCode`.
    Ident(String),
    Str(String),
    Int(i64),
    Float(f64),
    True,
    False,
    Null,
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
    And,
    Or,
    Not,
    Contains,
    LParen,
    RParen,
    Eof,
}

impl Tok {
    pub fn describe(&self) -> String {
        match self {
            Tok::Ident(s) => format!("field '{s}'"),
            Tok::Str(s) => format!("string \"{s}\""),
            Tok::Int(i) => format!("number {i}"),
            Tok::Float(f) => format!("number {f}"),
            Tok::True => "'true'".into(),
            Tok::False => "'false'".into(),
            Tok::Null => "'null'".into(),
            Tok::Eq => "'='".into(),
            Tok::Ne => "'!='".into(),
            Tok::Gt => "'>'".into(),
            Tok::Ge => "'>='".into(),
            Tok::Lt => "'<'".into(),
            Tok::Le => "'<='".into(),
            Tok::And => "'and'".into(),
            Tok::Or => "'or'".into(),
            Tok::Not => "'not'".into(),
            Tok::Contains => "'contains'".into(),
            Tok::LParen => "'('".into(),
            Tok::RParen => "')'".into(),
            Tok::Eof => "end of query".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub tok: Tok,
    /// Byte offset in the input.
    pub pos: usize,
}

fn ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_' || c == '@' || c == '$'
}

fn ident_continue(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '.' | '-' | '@' | '$')
}

pub fn tokenize(input: &str) -> Result<Vec<Token>, ParseError> {
    let mut out = Vec::new();
    let mut it = input.char_indices().peekable();
    while let Some(&(pos, c)) = it.peek() {
        if c.is_whitespace() {
            it.next();
            continue;
        }
        let single = |t: Tok| Token { tok: t, pos };
        match c {
            '(' => {
                it.next();
                out.push(single(Tok::LParen));
            }
            ')' => {
                it.next();
                out.push(single(Tok::RParen));
            }
            '=' => {
                it.next();
                // Accept `==` as a courtesy for people used to C-like syntax.
                if matches!(it.peek(), Some((_, '='))) {
                    it.next();
                }
                out.push(single(Tok::Eq));
            }
            '!' => {
                it.next();
                if matches!(it.peek(), Some((_, '='))) {
                    it.next();
                    out.push(single(Tok::Ne));
                } else {
                    return Err(ParseError::new("Expected '=' after '!'", pos));
                }
            }
            '<' => {
                it.next();
                match it.peek() {
                    Some((_, '=')) => {
                        it.next();
                        out.push(single(Tok::Le));
                    }
                    Some((_, '>')) => {
                        it.next();
                        out.push(single(Tok::Ne));
                    }
                    _ => out.push(single(Tok::Lt)),
                }
            }
            '>' => {
                it.next();
                if matches!(it.peek(), Some((_, '='))) {
                    it.next();
                    out.push(single(Tok::Ge));
                } else {
                    out.push(single(Tok::Gt));
                }
            }
            '"' | '\'' => {
                let quote = c;
                it.next();
                let mut s = String::new();
                let mut closed = false;
                while let Some((_, ch)) = it.next() {
                    if ch == quote {
                        closed = true;
                        break;
                    }
                    if ch == '\\' {
                        match it.next() {
                            Some((_, 'n')) => s.push('\n'),
                            Some((_, 't')) => s.push('\t'),
                            Some((_, 'r')) => s.push('\r'),
                            Some((_, e)) => s.push(e),
                            None => break,
                        }
                    } else {
                        s.push(ch);
                    }
                }
                if !closed {
                    return Err(ParseError::new("Unterminated string", pos));
                }
                out.push(Token { tok: Tok::Str(s), pos });
            }
            c if c.is_ascii_digit() || (c == '-' && next_is_digit(input, pos)) => {
                // -?digits(.digits)?([eE][+-]?digits)?
                let b = input.as_bytes();
                let digits = |mut i: usize| {
                    while i < b.len() && b[i].is_ascii_digit() {
                        i += 1;
                    }
                    i
                };
                let start = pos;
                let mut i = if b[pos] == b'-' { pos + 1 } else { pos };
                i = digits(i);
                let mut is_float = false;
                if i + 1 < b.len() && b[i] == b'.' && b[i + 1].is_ascii_digit() {
                    is_float = true;
                    i = digits(i + 1);
                }
                if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
                    let mut j = i + 1;
                    if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
                        j += 1;
                    }
                    if j < b.len() && b[j].is_ascii_digit() {
                        is_float = true;
                        i = digits(j);
                    }
                }
                while it.peek().is_some_and(|&(p, _)| p < i) {
                    it.next();
                }
                // `5m`, `10abc`: a number glued to letters is not a number.
                if let Some(&(_, ch)) = it.peek()
                    && (ident_start(ch) || ch == '.')
                {
                    return Err(ParseError::new(format!("Invalid number '{}{}'", &input[start..i], ch), start));
                }
                let text = &input[start..i];
                let finite = |f: f64| f.is_finite().then_some(f);
                let tok = if is_float {
                    text.parse::<f64>().ok().and_then(finite).map(Tok::Float)
                } else {
                    text.parse::<i64>()
                        .map(Tok::Int)
                        .ok()
                        .or_else(|| text.parse::<f64>().ok().and_then(finite).map(Tok::Float))
                };
                let tok = tok.ok_or_else(|| ParseError::new(format!("Invalid number '{text}'"), start))?;
                out.push(Token { tok, pos: start });
            }
            c if ident_start(c) => {
                let start = pos;
                let mut end = pos;
                while let Some(&(p, ch)) = it.peek() {
                    if !ident_continue(ch) {
                        break;
                    }
                    end = p + ch.len_utf8();
                    it.next();
                }
                let word = &input[start..end];
                if word.ends_with('.') {
                    return Err(ParseError::new(format!("Field name '{word}' cannot end with '.'"), start));
                }
                let tok = match word.to_ascii_lowercase().as_str() {
                    "and" => Tok::And,
                    "or" => Tok::Or,
                    "not" => Tok::Not,
                    "contains" => Tok::Contains,
                    "true" => Tok::True,
                    "false" => Tok::False,
                    "null" => Tok::Null,
                    _ => Tok::Ident(word.to_string()),
                };
                out.push(Token { tok, pos: start });
            }
            '&' if input[pos..].starts_with("&&") => {
                it.next();
                it.next();
                out.push(single(Tok::And));
            }
            '|' if input[pos..].starts_with("||") => {
                it.next();
                it.next();
                out.push(single(Tok::Or));
            }
            other => {
                return Err(ParseError::new(format!("Unexpected character '{other}'"), pos));
            }
        }
    }
    out.push(Token { tok: Tok::Eof, pos: input.len() });
    Ok(out)
}

fn next_is_digit(input: &str, pos: usize) -> bool {
    input[pos + 1..].chars().next().is_some_and(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(s: &str) -> Vec<Tok> {
        tokenize(s).unwrap().into_iter().map(|t| t.tok).collect()
    }

    #[test]
    fn basic_tokens() {
        assert_eq!(
            toks(r#"level = "Error" and durationMs >= 500"#),
            vec![
                Tok::Ident("level".into()),
                Tok::Eq,
                Tok::Str("Error".into()),
                Tok::And,
                Tok::Ident("durationMs".into()),
                Tok::Ge,
                Tok::Int(500),
                Tok::Eof
            ]
        );
    }

    #[test]
    fn operators() {
        assert_eq!(
            toks("a != 1 b <> 2 c < 3 d <= 4 e > 5 f == 6"),
            vec![
                Tok::Ident("a".into()),
                Tok::Ne,
                Tok::Int(1),
                Tok::Ident("b".into()),
                Tok::Ne,
                Tok::Int(2),
                Tok::Ident("c".into()),
                Tok::Lt,
                Tok::Int(3),
                Tok::Ident("d".into()),
                Tok::Le,
                Tok::Int(4),
                Tok::Ident("e".into()),
                Tok::Gt,
                Tok::Int(5),
                Tok::Ident("f".into()),
                Tok::Eq,
                Tok::Int(6),
                Tok::Eof
            ]
        );
    }

    #[test]
    fn keywords_case_insensitive() {
        assert_eq!(
            toks("NOT a AND b Or c CONTAINS null TRUE false"),
            vec![
                Tok::Not,
                Tok::Ident("a".into()),
                Tok::And,
                Tok::Ident("b".into()),
                Tok::Or,
                Tok::Ident("c".into()),
                Tok::Contains,
                Tok::Null,
                Tok::True,
                Tok::False,
                Tok::Eof
            ]
        );
    }

    #[test]
    fn numbers() {
        assert_eq!(toks("-12")[0], Tok::Int(-12));
        assert_eq!(toks("71.50")[0], Tok::Float(71.5));
        assert_eq!(toks("1e3")[0], Tok::Float(1000.0));
        assert_eq!(toks("2.5E-1")[0], Tok::Float(0.25));
        // Overflowing integers degrade to floats rather than failing.
        assert_eq!(toks("99999999999999999999")[0], Tok::Float(1e20));
        assert!(tokenize("5m").is_err());
    }

    #[test]
    fn strings_and_escapes() {
        assert_eq!(toks(r#""a \"b\" \n""#)[0], Tok::Str("a \"b\" \n".into()));
        assert_eq!(toks("'single'")[0], Tok::Str("single".into()));
        let e = tokenize(r#"message = "open"#).unwrap_err();
        assert_eq!(e.position, 10);
    }

    #[test]
    fn dotted_and_special_identifiers() {
        assert_eq!(toks("http.statusCode")[0], Tok::Ident("http.statusCode".into()));
        assert_eq!(toks("@t")[0], Tok::Ident("@t".into()));
        assert_eq!(toks("service-name")[0], Tok::Ident("service-name".into()));
        assert!(tokenize("http.").is_err());
    }

    #[test]
    fn errors_have_positions() {
        let e = tokenize("a = 1 # 2").unwrap_err();
        assert_eq!(e.position, 6);
        let e = tokenize("a ! 1").unwrap_err();
        assert_eq!(e.position, 2);
    }

    #[test]
    fn symbolic_boolean_operators() {
        assert_eq!(
            toks("a = 1 && b = 2 || c = 3"),
            vec![
                Tok::Ident("a".into()),
                Tok::Eq,
                Tok::Int(1),
                Tok::And,
                Tok::Ident("b".into()),
                Tok::Eq,
                Tok::Int(2),
                Tok::Or,
                Tok::Ident("c".into()),
                Tok::Eq,
                Tok::Int(3),
                Tok::Eof
            ]
        );
    }
}
