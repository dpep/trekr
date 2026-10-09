//! The command line's side of a position: the `FILE:LINE:COL` argument, and
//! a variable's answer. What is under the cursor is [`crate::query::position`].

use std::num::NonZeroU32;

/// A `FILE:LINE:COL` argument. Lines and columns count from 1; a `FILE:LINE`
/// has no column, and the line chooses.
pub(crate) struct Spec {
    pub(crate) path: String,
    pub(crate) line: NonZeroU32,
    pub(crate) col: Option<NonZeroU32>,
}

impl Spec {
    /// `None` when the input is not position-shaped; an `Err` saying why when
    /// it is, but a 0 names no position.
    ///
    /// Windows drive letters are not a concern here, but a path *can* contain a
    /// colon, so the split is from the right and only the last two fields.
    pub(crate) fn parse(spec: &str) -> Option<Result<Spec, String>> {
        let (rest, last) = spec.rsplit_once(':')?;
        let last: u32 = last.parse().ok()?;
        // `FILE:LINE:COL`, when the field before the column is also a number.
        // An empty path there is malformed, not a two-field spec: `:1:2` must
        // stay a refusal rather than becoming the file `:1`.
        let (path, line, col) = match rest.rsplit_once(':') {
            Some((path, line)) if line.parse::<u32>().is_ok() => {
                if path.is_empty() {
                    return None;
                }
                (path, line.parse::<u32>().ok()?, Some(last))
            }
            // `FILE:LINE`, which is what a hand typing it produces.
            _ if rest.is_empty() => return None,
            _ => (rest, last, None),
        };
        let zero = || format!("`{spec}`: lines and columns count from 1, so 0 names no position");
        let Some(line) = NonZeroU32::new(line) else {
            return Some(Err(zero()));
        };
        let col = match col.map(NonZeroU32::new) {
            Some(None) => return Some(Err(zero())),
            Some(col) => col,
            None => None,
        };
        Some(Ok(Spec {
            path: path.to_string(),
            line,
            col,
        }))
    }
}

/// A variable under the cursor, answered from the file alone, the way the LSP
/// answers one (DEC-064): a local or parameter by the writes its read can see,
/// an instance or class variable by the writes to it in this file.
pub(crate) fn variable_at(
    source: &[u8],
    facts: &crate::core::Facts,
    path: &str,
    line: u32,
    col: u32,
) -> Option<serde_json::Value> {
    use crate::resolve::vars::{self, Binding, Sigil};
    let found = vars::of_file(source, &facts.strings);
    let under = crate::query::position::variable_at(facts, source, &found, line, col)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_position_even_when_the_path_has_a_colon() {
        let parsed = |s: &str| {
            Spec::parse(s)
                .and_then(Result::ok)
                .map(|spec| (spec.path, spec.line.get(), spec.col.map(NonZeroU32::get)))
        };
        let path = |p: &str| p.to_string();
        // (written, parsed)
        let cases = [
            ("a:b/c.rb:12:5", Some((path("a:b/c.rb"), 12, Some(5)))),
            // `FILE:LINE` is what a hand types; the line chooses.
            (
                "app/models/user.rb:42",
                Some((path("app/models/user.rb"), 42, None)),
            ),
            (
                "/tmp/a:b/user.rb:42",
                Some((path("/tmp/a:b/user.rb"), 42, None)),
            ),
            ("no-position.rb", None),
            ("app.rb", None),
            (":1:2", None),
            (":42", None),
        ];
        for (written, want) in cases {
            assert_eq!(parsed(written), want, "{written}");
        }

        // Zero is a position shape, so it is refused as one rather than
        // dispatched elsewhere.
        let zero = |s: &str| matches!(Spec::parse(s), Some(Err(_)));
        assert!(zero("a.rb:0:0") && zero("a.rb:0") && zero("a.rb:3:0"));
        assert!(!zero("a.rb:3") && !zero("a.rb:3:1"));
    }
}
