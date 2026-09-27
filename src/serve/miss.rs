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

use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

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
    /// them, so a miss pastes straight into the CLI.
    pub(crate) fn event(&self, text: Option<&str>) -> serde_json::Value {
        let pos = text.map(|t| super::convert::to_pos(t, self.position));
        serde_json::json!({
            "op": self.op,
            "file": self.path.to_string_lossy(),
            "line": self.position.line + 1,
            "col": pos.map_or(self.position.character + 1, |p| p.col),
            "token": text.map(|t| token_at(t, self.position)).unwrap_or_default(),
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

/// One recorded miss, as `--usage --misses` shows it.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct Recorded {
    pub(crate) ts: String,
    pub(crate) op: String,
    pub(crate) file: String,
    pub(crate) line: u64,
    pub(crate) col: u64,
    #[serde(default)]
    pub(crate) token: String,
    pub(crate) outcome: String,
    #[serde(default)]
    pub(crate) why: Option<String>,
}

/// How much of the log's tail is read. The log is append-only and a month of
/// daily use is a couple of MB; the recent misses are what anyone acts on.
const TAIL_BYTES: u64 = 8 << 20;

/// The misses in the log's tail, oldest first, at or after `since` (an ISO
/// timestamp prefix, compared as text). A log that does not exist yet is no
/// misses.
pub(crate) fn read(log: &Path, since: Option<&str>) -> std::io::Result<Vec<Recorded>> {
    let mut file = match std::fs::File::open(log) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let len = file.metadata()?.len();
    let skip = len.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(skip))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    // Started mid-file: the first line is a fragment.
    let lines = text.lines().skip(usize::from(skip > 0));
    Ok(lines
        .filter(|line| line.contains(r#""event":"miss""#))
        .filter_map(|line| serde_json::from_str::<Recorded>(line).ok())
        .filter(|miss| since.is_none_or(|since| miss.ts.as_str() >= since))
        .collect())
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

    #[test]
    fn reads_only_miss_events_since_a_time() {
        let dir = std::env::temp_dir().join(format!("trekr-miss-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let log = dir.join("lsp.log");
        let line = |ts: &str, event: &str| {
            format!(
                r#"{{"ts":"{ts}","event":"{event}","op":"definition","file":"/a.rb","line":3,"col":5,"token":"foo","outcome":"empty","why":null}}"#
            )
        };
        let text = [
            line("2026-01-01T00:00:00.000Z", "miss"),
            line("2026-01-02T00:00:00.000Z", "request"),
            line("2026-01-03T00:00:00.000Z", "miss"),
        ]
        .join("\n");
        std::fs::write(&log, text).unwrap();
        let misses = read(&log, Some("2026-01-02")).unwrap();
        assert_eq!(misses.len(), 1);
        assert_eq!(misses[0].ts, "2026-01-03T00:00:00.000Z");
        assert!(read(&dir.join("absent.log"), None).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
