//! Query AST. Parsing produces this; execution consumes it. Neither knows
//! about the other.

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    Comparison(Comparison),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Comparison {
    pub field: String,
    pub op: Op,
    pub value: Literal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
    Contains,
}

impl Op {
    pub fn as_str(self) -> &'static str {
        match self {
            Op::Eq => "=",
            Op::Ne => "!=",
            Op::Gt => ">",
            Op::Ge => ">=",
            Op::Lt => "<",
            Op::Le => "<=",
            Op::Contains => "contains",
        }
    }

    pub fn is_ordering(self) -> bool {
        matches!(self, Op::Gt | Op::Ge | Op::Lt | Op::Le)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Null,
}

impl Literal {
    pub fn to_value(&self) -> telemetry::Value {
        match self {
            Literal::String(s) => telemetry::Value::String(s.clone()),
            Literal::Int(i) => telemetry::Value::Int(*i),
            Literal::Float(f) => telemetry::Value::Float(*f),
            Literal::Bool(b) => telemetry::Value::Bool(*b),
            Literal::Null => telemetry::Value::Null,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Literal::Int(i) => Some(*i as f64),
            Literal::Float(f) => Some(*f),
            _ => None,
        }
    }

    pub fn is_number(&self) -> bool {
        matches!(self, Literal::Int(_) | Literal::Float(_))
    }

    /// Text form used by `contains` and string-only fields.
    pub fn as_text(&self) -> String {
        match self {
            Literal::String(s) => s.clone(),
            Literal::Int(i) => i.to_string(),
            Literal::Float(f) => f.to_string(),
            Literal::Bool(b) => b.to_string(),
            Literal::Null => "null".into(),
        }
    }
}

impl Expr {
    pub fn and(a: Expr, b: Expr) -> Expr {
        Expr::And(Box::new(a), Box::new(b))
    }

    pub fn or(a: Expr, b: Expr) -> Expr {
        Expr::Or(Box::new(a), Box::new(b))
    }

    #[allow(clippy::should_implement_trait)]
    pub fn not(a: Expr) -> Expr {
        Expr::Not(Box::new(a))
    }

    pub fn cmp(field: &str, op: Op, value: Literal) -> Expr {
        Expr::Comparison(Comparison { field: field.to_string(), op, value })
    }

    /// Visit every comparison (used for query statistics and planning).
    pub fn comparisons(&self) -> Vec<&Comparison> {
        let mut out = Vec::new();
        fn walk<'a>(e: &'a Expr, out: &mut Vec<&'a Comparison>) {
            match e {
                Expr::And(a, b) | Expr::Or(a, b) => {
                    walk(a, out);
                    walk(b, out);
                }
                Expr::Not(a) => walk(a, out),
                Expr::Comparison(c) => out.push(c),
            }
        }
        walk(self, &mut out);
        out
    }

    fn prec(&self) -> u8 {
        match self {
            Expr::Or(..) => 1,
            Expr::And(..) => 2,
            Expr::Not(..) => 3,
            Expr::Comparison(_) => 4,
        }
    }

    fn fmt_prec(&self, f: &mut fmt::Formatter<'_>, min: u8) -> fmt::Result {
        let paren = self.prec() < min;
        if paren {
            f.write_str("(")?;
        }
        match self {
            // Binary operators are left-associative, so a right operand of
            // the same precedence needs parentheses to round-trip.
            Expr::Or(a, b) => {
                a.fmt_prec(f, 1)?;
                f.write_str(" or ")?;
                b.fmt_prec(f, 2)?;
            }
            Expr::And(a, b) => {
                a.fmt_prec(f, 2)?;
                f.write_str(" and ")?;
                b.fmt_prec(f, 3)?;
            }
            Expr::Not(a) => {
                f.write_str("not ")?;
                a.fmt_prec(f, 3)?;
            }
            Expr::Comparison(c) => write!(f, "{c}")?,
        }
        if paren {
            f.write_str(")")?;
        }
        Ok(())
    }
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.fmt_prec(f, 0)
    }
}

impl fmt::Display for Comparison {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} {}", self.field, self.op.as_str(), self.value)
    }
}

impl fmt::Display for Literal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Literal::String(s) => {
                f.write_str("\"")?;
                for c in s.chars() {
                    match c {
                        '"' => f.write_str("\\\"")?,
                        '\\' => f.write_str("\\\\")?,
                        '\n' => f.write_str("\\n")?,
                        '\t' => f.write_str("\\t")?,
                        '\r' => f.write_str("\\r")?,
                        c => write!(f, "{c}")?,
                    }
                }
                f.write_str("\"")
            }
            Literal::Int(i) => write!(f, "{i}"),
            // Debug formatting always includes a '.' or exponent, so the
            // value re-lexes as a float.
            Literal::Float(x) => write!(f, "{x:?}"),
            Literal::Bool(b) => write!(f, "{b}"),
            Literal::Null => f.write_str("null"),
        }
    }
}
