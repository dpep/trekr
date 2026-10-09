//! Position → fact. What is under the cursor?
//!
//! A single-file Prism reparse (PLAN §4) rather than a stored-span lookup: the
//! file on disk may be newer than the index, and reparsing one file costs
//! microseconds. The facts it produces are the same ones the blob layer stores,
//! so this shares the extractor rather than growing a second idea of what a
//! constant is.

use crate::core::{Call, ConstRef, Def, Kind, Pos};
use crate::resolve::vars::{Occurrence, Vars};
use std::num::NonZeroU32;

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

/// Where a definition's own name is written. A compact `class A::B` is
/// recorded at the start of its path, but the name it opens is `B`, at the end.
fn name_pos(def: &Def) -> Pos {
    let shift = match def.kind {
        Kind::Class | Kind::Module => def.name.len() - tail(&def.name),
        _ => 0,
    };
    Pos {
        line: def.pos.line,
        col: def.pos.col + shift as u32,
    }
}

/// How many columns from `name_pos` the definition's name spans. A macro's
/// def is recorded at its symbol, so the `:` comes first.
fn name_len(def: &Def) -> usize {
    tail(&def.name) + usize::from(def.via.is_some())
}

/// On the `A` of a compact `class A::B`: the namespace it is opened in,
/// answered as the reference it is. The extractor records the path whole, so
/// the segment is read back out of the definition's name.
fn compact_prefix(def: &Def, line: u32, col: u32) -> Option<ConstRef> {
    if !matches!(def.kind, Kind::Class | Kind::Module) || def.pos.line != line {
        return None;
    }
    let prefix = def.name.len() - tail(&def.name);
    let offset = col.checked_sub(def.pos.col)? as usize;
    if prefix == 0 || offset >= prefix {
        return None;
    }
    let end = def
        .name
        .get(offset..)?
        .find("::")
        .map_or(prefix, |i| offset + i);
    Some(ConstRef {
        name: def.name.get(..end)?.to_string(),
        nesting: def.nesting.clone(),
        pos: def.pos,
    })
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
        .map(|d| (d.name.as_str(), name_pos(d)))
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
    col: Option<NonZeroU32>,
) -> Option<(Under, Option<Snapped>)> {
    if let Some(col) = col
        && let Some(under) = at_facts(facts, line, col.get())
    {
        return Some((under, None));
    }
    let names = names_on_line(facts, line);
    // Nearest by column, leftmost on a tie — so a bare `FILE:LINE` takes the
    // first name on the line. Only *interesting* names are recorded, so
    // `w = Widget.new` snaps to `Widget` rather than to the local `w`.
    let from = col.map_or(0, NonZeroU32::get);
    let (name, at_col) = names
        .iter()
        .min_by_key(|(_, c)| (c.abs_diff(from), *c))?
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
        .find(|d| covers(name_pos(d), name_len(d), line, col) && d.kind != Kind::Constant)
    {
        return Some(Under::Definition(def.clone()));
    }
    if let Some(reference) = facts.defs.iter().find_map(|d| compact_prefix(d, line, col)) {
        return Some(Under::Constant(reference));
    }
    // The literal `include_context` is handed names a shared group (DEC-124).
    if let Some((pos, _, module)) = facts
        .shared_names
        .iter()
        .find(|(pos, len, _)| covers(*pos, *len as usize, line, col))
    {
        return Some(Under::Constant(ConstRef {
            name: module.clone(),
            nesting: Vec::new(),
            pos: *pos,
        }));
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
        .find(|d| covers(name_pos(d), name_len(d), line, col))
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

/// The template a `render` or `extends` names at a position (DEC-524). A
/// definition asked there opens it, before the variable it may be written as.
pub(crate) fn template_at(
    facts: &crate::core::Facts,
    line: u32,
    col: u32,
) -> Option<&crate::core::TemplateRef> {
    facts
        .templates
        .iter()
        .find(|t| covers(t.pos, t.len as usize, line, col))
}

/// The variable a character names: the one written over it, or one it is
/// just past where no name is written there. On `x[0]`'s `[` that is the
/// `[]` call, though `x` ends there; on the `n` of a `def #{n}_x` a string
/// makes, it is `n` (DEC-036 addendum).
pub(crate) fn variable_at<'v>(
    facts: &crate::core::Facts,
    source: &[u8],
    vars: &'v Vars,
    line: u32,
    col: u32,
) -> Option<&'v Occurrence> {
    let offset = offset_of(source, line, col)?;
    let found = vars.at(offset)?;
    (offset < found.span.end || at_facts(facts, line, col).is_none()).then_some(found)
}

