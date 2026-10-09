//! What both fronts ask of an example group's members over a checkout:
//! the checkout's files as `resolve::members` reads them, the member at a
//! position, and an includer's answer for a call (DEC-490).

use crate::core::{Def, Facts};
use crate::resolve::members::{self, Asked, Context, Files};
use crate::store::Store;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

/// The checkout's files, each parsed once: an editor's open copy where it
/// has one, else the disk's.
pub(crate) struct CheckoutFiles<'a> {
    store: &'a Store,
    root: &'a Path,
    root_str: &'a str,
    /// Open buffers' text, by checkout-relative path.
    open: HashMap<String, String>,
    held: RefCell<HashMap<String, Option<Arc<Facts>>>>,
    /// Whether each file's text can open an example group.
    groups: RefCell<HashMap<String, bool>>,
}

impl<'a> CheckoutFiles<'a> {
    pub(crate) fn new(store: &'a Store, root: &'a Path, root_str: &'a str) -> CheckoutFiles<'a> {
        CheckoutFiles::with_open(store, root, root_str, HashMap::new())
    }

    pub(crate) fn with_open(
        store: &'a Store,
        root: &'a Path,
        root_str: &'a str,
        open: HashMap<String, String>,
    ) -> CheckoutFiles<'a> {
        CheckoutFiles {
            store,
            root,
            root_str,
            open,
            held: RefCell::new(HashMap::new()),
            groups: RefCell::new(HashMap::new()),
        }
    }
}

impl CheckoutFiles<'_> {
    /// An open buffer's text, by checkout-relative path.
    pub(crate) fn open_text(&self, path: &str) -> Option<&str> {
        self.open.get(path).map(String::as_str)
    }

    /// A file's facts, parsed already from the text on disk.
    pub(crate) fn hold(&self, path: &str, facts: Arc<Facts>) {
        self.held.borrow_mut().insert(path.to_string(), Some(facts));
    }
}

impl Files for CheckoutFiles<'_> {
    fn facts(&self, path: &str) -> Option<Arc<Facts>> {
        if let Some(held) = self.held.borrow().get(path) {
            return held.clone();
        }
        let facts = read_facts(self.root, &self.open, path);
        self.held
            .borrow_mut()
            .insert(path.to_string(), facts.clone());
        facts
    }

    fn prefetch(&self, paths: &[String]) {
        use rayon::prelude::*;
        let mut wanted: Vec<&String> = {
            let held = self.held.borrow();
            paths
                .iter()
                .filter(|path| !held.contains_key(*path))
                .collect()
        };
        wanted.sort();
        wanted.dedup();
        if wanted.len() < 2 {
            return;
        }
        let (root, open) = (self.root, &self.open);
        let read: Vec<(String, Option<Arc<Facts>>)> = wanted
            .par_iter()
            .map(|path| (path.to_string(), read_facts(root, open, path)))
            .collect();
        self.held.borrow_mut().extend(read);
    }

    fn calling(&self, name: &str) -> Vec<String> {
        self.store
            .files_calling(self.root_str, name)
            .unwrap_or_default()
    }

    fn mentions(&self, path: &str, needle: &str) -> bool {
        if let Some(text) = self.open.get(path) {
            return text.contains(needle);
        }
        if let Some(Some(facts)) = self.held.borrow().get(path)
            && let Some(source) = facts.source.as_deref()
        {
            return contains(source, needle.as_bytes());
        }
        crate::scan::read_source(self.root.join(path))
            .is_ok_and(|bytes| contains(&bytes, needle.as_bytes()))
    }

    fn may_open_groups(&self, path: &str) -> bool {
        if let Some(known) = self.groups.borrow().get(path) {
            return *known;
        }
        let words = crate::extract::GROUP_WORDS;
        let held = self.held.borrow().get(path).cloned().flatten();
        let may = match (self.open.get(path), held) {
            (Some(text), _) => words.iter().any(|w| text.contains(w)),
            (None, Some(facts)) => facts
                .source
                .as_deref()
                .is_some_and(|source| words.iter().any(|w| contains(source, w.as_bytes()))),
            (None, None) => crate::scan::read_source(self.root.join(path))
                .is_ok_and(|bytes| words.iter().any(|w| contains(&bytes, w.as_bytes()))),
        };
        self.groups.borrow_mut().insert(path.to_string(), may);
        may
    }
}

/// A file's facts: its open buffer's, else the disk's.
fn read_facts(root: &Path, open: &HashMap<String, String>, path: &str) -> Option<Arc<Facts>> {
    let bytes = match open.get(path) {
        Some(text) => Some(text.as_bytes().to_vec()),
        None => crate::scan::read_source(root.join(path)).ok(),
    };
    bytes.map(|bytes| Arc::new(crate::extract::extract_file(path, &bytes)))
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    // `str`'s search skips ahead; a window per byte does not.
    match (std::str::from_utf8(haystack), std::str::from_utf8(needle)) {
        (Ok(haystack), Ok(needle)) => haystack.contains(needle),
        _ => haystack
            .windows(needle.len())
            .any(|window| window == needle),
    }
}

