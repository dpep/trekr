//! Where a click came back empty or unsure, so it can be read back and fixed
//! (DEC-083).
//!
//! `--usage` counts that 40% of definitions missed; this says which ones. A
//! `miss` line goes to `lsp.log` — local, beside the store, the same file that
//! already names every requested file and line — with the column, the token
//! under the cursor, and the engine's one-line reason. Nothing new leaves the
//! machine, and `TREKR_LOG=off` turns it off with the rest of the log.
//!
//! Written after the answer is on the wire, never ahead of it: the token is
//! read then, from the document the request already loaded.

use std::cell::RefCell;
use std::path::PathBuf;

thread_local! {
    static WHY: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// The handler's reason, in a line: the last one noted wins.
pub(crate) fn why(text: impl Into<String>) {
    WHY.with(|w| *w.borrow_mut() = Some(text.into()));
}

/// Take and clear the reason, so the next request starts from nothing.
pub(crate) fn take_why() -> Option<String> {
    WHY.with(|w| w.borrow_mut().take())
}

/// A miss, as the dispatcher saw it. The token is read later, off the clock.
pub(crate) struct Miss {
    pub(crate) op: &'static str,
    pub(crate) path: PathBuf,
    pub(crate) position: lsp_types::Position,
    pub(crate) outcome: String,
    pub(crate) why: Option<String>,
}

/// The requests a miss is recorded for: the two a click sends.
pub(crate) fn op_of(method: &str) -> Option<&'static str> {
    match method {
        "textDocument/definition" => Some("definition"),
        "textDocument/hover" => Some("hover"),
        _ => None,
    }
}

impl Miss {
    /// The log line. `line` and `col` are 1-based bytes, as `--def` takes
    /// them: the character the caret `reads` in the document's `text`, so
    /// a miss pastes straight into the CLI and asks it the same question.
    pub(crate) fn event(&self, read: Option<(crate::core::Pos, &str)>) -> serde_json::Value {
        serde_json::json!({
            "op": self.op,
            "file": self.path.to_string_lossy(),
            "line": read.map_or(self.position.line + 1, |(at, _)| at.line),
            "col": read.map_or(self.position.character + 1, |(at, _)| at.col),
            "token": read.map(|(_, t)| token_at(t, self.position)).unwrap_or_default(),
            "outcome": self.outcome,
            "why": self.why,
        })
    }
}

/// Longest token kept: a name, not a line of code.
const TOKEN_MAX: usize = 64;

/// The identifier under a position, with the sigils and suffixes that are part
/// of a Ruby name (`@ivar`, `$global`, `valid?`, `save!`, `Foo::Bar`). Empty
/// when the cursor is on punctuation or whitespace — itself worth knowing.
pub(crate) fn token_at(text: &str, position: lsp_types::Position) -> String {
    let offset = super::convert::offset_of(text, position);
    let line_start = text[..offset].rfind('\n').map_or(0, |n| n + 1);
    let line_end = text[offset..].find('\n').map_or(text.len(), |n| offset + n);
    let line = &text[line_start..line_end];
    let at = offset - line_start;
    let part = |c: char| c.is_alphanumeric() || matches!(c, '_' | ':' | '@' | '$');
    let start = line[..at]
        .char_indices()
        .rev()
        .take_while(|(_, c)| part(*c))
        .last()
        .map_or(at, |(i, _)| i);
    let mut end = line[at..]
        .char_indices()
        .find(|(_, c)| !part(*c))
        .map_or(line.len(), |(i, _)| at + i);
    if line[end..].starts_with(['?', '!']) {
        end += 1;
    }
    line[start..end]
        .trim_matches(':')
        .chars()
        .take(TOKEN_MAX)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::Position;

    fn at(text: &str, line: u32, character: u32) -> String {
        token_at(text, Position { line, character })
    }

    #[test]
    fn reads_the_ruby_name_under_the_cursor() {
        let text = "x = 1\nfoo.valid?(@bar, Baz::Qux)\n";
        assert_eq!(at(text, 1, 5), "valid?");
        assert_eq!(at(text, 1, 12), "@bar");
        assert_eq!(at(text, 1, 18), "Baz::Qux");
        assert_eq!(at(text, 1, 16), "", "punctuation is no token");
    }

    #[test]
    fn a_token_never_runs_past_its_line() {
        assert_eq!(at("abc\ndef", 0, 3), "abc");
        assert_eq!(at("abc", 5, 0), "abc", "past the end clamps");
    }
}
