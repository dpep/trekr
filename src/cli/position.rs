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
        .chain(facts.calls.iter().map(|c| (c.name.as_str(), c.pos)))
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
        .find(|c| covers(c.pos, c.name.len(), line, col))
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
    fn whitespace_is_not_a_fact() {
        assert!(at(b"class W\nend\n", 1, 1).is_none());
    }
}
