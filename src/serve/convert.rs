//! LSP's coordinates and ours.
//!
//! LSP counts lines from zero and characters in UTF-16 code units; this engine
//! counts lines from one and columns in bytes, because that is what `file:line:col`
//! means everywhere else. The conversion needs the line's text, so it lives
//! here rather than being guessed at each call site.

use crate::core::Pos;
use lsp_types::{Position, Range};
use std::path::PathBuf;

/// `file:///a/b.rb` → `/a/b.rb`. Percent-decoding only; no other scheme is
/// answerable, so anything else is `None` rather than a wrong path.
pub(crate) fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // A `file://host/path` form is not something a local editor sends.
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    Some(PathBuf::from(percent_decode(rest)))
}

pub(crate) fn path_to_uri(path: &std::path::Path) -> String {
    let mut out = String::from("file://");
    for byte in path.to_string_lossy().bytes() {
        match byte {
            b'/' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            b if b.is_ascii_alphanumeric() => out.push(b as char),
            b => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(byte) = u8::from_str_radix(&text[i + 1..i + 3], 16)
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// An LSP position in a document → our 1-based line and byte column.
pub(crate) fn to_pos(text: &str, position: Position) -> Pos {
    let line = text.lines().nth(position.line as usize).unwrap_or("");
    // `character` counts UTF-16 code units; walk them until the budget is
    // spent, which is exact for ASCII and correct for everything else.
    let mut utf16 = 0u32;
    let mut byte = 0usize;
    for ch in line.chars() {
        if utf16 >= position.character {
            break;
        }
        utf16 += ch.len_utf16() as u32;
        byte += ch.len_utf8();
    }
    Pos {
        line: position.line + 1,
        col: byte as u32 + 1,
    }
}

/// Our 1-based line and byte column → an LSP position. `text` is the file the
/// position is in, when we have it; without it the column is assumed ASCII,
/// which is right for the overwhelming majority of Ruby and never crashes.
pub(crate) fn to_position(text: Option<&str>, line: u32, col: u32) -> Position {
    let character = match text.and_then(|t| t.lines().nth(line.saturating_sub(1) as usize)) {
        Some(source) => {
            let byte = col.saturating_sub(1) as usize;
            source
                .char_indices()
                .take_while(|(index, _)| *index < byte)
                .map(|(_, ch)| ch.len_utf16() as u32)
                .sum()
        }
        None => col.saturating_sub(1),
    };
    Position {
        line: line.saturating_sub(1),
        character,
    }
}

/// A zero-width range at a position — what a "go here" answer needs.
pub(crate) fn point(text: Option<&str>, line: u32, col: u32) -> Range {
    let position = to_position(text, line, col);
    Range {
        start: position,
        end: position,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_path_through_a_uri() {
        let path = PathBuf::from("/tmp/a b/c.rb");
        let uri = path_to_uri(&path);
        assert!(uri.starts_with("file:///tmp/a%20b/"), "{uri}");
        assert_eq!(uri_to_path(&uri), Some(path));
    }

    #[test]
    fn refuses_a_scheme_it_cannot_answer_for() {
        assert_eq!(uri_to_path("untitled:Untitled-1"), None);
    }

    #[test]
    fn counts_characters_in_utf16_and_columns_in_bytes() {
        // `é` is one UTF-16 unit and two bytes, so the two coordinate systems
        // disagree from the second character onward.
        let text = "é = 1\n";
        let pos = to_pos(
            text,
            Position {
                line: 0,
                character: 2,
            },
        );
        assert_eq!((pos.line, pos.col), (1, 4));
        assert_eq!(
            to_position(Some(text), 1, 4),
            Position {
                line: 0,
                character: 2
            },
            "and back again"
        );
    }

    #[test]
    fn a_missing_line_falls_back_to_bytes_rather_than_failing() {
        assert_eq!(
            to_position(None, 3, 5),
            Position {
                line: 2,
                character: 4
            }
        );
    }
}
