//! The command line's side of a position: the `FILE:LINE:COL` argument, and
//! a variable's answer. What is under the cursor is [`crate::query::position`].

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

/// A variable under the cursor, answered from the file alone, the way the LSP
/// answers one (DEC-064): a local or parameter by the writes its read can see,
/// an instance or class variable by the writes to it in this file.
pub(crate) fn variable_at(
    source: &[u8],
    path: &str,
    line: u32,
    col: u32,
) -> Option<serde_json::Value> {
    use crate::resolve::vars::{self, Binding, Sigil};
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
}
