mod classify;
pub(crate) mod drill;
mod format;
mod history;
mod keywords;
pub(crate) mod refs;
pub(crate) mod lex;
pub(crate) mod mssql_value;
mod relations;
mod scope;
mod split;
pub(crate) mod tsql;

pub use classify::{DmlVerb, Invalidates, StmtKind, classify};
pub use format::format_statement;
pub use relations::{Vet, relation_names, vet};
pub(crate) use relations::is_read_statement;
pub(crate) use scope::scope_at;
pub use split::{ByteTarget, StmtSpan, pick, split_statements};

pub(crate) use history::{history_is_write, history_text};
pub(crate) use keywords::{NEVER_A_FUNCTION, RESERVED, TYPE_OR_FUNCTION_NAME};
#[cfg(all(test, feature = "pg-tests"))]
pub(crate) use keywords::OTHER_KEYWORDS;
#[cfg(all(test, feature = "pg-tests"))]
pub(crate) use relations::{VETTED, VETTED_OPERATORS, VETTED_SIZES};

pub(crate) mod order;

pub(crate) mod map;
