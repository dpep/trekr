//! Position → fact. What is under the cursor?
//!
//! A single-file Prism reparse (PLAN §4) rather than a stored-span lookup: the
//! file on disk may be newer than the index, and reparsing one file costs
//! microseconds. The facts it produces are the same ones the blob layer stores,
//! so this shares the extractor rather than growing a second idea of what a
//! constant is.

use crate::core::{Call, ConstRef, Def, Kind, Pos};

/// A `FILE:LINE:COL` argument.
pub(crate) struct Spec {
    pub(crate) path: String,
    pub(crate) line: u32,
    pub(crate) col: u32,
}

impl Spec {
    /// Windows drive letters are not a concern here, but a path *can* contain a
    /// colon, so the split is from the right and only the last two fields.
    pub(crate) fn parse(spec: &str) -> Option<Spec> {
        let (rest, last) = spec.rsplit_once(':')?;
        let last: u32 = last.parse().ok()?;
        // `FILE:LINE:COL`, when the field before the column is also a number.
        // An empty path there is malformed, not a two-field spec: `:1:2` must
        // stay a refusal rather than becoming the file `:1`.
        if let Some((path, line)) = rest.rsplit_once(':')
            && let Ok(line) = line.parse::<u32>()
        {
            return (!path.is_empty()).then(|| Spec {
                path: path.to_string(),
                line,
                col: last,
            });
        }
        // `FILE:LINE`, which is what a hand typing it produces. Columns are
        // 1-based, so 0 means "not given" and the line gets to choose.
        if rest.is_empty() {
            return None;
        }
        Some(Spec {
            path: rest.to_string(),
            line: last,
            col: 0,
        })
    }

    /// Why a position-shaped input names no position, if it does not: lines
    /// and columns count from 1. `FILE:LINE` leaves the column 0 internally,
    /// so only a written `:0` column is refused.
    pub(crate) fn out_of_range(&self, written: &str) -> Option<String> {
        let col_written = written
            .rsplit_once(':')
            .and_then(|(rest, _)| rest.rsplit_once(':'))
            .is_some_and(|(_, line)| line.parse::<u32>().is_ok());
        (self.line == 0 || (col_written && self.col == 0))
            .then(|| format!("`{written}`: lines and columns count from 1, so 0 names no position"))
    }
}

/// What the cursor is on. Ordered by how much this engine can say about it.
pub(crate) enum Under {
    /// A class, module, or method definition — it *is* the answer.
    Definition(Def),
    /// A constant reference, which the tree layer can resolve exactly.
    Constant(ConstRef),
    /// A method call. Receiver shape is recorded, but narrowing it needs the
    /// method ladder, so this is honest residue for now.
    Call(Call),
}

/// Does a name starting at `pos` and `len` bytes long cover `(line, col)`?
///
/// Columns are 1-based and byte-oriented, matching what the extractor records.
fn covers(pos: Pos, len: usize, line: u32, col: u32) -> bool {
    pos.line == line && col >= pos.col && col < pos.col + len as u32
}

/// The last segment of a written constant path is what sits at its position:
/// `A::B` is recorded at `B`'s offset, so `B` is what the cursor can be on.
fn tail(name: &str) -> usize {
    name.rsplit("::").next().unwrap_or(name).len()
}

/// The innermost fact at a position, preferring the most specific reading.
///
/// Production goes through `at_or_snap`, which falls back to the nearest name
/// on the line; this is the exact-only reading, kept for the tests that pin it.
#[cfg(test)]
fn at(source: &[u8], line: u32, col: u32) -> Option<Under> {
    at_facts(&crate::extract::extract(source), line, col)
}

/// A name the query did not land on exactly, and where it really is.
pub(crate) struct Snapped {
    pub(crate) name: String,
    pub(crate) col: u32,
    /// The other names on that line, so a re-query can be exact.
    pub(crate) alternatives: Vec<(String, u32)>,
}

/// The written last segment: `A::B` sits at `B`, so `B` is what a reader sees.
fn last_segment(name: &str) -> &str {
    name.rsplit("::").next().unwrap_or(name)
}

/// Every name on a line, left to right, with the column it starts at.
fn names_on_line(facts: &crate::core::Facts, line: u32) -> Vec<(String, u32)> {
    let mut found: Vec<(String, u32)> = Vec::new();
    for (name, pos) in facts
        .defs
        .iter()
        .map(|d| (d.name.as_str(), d.pos))
        .chain(facts.const_refs.iter().map(|r| (r.name.as_str(), r.pos)))
        .chain(facts.calls.iter().map(|c| (c.written_name(), c.pos)))
    {
        if pos.line == line {
            found.push((last_segment(name).to_string(), pos.col));
        }
    }
    found.sort_by_key(|(_, col)| *col);
    found.dedup_by(|a, b| a.1 == b.1);
    found
}

