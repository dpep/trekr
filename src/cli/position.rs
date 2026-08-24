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
        let (rest, col) = spec.rsplit_once(':')?;
        let (path, line) = rest.rsplit_once(':')?;
        if path.is_empty() {
            return None;
        }
        Some(Spec {
            path: path.to_string(),
            line: line.parse().ok()?,
            col: col.parse().ok()?,
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
pub(crate) fn at(source: &[u8], line: u32, col: u32) -> Option<Under> {
    let facts = crate::extract::extract(source);

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
