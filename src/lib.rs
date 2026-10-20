#![warn(clippy::unwrap_used)]

/// What a query may spend refreshing the index, and then answering (`docs/cli.md`).
pub const BUDGET: std::time::Duration = std::time::Duration::from_secs(2);

pub mod adapters;
mod code_mode;
pub mod counting;
pub mod discovery;
pub mod model;
pub mod normalize;
pub mod outcomes;
pub mod query;
pub mod redaction;
pub mod shell;
pub mod source;
pub mod store;
