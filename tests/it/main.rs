#![allow(
    clippy::too_many_lines,
    reason = "a scenario test reads as one fixture, its calls, and its assertions"
)]

mod adapters;
mod cli;
mod commands;
mod common;
mod compressed;
mod counts;
mod cursors;
mod derived;
mod docs;
mod identity;
mod lookup;
mod messages;
mod outcomes;
mod output_prefix;
mod pages;
mod privacy;
mod refresh;
mod schema;
mod usage;