/// The position asked for, or the nearest name on the same line.
///
/// A column typed by hand is a guess, and landing one character into the
/// whitespace beside a method used to answer "nothing at that position" — a
/// true statement that helps nobody. Bounded to the line, because snapping
/// across lines answers a different question than the one asked, and always
/// disclosed: an answer about a name the caller did not type has to say so.
pub(crate) fn at_or_snap(
    facts: &crate::core::Facts,
    line: u32,
    col: u32,
) -> Option<(Under, Option<Snapped>)> {
    if col > 0
        && let Some(under) = at_facts(facts, line, col)
    {
        return Some((under, None));
    }
    let names = names_on_line(facts, line);
    // Nearest by column, leftmost on a tie — so a bare `FILE:LINE` (column 0)
    // takes the first name on the line. Only *interesting* names are recorded,
    // so `w = Widget.new` snaps to `Widget` rather than to the local `w`.
    let (name, at_col) = names
        .iter()
        .min_by_key(|(_, c)| (c.abs_diff(col), *c))?
        .clone();
    let under = at_facts(facts, line, at_col)?;
    let alternatives = names
        .iter()
        .filter(|(_, c)| *c != at_col)
        .cloned()
        .collect();
    Some((
        under,
        Some(Snapped {
            name,
            col: at_col,
            alternatives,
        }),
    ))
}

/// A variable under the cursor, answered from the file alone, the way the LSP
/// answers one (DEC-064): a local or parameter by the writes its read can see,
/// an instance or class variable by the writes to it in this file.
pub(crate) fn variable_at(
    source: &[u8],
    path: &str,
    line: u32,
    col: u32,
) -> Option<serde_json::Value> {
    use crate::serve::vars::{self, Binding, Sigil};
    let start = source
        .split_inclusive(|b| *b == b'\n')
        .take(line.checked_sub(1)? as usize)
        .map(<[u8]>::len)
        .sum::<usize>();
    let offset = start + (col as usize).checked_sub(1)?;
    let found = vars::analyze(source);
    let under = found.at(offset)?;
    let writes: Vec<&vars::Occurrence> = match under.sigil {
        Sigil::Local => found.local_definitions(under),
        Sigil::Instance | Sigil::Class => found
            .same(under)
            .into_iter()
            .filter(|o| o.is_write())
            .collect(),
    };
    let variable = match under.sigil {
        Sigil::Local
            if !writes.is_empty()
                && writes
                    .iter()
                    .all(|w| matches!(w.write, Some(Binding::Param | Binding::BlockParam))) =>
        {
            "parameter"
        }
        Sigil::Local => "local",
        Sigil::Instance => "ivar",
        Sigil::Class => "cvar",
    };
    let lines = crate::extract::LineIndex::new(source);
    let sites: Vec<serde_json::Value> = writes
        .iter()
        .map(|w| {
            let at = lines.pos(w.span.start);
            serde_json::json!({
                "path": path, "line": at.line, "col": at.col,
                "kind": w.write.map_or("assigned", Binding::describe),
            })
        })
        .collect();
    let mut answer = serde_json::json!({
        "under": "variable",
        "variable": variable,
        "name": under.name,
        "status": if sites.is_empty() { "residue" } else { "resolved" },
        "confidence": if sites.is_empty() { 0.0 } else { 1.0 },
        "resolved_via": "flow",
        "definition": sites,
    });
    let reason = match (under.sigil, writes.is_empty()) {
        (Sigil::Local, true) => Some("no write to this local reaches here"),
        (Sigil::Local, false) => None,
        // The class's other files are the LSP's to read; say where we looked.
        (_, true) => Some("not set in this file; its class's other files were not searched"),
        (_, false) => Some("writes in this file; its class's other files were not searched"),
    };
    if let (Some(reason), Some(object)) = (reason, answer.as_object_mut()) {
        object.insert("reason".into(), reason.into());
    }
    Some(answer)
}

/// The identifier the cursor is on, if it is on one. 1-based byte columns.
pub(crate) fn word_at(source: &[u8], line: u32, col: u32) -> Option<String> {
    let text = source
        .split(|b| *b == b'\n')
        .nth(line.checked_sub(1)? as usize)?;
    let at = (col as usize).checked_sub(1)?;
    let is_word = |b: &u8| b.is_ascii_alphanumeric() || *b == b'_' || *b >= 0x80;
    if !text.get(at).is_some_and(is_word) {
        return None;
    }
    let start = text[..at]
        .iter()
        .rposition(|b| !is_word(b))
        .map_or(0, |i| i + 1);
    let end = text[at..]
        .iter()
        .position(|b| !is_word(b))
        .map_or(text.len(), |i| at + i);
    String::from_utf8(text[start..end].to_vec()).ok()
}