/// The character an editor's caret reads, given the one to its right: that
/// one, unless the caret is just past a variable, which it reads on its last
/// character. At an identifier's boundary the identifier wins, as in
/// rust-analyzer, so `a+b` and `list[0]` read `a` and `list` with the caret
/// after them (DEC-036 addendum). A `FILE:LINE:COL` names a character and is
/// not read.
pub(crate) fn caret_reads(source: &[u8], vars: &Vars, right: Pos) -> Pos {
    let Some(offset) = offset_of(source, right.line, right.col) else {
        return right;
    };
    match vars.at(offset) {
        Some(found) if found.span.end == offset && found.span.start < offset => {
            // The last character's first byte: back over UTF-8 continuations.
            let last = (found.span.start..offset)
                .rev()
                .find(|&i| source[i] & 0xC0 != 0x80)
                .unwrap_or(found.span.start);
            crate::extract::LineIndex::new(source).pos(last)
        }
        _ => right,
    }
}

/// The byte offset of a 1-based line and column, clamped as the editor's
/// are: a column past the end of its line is the line's end, never a byte of
/// a later line. `None` past the last line, or at column 0.
pub(crate) fn offset_of(source: &[u8], line: u32, col: u32) -> Option<usize> {
    let mut lines = source.split_inclusive(|b| *b == b'\n');
    let start: usize = lines
        .by_ref()
        .take(line.checked_sub(1)? as usize)
        .map(<[u8]>::len)
        .sum();
    let text = lines.next()?;
    let len = text.strip_suffix(b"\n").unwrap_or(text).len();
    Some(start + (col as usize).checked_sub(1)?.min(len))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_caret_just_past_a_variable_reads_its_last_character() {
        let source = "list = [1]\nlist[0]\na = b = 1\na+b\nhé = 1\nhé + 1\n".as_bytes();
        let vars = crate::resolve::vars::analyze(source);
        let at = |line, col| Pos { line, col };
        // (the character to the caret's right, what it reads)
        let cases = [
            (at(2, 5), at(2, 4)), // `list|[`: the `t`
            (at(2, 1), at(2, 1)), // `|list`: the `l`
            (at(2, 3), at(2, 3)), // `li|st`: inside, the `s`
            (at(4, 2), at(4, 1)), // `a|+b`: the `a`
            (at(4, 4), at(4, 3)), // `a+b|`: past the line's end, the `b`
            (at(6, 4), at(6, 2)), // `hé| + 1`: the `é`, two bytes back
            (at(9, 1), at(9, 1)), // past the last line: as given
        ];
        for (right, reads) in cases {
            assert_eq!(caret_reads(source, &vars, right), reads, "{right:?}");
        }
    }

    #[test]
    fn a_variable_answers_where_written_and_where_nothing_follows_it() {
        let source = b"x = [1]\nx[0]\nx.size\n-x\n";
        let facts = crate::extract::extract(source);
        let vars = crate::resolve::vars::analyze(source);
        // (line, col, does `x` answer)
        let cases = [
            (2, 1, true),  // on `x`
            (2, 2, false), // `[`: the `[]` call, though the column touches `x`
            (3, 2, true),  // `.`: no name starts there
            (3, 3, false), // `size`
            (4, 1, false), // `-`: the unary `-@` call
            (4, 2, true),  // `x`: `-@` is written as one character
        ];
        for (line, col, want) in cases {
            let found = variable_at(&facts, source, &vars, line, col).is_some();
            assert_eq!(found, want, "{line}:{col}");
        }
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
    fn a_macros_symbol_reads_as_the_definition_it_makes() {
        let source = b"class W\n  attr_reader :count\n  alias_method :size?, :count\nend\n";
        for (line, col, name) in [(2, 15, "count"), (2, 20, "count"), (3, 19, "size?")] {
            let Some(Under::Definition(def)) = at(source, line, col) else {
                panic!("expected a definition at {line}:{col}");
            };
            assert_eq!(def.name, name);
        }
    }

    #[test]
    fn a_compact_path_answers_each_segment_for_what_it_is() {
        let source = b"class Outer::Mid::Inner\nend\nmodule Outer::Mixin\nend\n";
        // `Inner`: the class being opened.
        let Some(Under::Definition(def)) = at(source, 1, 19) else {
            panic!("expected the class definition");
        };
        assert_eq!(def.name, "Outer::Mid::Inner");
        let Some(Under::Definition(def)) = at(source, 3, 15) else {
            panic!("expected the module definition");
        };
        assert_eq!(def.name, "Outer::Mixin");
        // `Outer` and `Mid`: the namespaces it is opened in, as references.
        for (col, name) in [(7, "Outer"), (14, "Outer::Mid")] {
            let Some(Under::Constant(reference)) = at(source, 1, col) else {
                panic!("expected a reference at column {col}");
            };
            assert_eq!(reference.name, name);
        }
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

    #[test]
    fn a_column_past_its_line_is_the_lines_end() {
        let source = b"ab\ncount = 1\nlast";
        for (line, col, offset) in [
            (1, 1, Some(0)),
            (1, 2, Some(1)),
            (1, 99, Some(2)),
            (2, 99, Some(12)),
            (3, 99, Some(17)),
            (4, 1, None),
            (1, 0, None),
        ] {
            assert_eq!(offset_of(source, line, col), offset, "{line}:{col}");
        }
    }
}
