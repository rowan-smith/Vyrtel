//! Vyrtel's query language and execution engine.
//!
//! Parsing ([`parser`]) produces an [`ast::Expr`] and knows nothing about
//! storage. Execution compiles the AST ([`eval`]), prunes segments and
//! blocks with indexes ([`prune`]) and evaluates the remaining predicate
//! ([`exec`]). The grammar is documented in `docs/query-language.md`.

pub mod aggregate;
pub mod ast;
mod engine;
mod error;
pub mod eval;
pub mod exec;
pub mod fields;
pub mod lexer;
pub mod parser;
pub mod prune;
pub mod stats;
pub mod traces;

pub use engine::{Engine, MetricInfo, MetricOutput, MetricQuery, SearchOutput, compile_query};
pub use error::{ParseError, QueryError};
pub use exec::{Cursor, Diagnostics, Direction, QueryLimits, TimeRange};
pub use parser::parse;