/// The same, against facts already parsed — which a resident front has, and a
/// one-shot CLI invocation does not.
pub(crate) fn at_facts(facts: &crate::core::Facts, line: u32, col: u32) -> Option<Under> {
    // A definition's own name wins over anything else at the same spot: on
    // `class Widget` the cursor is on the declaration, not on a reference.
    if let Some(def) = facts
        .defs
        .iter()
        .find(|d| covers(d.pos, tail(&d.name), line, col) && d.kind != Kind::Constant)
    {
        return Some(Under::Definition(def.clone()));
    }
    // Longest name wins among constants: on the `B` of `A::B` both `A::B` and a
    // bare `B` may be recorded, and the qualified one is what was written.
    if let Some(reference) = facts
        .const_refs
        .iter()
        .filter(|r| covers(r.pos, tail(&r.name), line, col))
        .max_by_key(|r| r.name.len())
    {
        return Some(Under::Constant(reference.clone()));
    }
    if let Some(def) = facts
        .defs
        .iter()
        .find(|d| covers(d.pos, tail(&d.name), line, col))
    {
        return Some(Under::Definition(def.clone()));
    }
    facts
        .calls
        .iter()
        .find(|c| covers(c.pos, c.written_len(), line, col))
        .cloned()
        .map(Under::Call)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_position_even_when_the_path_has_a_colon() {
        let spec = Spec::parse("a:b/c.rb:12:5").expect("parses");
        assert_eq!(
            (spec.path.as_str(), spec.line, spec.col),
            ("a:b/c.rb", 12, 5)
        );
        assert!(Spec::parse("no-position.rb").is_none());
        assert!(Spec::parse(":1:2").is_none());

        // `FILE:LINE` is what a hand types; column 0 means "the line chooses".
        let bare = Spec::parse("app/models/user.rb:42").unwrap();
        assert_eq!(bare.path, "app/models/user.rb");
        assert_eq!((bare.line, bare.col), (42, 0));
        let colonic = Spec::parse("/tmp/a:b/user.rb:42").unwrap();
        assert_eq!(colonic.path, "/tmp/a:b/user.rb");
        assert_eq!((colonic.line, colonic.col), (42, 0));
        assert!(Spec::parse("app.rb").is_none());

        // Zero is a position shape, so it is refused as one rather than
        // dispatched elsewhere; a line-only spec's column is not a written 0.
        let zero = |s: &str| Spec::parse(s).unwrap().out_of_range(s).is_some();
        assert!(zero("a.rb:0:0") && zero("a.rb:0") && zero("a.rb:3:0"));
        assert!(!zero("a.rb:3") && !zero("a.rb:3:1"));
        assert!(Spec::parse(":42").is_none());
    }

    #[test]
    fn finds_the_qualified_constant_rather_than_its_last_segment() {
        let source = b"module N\n  X = Foo::Bar\nend\n";
        // Column of `Bar` within `  X = Foo::Bar`.
        let Some(Under::Constant(reference)) = at(source, 2, 13) else {
            panic!("expected a constant under the cursor");
        };
        assert_eq!(reference.name, "Foo::Bar");
        assert_eq!(reference.nesting, ["N"]);
    }

    #[test]
    fn a_definitions_own_name_reads_as_the_definition_not_a_reference() {
        let source = b"class Widget\nend\n";
        let Some(Under::Definition(def)) = at(source, 1, 7) else {
            panic!("expected a definition");
        };
        assert_eq!(def.name, "Widget");
    }

    #[test]
    fn a_method_call_is_found_and_carries_its_receiver_shape() {
        let source = b"class W\n  def go\n    helper\n  end\nend\n";
        let Some(Under::Call(call)) = at(source, 3, 5) else {
            panic!("expected a call");
        };
        assert_eq!(call.name, "helper");
        assert_eq!(call.recv, crate::core::RecvShape::Implicit);
    }

    #[test]
    fn the_word_under_the_cursor_is_read_whole() {
        let source = b"x = 5\n  normalize(super(value))\n";
        assert_eq!(word_at(source, 2, 15).as_deref(), Some("super"));
        assert_eq!(word_at(source, 2, 5).as_deref(), Some("normalize"));
        assert_eq!(word_at(source, 1, 2), None, "whitespace");
        assert_eq!(word_at(source, 9, 1), None, "past the end");
    }

    #[test]
    fn whitespace_is_not_a_fact() {
        assert!(at(b"class W\nend\n", 1, 1).is_none());
    }
}
