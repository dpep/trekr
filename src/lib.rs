//! trekr: the engine, and two fronts over it — `cli/` and `serve/`, the
//! language server — which only this root sees together.

pub(crate) mod background;
pub(crate) mod cli;
pub(crate) mod core;
pub(crate) mod extract;
pub(crate) mod failure;
pub(crate) mod gems;
pub(crate) mod inflect;
pub(crate) mod log;
pub(crate) mod query;
pub(crate) mod rbs;
pub(crate) mod resolve;
pub(crate) mod scan;
pub(crate) mod schema;
pub(crate) mod serve;
pub(crate) mod store;
pub(crate) mod tree;
pub(crate) mod usage;

/// The binary's entry: the CLI, which hands `--lsp` to the language server.
pub fn run() -> std::process::ExitCode {
    cli::run(serve::run)
}
