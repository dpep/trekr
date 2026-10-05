//! The per-site reference rule both fronts list by: a call site tiered
//! against the query, nothing ruled out on a partial index (DEC-320), and a
//! shared group's ruled-out call asked again of the groups that include it
//! (DEC-499).

use super::members::{CheckoutFiles, includer_reference};
use crate::core::{Call, Facts};
use crate::resolve::refs::{self, Query, Reference, Tier};
use crate::tree::Tree;

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
