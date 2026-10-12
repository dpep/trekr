//! The reference rules both fronts list by: a call site tiered against the
//! query, nothing ruled out on a partial index (DEC-320), and a shared
//! group's ruled-out call asked again of the groups that include it
//! (DEC-499); and a class, module or constant's mentions, by Ruby's lookup.

use super::members::{CheckoutFiles, includer_reference};
use crate::core::{Call, Facts};
use crate::resolve::refs::{self, Query, Reference, Tier};
use crate::store::{Ref, Store};
use crate::tree::Tree;
use std::collections::HashMap;

/// The indexed mentions of the class, module or constant `fqn` in the
/// checkout at `root`: its definitions and constant references that Ruby's
/// lookup resolves to it.
///
/// The index records a mention by the name written (`Base`,
/// `ActiveRecord::Base`), so the rows asked for are those that could be it,
/// and each is named in the nesting it was written in ([`placed_name`]):
/// same-named constants elsewhere are named otherwise and drop out, which is
/// the point. A definition is written as one of the name's [`spellings`]; a
/// reference may reach it through any scope's ancestors (`Leaf::KC` for
/// `Kernel::KC`), so every reference ending in its last segment is placed.
pub(crate) fn constant_mentions(
    tree: &Tree,
    store: &Store,
    root: &str,
    fqn: &str,
) -> anyhow::Result<Vec<Ref>> {
    let fqn = &crate::tree::whole_name(fqn);
    let mut found = Vec::new();
    for written in &spellings(fqn) {
        found.extend(
            store
                .refs(root, written)?
                .into_iter()
                .filter(|row| {
                    row.role == "definition"
                        && matches!(row.kind.as_deref(), Some("class" | "module" | "constant"))
                })
                .filter(|row| names_constant(tree, written, &row.nesting, &row.path, fqn)),
        );
    }
    let leaf = fqn.rsplit("::").next().unwrap_or(fqn);
    // A common leaf (`Base`) is written the same way, in the same nesting,
    // many times a file: each is placed once.
    let mut placed: HashMap<(String, Vec<String>, String), bool> = HashMap::new();
    for (written, row) in store.const_refs_ending(root, leaf)? {
        let names = *placed
            .entry((written, row.nesting.clone(), row.path.clone()))
            .or_insert_with_key(|(written, nesting, path)| {
                names_constant(tree, written, nesting, path, fqn)
            });
        if names {
            found.push(row);
        }
    }
    found.sort_by(|a, b| (&a.path, a.line, a.col).cmp(&(&b.path, b.line, b.col)));
    Ok(found)
}

/// The ways a reference can be written that name the constant `fqn` from
/// some scope: each suffix of its whole name, and the whole name from the
/// top level, rooted or through Object, whose constants are the top level's
/// (`tree::qualify`). Asked of the index, which records a reference as
/// written; each row found is then placed to see whether it is `fqn`.
pub(crate) fn spellings(fqn: &str) -> Vec<String> {
    let fqn = crate::tree::whole_name(fqn);
    let mut found: Vec<String> = std::iter::once(fqn.as_str())
        .chain(fqn.match_indices("::").map(|(at, _)| &fqn[at + 2..]))
        .map(str::to_string)
        .collect();
    found.extend([
        format!("::{fqn}"),
        format!("Object::{fqn}"),
        format!("::Object::{fqn}"),
    ]);
    found
}

/// Whether a constant written as `written` in `nesting`, in the file at
/// `path`, is `fqn`: whether that is its [`placed_name`].
pub(crate) fn names_constant(
    tree: &Tree,
    written: &str,
    nesting: &[String],
    path: &str,
    fqn: &str,
) -> bool {
    placed_name(tree, written, nesting, path) == fqn
}

/// The whole name of a constant written as `written` in `nesting`, in the
/// file at `path`, as far as the index can place it: its own, else the
/// longest leading part's plus the rest as written (`Rack::Utils` inside
/// `module App` is `App::Rack::Utils` once `App::Rack` resolves, though
/// nothing defines `Utils`), else the name as written, from the top.
pub(crate) fn placed_name(tree: &Tree, written: &str, nesting: &[String], path: &str) -> String {
    if let Some(found) = tree.resolve_at(written, nesting, path).fqn {
        return found;
    }
    let bare = written.strip_prefix("::").unwrap_or(written);
    let rooted = if written.starts_with("::") { "::" } else { "" };
    let segments: Vec<&str> = bare.split("::").collect();
    (1..segments.len())
        .rev()
        .find_map(|n| {
            let prefix = segments[..n].join("::");
            let placed = tree
                .resolve_at(&format!("{rooted}{prefix}"), nesting, path)
                .fqn?;
            Some(crate::tree::qualify(&placed, &segments[n..].join("::")))
        })
        .unwrap_or_else(|| crate::tree::whole_name(bare))
}

/// One call site, tiered. Reads only the tree and the file's own facts, so
/// it runs on any worker.
pub(crate) fn tier(
    tree: &Tree,
    facts: &Facts,
    call: &Call,
    path: &str,
    query: &Query,
    target: Option<&str>,
    partial: bool,
) -> Reference {
    let mut reference = refs::tier_call(tree, facts, call, path, query, target);
    if partial {
        reference.unrule();
    }
    reference
}

/// Whether [`rescue`] may change this site's answer: a call in a shared
/// group's body, ruled out for a query that names its owner.
pub(crate) fn rescuable(
    facts: &Facts,
    call: &Call,
    target: Option<&str>,
    reference: &Reference,
) -> bool {
    reference.tier == Tier::Excluded
        && target.is_some()
        && crate::resolve::members::in_shared_body(facts, call)
}

/// A [`rescuable`] site, answered by its includers where one mixes in the
/// target. It reads other files through `files`, which is not `Sync`: run it
/// where the fronts merge, one site at a time.
pub(crate) fn rescue(
    tree: &Tree,
    files: &CheckoutFiles<'_>,
    call: &Call,
    target: &str,
    reference: &mut Reference,
) {
    if let Some(rescued) = includer_reference(tree, files, call, target, reference) {
        *reference = rescued;
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_name_is_spelled_by_each_suffix_and_from_the_top_level() {
        assert_eq!(
            super::spellings("A::B::C"),
            [
                "A::B::C",
                "B::C",
                "C",
                "::A::B::C",
                "Object::A::B::C",
                "::Object::A::B::C"
            ]
            .map(String::from)
        );
        assert_eq!(super::spellings("Object::X"), super::spellings("X"));
    }
}