/// The member at a position: written there (`let(:widget)`, `def helper`),
/// or the one a call there reads. A bare `FILE:LINE` takes a member defined
/// on the line. Its file's checkout-relative path, and its definition.
pub(crate) fn member_at_position(
    tree: &crate::tree::Tree,
    files: &CheckoutFiles<'_>,
    relative: &str,
    line: u32,
    col: Option<std::num::NonZeroU32>,
) -> Option<(String, Def)> {
    use crate::resolve::members::{Named, is_member, named_by};
    let facts = files.facts(relative)?;
    let defined_on = |line: u32, from: u32| {
        facts
            .defs
            .iter()
            .filter(|def| def.pos.line == line && def.pos.col >= from && is_member(def))
            .filter(|def| !members::names_a_named_subject(def, &facts))
            .min_by_key(|def| def.pos.col)
            .map(|def| (relative.to_string(), def.clone()))
    };
    let Some(col) = col else {
        return defined_on(line, 0);
    };
    match super::position::at_facts(&facts, line, col.get())? {
        super::position::Under::Definition(def) if is_member(&def) => {
            Some((relative.to_string(), def))
        }
        // On the `let` of `let(:widget)`: the member it defines.
        super::position::Under::Call(call)
            if matches!(call.name.as_str(), "let" | "let!" | "subject" | "subject!") =>
        {
            defined_on(call.pos.line, call.pos.col)
        }
        super::position::Under::Call(call) => match named_by(tree, &facts, &call)? {
            Named::Here(def) => Some((relative.to_string(), *def)),
            Named::Shared { site, line } => {
                let root = tree.site_path("");
                let path = site.strip_prefix(&root)?.to_string();
                let facts = files.facts(&path)?;
                let def = facts
                    .defs
                    .iter()
                    .find(|def| def.pos.line == line && def.name == call.name && is_member(def))?
                    .clone();
                Some((path, def))
            }
        },
        _ => None,
    }
}

/// `--def` on a call in a shared group's body that the body does not
/// answer: the includers' members of the name, one answer each (DEC-490).
/// A helper `RSpec.configure` mixes into every group answers too, for the
/// includers that define none. `None` when no includer defines one, or
/// something else answered.
pub(crate) fn includer_answer(
    tree: &crate::tree::Tree,
    files: &CheckoutFiles<'_>,
    relative: &str,
    call: &crate::core::Call,
    answer: &crate::resolve::MethodAnswer,
) -> Option<crate::resolve::MethodAnswer> {
    let facts = files.facts(relative)?;
    let context = Context::new(tree, files);
    let helper = answer.status != crate::tree::Status::Residue;
    if helper
        && !answer
            .owner
            .as_deref()
            .is_some_and(|o| context.is_helper(o))
    {
        return None;
    }
    let members::Includers {
        found,
        mixed,
        unanswered,
    } = members::includer_members(&context, relative, &facts, call);
    if found.is_empty() && mixed.is_empty() {
        return None;
    }
    let mut sites: Vec<crate::tree::Site> = found
        .iter()
        .map(|(path, def)| crate::tree::Site {
            path: tree.site_path(path),
            line: def.pos.line,
            col: def.pos.col,
            kind: "method".to_string(),
        })
        .chain(mixed.iter().map(|method| method.site.clone()))
        .collect();
    let groups = sites.len();
    let mut agreement =
        format!("{groups} groups that include the shared group define it or mix it in");
    if helper && unanswered > 0 {
        sites.extend(answer.sites.iter().cloned());
        agreement = format!(
            "{groups} of the groups that include the shared group define it; \
             {} answers for {unanswered} that do not",
            answer.owner.as_deref().unwrap_or_default()
        );
    }
    let (owner, kind, defined_via) = match (found.first(), mixed.first()) {
        (Some((path, def)), _) => (
            Asked { path, def }.owner(),
            match def.via {
                Some(_) => crate::tree::Kind::Declaration,
                None => crate::tree::Kind::Definition,
            },
            def.via.clone(),
        ),
        (None, Some(method)) => (method.owner.clone(), method.kind(), method.declared_via()),
        (None, None) => return None,
    };
    let n = sites.len();
    Some(crate::resolve::MethodAnswer {
        status: match n {
            1 => crate::tree::Status::Resolved,
            _ => crate::tree::Status::Ambiguous,
        },
        confidence: crate::resolve::share(1, n),
        resolved_via: Some("includer".to_string()),
        receiver: answer.receiver,
        receiver_type: answer.receiver_type.clone(),
        receiver_kind: answer.receiver_kind.clone(),
        owner: Some(owner),
        kind: Some(kind),
        defined_via,
        sites,
        agreement: (n > 1).then_some(agreement),
        signatures: Vec::new(),
        unresolved_ancestors: Vec::new(),
        candidates: Vec::new(),
        reason: None,
    })
}

/// A call in a shared group's body that `--refs Owner#method` ruled out on
/// the example, tiered again by its includers (DEC-499): where a module an
/// including group mixes in has the method, the call runs it for that
/// group — `confirmed` when every includer's answer is that method,
/// `possible` when only some.
pub(crate) fn includer_reference(
    tree: &crate::tree::Tree,
    files: &CheckoutFiles<'_>,
    call: &crate::core::Call,
    target: &str,
    excluded: &crate::resolve::refs::Reference,
) -> Option<crate::resolve::refs::Reference> {
    use crate::resolve::refs::Tier;
    let facts = files.facts(&excluded.path)?;
    let context = Context::new(tree, files);
    let includers = members::includer_members(&context, &excluded.path, &facts, call);
    let lands = |method: &&crate::tree::MethodDef| {
        !method.singleton && crate::tree::public_name(&method.owner) == target
    };
    let hits = includers.mixed.iter().filter(lands).count();
    if hits == 0 {
        return None;
    }
    let every =
        includers.found.is_empty() && includers.unanswered == 0 && hits == includers.mixed.len();
    let (tier, why, proximity) = match every {
        true => (
            Tier::Confirmed,
            "the shared group runs in groups that include it, each mixing in this",
            0,
        ),
        false => (
            Tier::Possible,
            "the shared group runs in groups that include it, one of which mixes in this",
            1,
        ),
    };
    Some(crate::resolve::refs::Reference {
        tier,
        owner: Some(target.to_string()),
        why,
        ruling: None,
        proximity,
        from: Some("includer"),
        ..excluded.clone()
    })
}
