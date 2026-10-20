#![warn(clippy::unwrap_used)]

/// What a query may spend refreshing the index, and then answering (`docs/cli.md`).
pub const BUDGET: std::time::Duration = std::time::Duration::from_secs(2);

#[expect(
    clippy::too_many_lines,
    reason = "parser sources change only with `PARSER_VERSION`, which reindexes every user's history, so this moves onto each agent's `parse` with the next parser change"
)]
pub mod adapters;
mod code_mode;
pub mod counting;
pub mod discovery;
pub mod model;
pub mod normalize;
#[expect(
    clippy::too_many_lines,
    reason = "parser sources change only with `PARSER_VERSION`, which reindexes every user's history, so this moves onto `structured` with the next parser change"
)]
pub mod outcomes;
pub mod query;
#[expect(
    clippy::too_many_lines,
    reason = "parser sources change only with `PARSER_VERSION`, which reindexes every user's history, so this moves onto `redact_named` with the next parser change"
)]
pub mod redaction;
pub mod shell;
pub mod source;
pub mod store;
