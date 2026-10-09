//! What the CLI and the LSP both ask of a checkout, beneath either front:
//! the fact under a position, what a definition is asked of there, which of
//! an answer's locations a definition or
//! a declaration request gets, an example group's members, how a call site is
//! tiered for references, a variable's mentions, and which file a `require`
//! string names. Neither front imports
//! the other for these.

pub(crate) mod def;
pub(crate) mod locations;
pub(crate) mod members;
pub(crate) mod position;
pub(crate) mod refs;
pub(crate) mod require;
pub(crate) mod variables;
