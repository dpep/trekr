//! A variable's mentions, beneath both fronts. A local's never leave its
//! file; an instance or class variable belongs to an object, so its mentions
//! are looked for in every file that opens its class and its ancestors, as
//! far as the tree resolves them (DEC-064). Each front reads those files its
//! own way — the editor's open buffers, the CLI's disk.

use crate::resolve::vars::{Occurrence, Owner, Sigil, Vars};
use crate::tree::Tree;
use std::collections::HashSet;

/// How many files are read for one member. A class reopened across more than
/// this — a core extension, a god object — is read in the tree's site order
/// and cut there; a mention among the rest is not found.
pub(crate) const MAX_FILES: usize = 64;

/// The class a member belongs to, and the files that may mention it.
pub(crate) struct ClassScope {
    /// The file at hand, then each other file that opens the class or an
    /// ancestor, as the tree spells its path.
    pub(crate) files: Vec<String>,
    chain: HashSet<String>,
    singleton: bool,
}

impl ClassScope {
    /// `None` when the class cannot be named — a top-level ivar, a template's,
    /// a nesting the tree does not place — and the file at hand is all there is.
    pub(crate) fn of(tree: &Tree, here: &str, owner: &Owner, sigil: Sigil) -> Option<ClassScope> {
        if owner.nesting.is_empty() {
            return None;
        }
        let fqn = tree.scope_fqn(&owner.nesting)?;
        tree.kind_of(&fqn)?;
        // A class-level ivar belongs to that one class object; an instance's,
        // or a class variable, is shared down the ancestry.
        let chain: Vec<String> = match sigil == Sigil::Instance && owner.singleton {
            true => vec![fqn],
            false => tree.ancestors(&fqn).chain.clone(),
        };
        let mut files = vec![here.to_string()];
        for name in &chain {
            for site in tree.sites(name) {
                if files.len() >= MAX_FILES || !tree.in_checkout(&site.path) {
                    continue;
                }
                if !files.contains(&site.path) {
                    files.push(site.path);
                }
            }
        }
        Some(ClassScope {
            files,
            chain: chain.into_iter().collect(),
            singleton: owner.singleton,
        })
    }

    /// Does a nesting written in one of the files place in the class's chain?
    pub(crate) fn holds(&self, tree: &Tree, nesting: &[String]) -> bool {
        tree.scope_fqn(nesting)
            .is_some_and(|fqn| self.chain.contains(&fqn))
    }

    /// The mentions of `want` among one file's variables, given which of the
    /// file's nestings are the class's (`inside`).
    pub(crate) fn mentions<'a>(
        &self,
        vars: &'a Vars,
        want: &Occurrence,
        inside: impl Fn(&[String]) -> bool,
    ) -> Vec<&'a Occurrence> {
        vars.occurrences
            .iter()
            .filter(|o| o.sigil == want.sigil && o.name == want.name)
            .filter(|o| {
                vars.owner(o).is_some_and(|theirs| {
                    theirs.singleton == self.singleton && inside(&theirs.nesting)
                })
            })
            .collect()
    }
}
