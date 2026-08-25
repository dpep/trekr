//! Layer 2: what the blob facts mean once you know which files are here.
//!
//! Blob facts are deliberately ignorant of each other — `class Widget < Base`
//! records the string `Base` and stops. This layer is where a checkout's facts
//! are assembled into a constant namespace and an ancestor order, so that
//! `Base` becomes a place.
//!
//! **Rebuilt, not patched.** PLAN §4 takes the Glean/Kythe lesson: per-file
//! facts cache perfectly, and the cross-file graph is where invalidation bites.
//! So this is a whole-checkout rebuild from SQL with no incremental machinery
//! at all. It is cheap enough that adding any would be paying interest on a
//! debt we do not have — see the measurement in docs/ARCHITECTURE.md.
//!
//! Semantics follow Shopify's Rubydex (MIT) `docs/ruby-behaviors.md`.

use crate::core::Param;
use crate::store::{DeclRow, EdgeRow, MethodRow, Store};
use serde::Serialize;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// A name's declaration site — the answer to "where is this?".
#[derive(Clone, Debug, Serialize)]
pub(crate) struct Site {
    pub(crate) path: String,
    pub(crate) line: u32,
    pub(crate) col: u32,
    pub(crate) kind: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MixinKind {
    Prepend,
    Include,
}

#[derive(Clone, Debug)]
struct Mixin {
    kind: MixinKind,
    target: Target,
}

/// A constant reference this scope inherits or contains, before resolution.
#[derive(Clone, Debug)]
struct Target {
    /// The constant as written, which may itself be a path.
    name: String,
    /// The lexical nesting it was written in — what will resolve it.
    nesting: Vec<String>,
}

/// Everything the checkout says about one fully-qualified name.
#[derive(Debug, Default)]
struct Entry {
    kind: String,
    /// Every place it is declared. A reopened class has several; they are one
    /// name, not several, which is why this is a list and not a key.
    sites: Vec<Site>,
    /// Mixins in **source order**, prepends and includes interleaved. Keeping
    /// them in one list is load-bearing: an include only dedups against the
    /// prepends seen *before* it, so `include A; prepend A` and
    /// `prepend A; include A` give different chains.
    mixins: Vec<Mixin>,
    /// First one wins: Ruby raises on a conflicting reopen, so a disagreement
    /// in the index is bad input rather than a case to model.
    superclass: Option<Target>,
    /// `extend M` — M's *instance* methods become this scope's singleton
    /// methods. A different chain from `include`, which is why it is a
    /// different field rather than another mixin kind.
    extends: Vec<Target>,
    /// `Bar = Foo` — the right-hand side as written. `Bar` is a constant in its
    /// own right and keeps its own site; but anywhere a *namespace* is needed
    /// the alias is followed through.
    alias_of: Option<Target>,
}

/// A method definition with its owner resolved.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct MethodDef {
    pub(crate) name: String,
    pub(crate) owner: String,
    pub(crate) singleton: bool,
    pub(crate) visibility: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) via: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) sig_returns: Option<String>,
    /// Required positional arity and whether the method takes more than that.
    pub(crate) arity: (u32, bool),
    pub(crate) site: Site,
}

impl MethodDef {
    /// Could this method be called with `argc` positional arguments? `None`
    /// argc means a splat hid the count, which cannot rule anything out.
    pub(crate) fn accepts(&self, argc: Option<u32>) -> bool {
        let Some(argc) = argc else { return true };
        let (required, variadic) = self.arity;
        argc >= required && (variadic || argc == required)
    }

    /// A bare `private :foo` asserts visibility about a method that may live in
    /// an ancestor. It is a `def` row (DEC-004) but not a definition, so it
    /// must not answer "where is this defined".
    fn is_definition(&self) -> bool {
        !matches!(
            self.via.as_deref(),
            Some("private") | Some("protected") | Some("public")
        )
    }
}

pub(crate) struct Tree {
    names: HashMap<String, Entry>,
    methods: Vec<MethodDef>,
    /// (owner, singleton, name) → definitions. A reopened class gives several.
    by_owner: HashMap<(String, bool, String), Vec<usize>>,
    /// name → every definition anywhere, for ranked residue.
    by_name: HashMap<String, Vec<usize>>,
    /// Classes that have each module in their ancestor chain. Built lazily,
    /// because the common path never asks: it costs a pass over every name and
    /// only a call inside a module needs it.
    includers: RefCell<Option<HashMap<String, Vec<String>>>>,
    /// Linearization is memoized per name. Only the top-level call is cached;
    /// the recursion under it is cheap, and caching mid-flight would mean
    /// caching a chain computed against a partial `seen` set — not the same
    /// answer.
    ancestors: RefCell<HashMap<String, Rc<Ancestry>>>,
}

/// A linearized ancestor chain, and how much of it we could actually build.
#[derive(Debug, Default)]
pub(crate) struct Ancestry {
    pub(crate) chain: Vec<String>,
    /// Ancestor targets that resolved to nothing — a gem superclass, a
    /// dynamically built module. A miss further down the chain is only as
    /// trustworthy as this list is short, so it travels with the answer.
    pub(crate) unresolved: Vec<String>,
}

/// Where in Ruby's lookup ladder an answer came from. The rungs are ordered,
/// and which one hit is the most useful thing to tell a caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Via {
    /// Found in an enclosing lexical scope — Ruby's first choice.
    Lexical,
    /// Found in an ancestor of the innermost scope.
    Ancestor,
    /// Found at the top level.
    Root,
    /// A later segment of a path, found under the segment before it.
    Path,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Status {
    Resolved,
    /// Nothing in the index carries this name. It may belong to a gem, or be
    /// built at runtime, or be a typo — the index cannot tell those apart, and
    /// says so rather than guessing.
    Residue,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Resolution {
    pub(crate) status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) fqn: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) resolved_via: Option<Via>,
    /// 1 or 0, and that is not a hedge: the ladder below is Ruby's own constant
    /// lookup, so within the indexed set a hit is exact rather than ranked. The
    /// uncertainty that does exist is reported as evidence — `scopes_tried`,
    /// `unresolved_ancestors` — instead of being smeared into a number that
    /// would look like a measurement. Grading arrives with the method ladder,
    /// where the yields are measured.
    pub(crate) confidence: f64,
    /// Candidate scopes checked and rejected before the answer.
    pub(crate) scopes_tried: usize,
    /// Ancestors we could not resolve while looking. A residue carrying any of
    /// these is a weaker "no" than one carrying none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) unresolved_ancestors: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) sites: Vec<Site>,
}

/// `A` inside scope `X` is `X::A`; at the top level it is just `A`.
fn qualify(scope: &str, name: &str) -> String {
    if scope.is_empty() {
        name.to_string()
    } else {
        format!("{scope}::{name}")
    }
}

impl Tree {
    /// Assemble a checkout's namespace from its blob facts.
    pub(crate) fn build(store: &Store, root: &str) -> rusqlite::Result<Tree> {
        // Core goes in first, so that a checkout reopening `class Object` adds
        // to it rather than being shadowed by it, and so that every class ends
        // up with an Object/Kernel/BasicObject tail.
        let (mut decls, mut edges, mut methods) = core_rows();

        // Gems sit before the checkout so a gem may reopen core and the
        // checkout may reopen a gem — which is what Rails actually does. The
        // ordering is carried by `checkout.id`, which rises with insert order,
        // so one query per fact kind serves all of them.
        let mut roots: Vec<String> = crate::gems::for_checkout(std::path::Path::new(root))
            .into_iter()
            .filter_map(|gem| gem.root)
            .map(|path| path.to_string_lossy().into_owned())
            .collect();
        roots.push(root.to_string());

        decls.extend(store.declarations(&roots)?);
        edges.extend(store.ancestry(&roots)?);
        methods.extend(store.methods(&roots)?);

        let mut tree = Tree::assemble(decls, edges);
        tree.add_methods(methods);
        Ok(tree)
    }

    fn assemble(decls: Vec<DeclRow>, edges: Vec<EdgeRow>) -> Tree {
        let mut tree = Tree {
            names: HashMap::new(),
            methods: Vec::new(),
            by_owner: HashMap::new(),
            by_name: HashMap::new(),
            includers: RefCell::new(None),
            ancestors: RefCell::new(HashMap::new()),
        };

        // Placing a name can depend on a name not placed yet: `class A::B`
        // needs `A`, and `A` may itself have been written compactly. So settle
        // the key set first, iterating until nothing new appears — each round
        // only adds, so it terminates. Sites are attached in the second pass,
        // which is why an early misplacement leaves no residue behind.
        loop {
            let before = tree.names.len();
            for decl in &decls {
                let fqn = tree.place_decl(decl);
                let nesting = tree.scopes(&decl.nesting);
                // Kind and alias are settled here, not with the sites: placing
                // `class ALIAS::Bar` has to be able to follow `ALIAS` already.
                tree.declare_key(fqn, decl, nesting);
            }
            if tree.names.len() == before {
                break;
            }
        }
        for decl in &decls {
            let fqn = tree.place_decl(decl);
            tree.declare(fqn, decl);
        }

        for edge in edges {
            let owner = tree.scopes(&edge.owner);
            let scope = owner.first().cloned().unwrap_or_default();
            // Ruby evaluates a superclass expression *outside* the class body:
            // `class C < Base` looks up `Base` where `C` is written, not where
            // `C`'s constants live. Every other relation is written inside.
            let nesting = if edge.relation == "superclass" {
                owner.get(1..).unwrap_or_default().to_vec()
            } else {
                owner.clone()
            };
            let target = Target {
                name: edge.target,
                nesting,
            };
            let entry = tree.names.entry(scope).or_default();
            match edge.relation.as_str() {
                "prepend" => entry.mixins.push(Mixin {
                    kind: MixinKind::Prepend,
                    target,
                }),
                "include" => entry.mixins.push(Mixin {
                    kind: MixinKind::Include,
                    target,
                }),
                "superclass" => {
                    entry.superclass.get_or_insert(target);
                }
                "extend" => entry.extends.push(target),
                _ => continue,
            };
        }
        tree
    }

    /// Ruby's `Module.nesting`, rebuilt from what the blob layer saw.
    ///
    /// The blob layer records nesting **as written** — `["B", "A"]` for a class
    /// inside `module A; module B` — because that is all the bytes determine.
    /// Ruby's nesting is the qualified form, `["A::B", "A"]`, and only a
    /// namespace can produce it: a compact `module A::B` inside `module X` may
    /// land under `X` or at the top level depending on what `X::A` is. Getting
    /// this wrong silently resolves every constant in a doubly-nested module to
    /// the wrong place, so it is worth the pass.
    fn scopes(&self, written: &[String]) -> Vec<String> {
        let mut qualified: Vec<String> = Vec::new();
        // Outermost first: each scope is placed in the ones already built.
        for name in written.iter().rev() {
            let here = self.place(name, &qualified);
            qualified.insert(0, here);
        }
        qualified
    }

    fn place_decl(&self, decl: &DeclRow) -> String {
        self.place(&decl.name, &self.scopes(&decl.nesting))
    }

    /// Where a declaration written as `name` inside `scopes` (innermost first)
    /// actually lands.
    fn place(&self, name: &str, scopes: &[String]) -> String {
        // `class ::Bar` is owned by the top level whatever the nesting is —
        // though the nesting still applies to constants read inside its body.
        let (rooted, body) = match name.strip_prefix("::") {
            Some(body) => (true, body),
            None => (false, name),
        };
        let Some((prefix, last)) = body.rsplit_once("::") else {
            let scope = if rooted {
                ""
            } else {
                scopes.first().map_or("", String::as_str)
            };
            return qualify(scope, body);
        };
        if rooted {
            return qualify(prefix, last);
        }
        // Only the last segment is created; the prefix goes through ordinary
        // constant lookup.
        for scope in scopes.iter().map(String::as_str).chain([""]) {
            let candidate = qualify(scope, prefix);
            if self.names.contains_key(&candidate) {
                return qualify(&self.namespace_of(&candidate), last);
            }
        }
        // Unknown prefix — Ruby would raise, but a partial index reaches here
        // constantly because the prefix belongs to a gem. Top level is the
        // honest guess.
        qualify(prefix, last)
    }

    /// Everything about a name except where it is written. Idempotent, so the
    /// placement loop can run it as many times as it needs to.
    fn declare_key(&mut self, fqn: String, decl: &DeclRow, nesting: Vec<String>) {
        let entry = self.names.entry(fqn).or_default();
        // A constant assigned into a class does not make the class a constant;
        // whichever declaration says "class" or "module" names the namespace.
        if entry.kind.is_empty() || (entry.kind == "constant" && decl.kind != "constant") {
            entry.kind = decl.kind.clone();
        }
        if let Some(target) = &decl.target
            && decl.kind == "constant"
        {
            entry.alias_of.get_or_insert(Target {
                name: target.clone(),
                nesting,
            });
        }
    }

    fn declare(&mut self, fqn: String, decl: &DeclRow) {
        self.names.entry(fqn).or_default().sites.push(Site {
            path: decl.path.clone(),
            line: decl.line,
            col: decl.col,
            kind: decl.kind.clone(),
        });
    }

    pub(crate) fn sites(&self, fqn: &str) -> &[Site] {
        self.names.get(fqn).map_or(&[], |e| &e.sites)
    }
}

impl Tree {
    /// The ancestor chain of a name, in Ruby's linearization order:
    /// `[prepends, self, includes, superclass's chain]`, with the first
    /// occurrence of each module winning.
    ///
    /// Memoized, because a file's every constant reference asks for the chain
    /// of the same enclosing class.
    pub(crate) fn ancestors(&self, fqn: &str) -> Rc<Ancestry> {
        let cached = self.ancestors.borrow().get(fqn).cloned();
        if let Some(cached) = cached {
            return cached;
        }
        let mut out = Ancestry::default();
        out.chain = self.linearize(fqn, &mut out, &mut Vec::new());
        let chain = Rc::new(out);
        self.ancestors
            .borrow_mut()
            .insert(fqn.to_string(), chain.clone());
        chain
    }

    /// Ruby's linearization, and the one place where prepend and include are
    /// genuinely not symmetrical.
    ///
    /// * **includes dedup, first-wins**: a module already reachable through a
    ///   prepend, an earlier include, or the superclass chain is dropped from
    ///   the new include, keeping its original deeper position.
    /// * **prepends re-order, last-wins**: an already-present module is pulled
    ///   out and re-inserted at the front — unless the whole prepend would be a
    ///   no-op, in which case it is skipped so existing order survives.
    ///
    /// The asymmetry is real Ruby, not an artifact: `prepend A; include A` puts
    /// `A` once in front, while `include A; prepend A` puts it in *both* places.
    /// A single "seen" set gets that wrong and looks right on every simple case.
    fn linearize(&self, fqn: &str, out: &mut Ancestry, stack: &mut Vec<String>) -> Vec<String> {
        if stack.iter().any(|f| f == fqn) {
            // Not valid Ruby, but a partial or half-written index reaches here.
            // An empty chain lets the caller finish instead of recursing.
            return Vec::new();
        }
        stack.push(fqn.to_string());
        let entry = self.names.get(fqn);

        // The parent chain is needed before includes, because includes dedup
        // against it.
        let parent: Vec<String> = match entry.and_then(|e| e.superclass.as_ref()) {
            Some(target) => self.chain_of(target, out, stack),
            // Every class without an explicit superclass inherits Object, and
            // that tail is most of what core indexing buys: it is how `puts`
            // and `raise` become findable from an ordinary class body.
            None if self.inherits_object(fqn, entry) => self.linearize(OBJECT, out, stack),
            None => Vec::new(),
        };

        let mut prepends: Vec<String> = Vec::new();
        let mut includes: Vec<String> = Vec::new();
        for mixin in entry.map(|e| e.mixins.as_slice()).unwrap_or_default() {
            let mut ids = self.chain_of(&mixin.target, out, stack);
            match mixin.kind {
                MixinKind::Prepend => {
                    // Last wins: an existing entry is pulled out and re-inserted
                    // at the front — unless the whole prepend is a no-op, when
                    // skipping it preserves the order already established.
                    if ids.iter().any(|id| !prepends.contains(id)) {
                        prepends.retain(|id| !ids.contains(id));
                        for id in ids.into_iter().rev() {
                            prepends.insert(0, id);
                        }
                    }
                }
                MixinKind::Include => {
                    // First wins: anything already reachable keeps its deeper
                    // position instead of being pulled forward.
                    ids.retain(|id| {
                        !prepends.contains(id) && !includes.contains(id) && !parent.contains(id)
                    });
                    for id in ids.into_iter().rev() {
                        includes.insert(0, id);
                    }
                }
            }
        }

        stack.pop();
        let mut chain = prepends;
        chain.push(fqn.to_string());
        chain.extend(includes);
        chain.extend(parent);
        chain
    }

    /// Does this name get Ruby's implicit `< Object`?
    ///
    /// Only classes — a module has no superclass at all — and not the two
    /// roots, whose own chain the core stub states outright.
    fn inherits_object(&self, fqn: &str, entry: Option<&Entry>) -> bool {
        entry.is_some_and(|e| e.kind == "class")
            && fqn != OBJECT
            && fqn != "BasicObject"
            && self.names.contains_key(OBJECT)
    }

    /// One mixin or superclass target: its own whole chain, or nothing plus a
    /// note that we could not see it.
    fn chain_of(
        &self,
        target: &Target,
        out: &mut Ancestry,
        stack: &mut Vec<String>,
    ) -> Vec<String> {
        match self.resolve_lexical(&target.name, &target.nesting) {
            Some(fqn) => {
                let fqn = self.namespace_of(&fqn);
                self.linearize(&fqn, out, stack)
            }
            // `class Widget < ActiveRecord::Base` in a checkout with no gems
            // indexed. The chain stops here, and the answer says so.
            None => {
                if !out.unresolved.contains(&target.name) {
                    out.unresolved.push(target.name.clone());
                }
                Vec::new()
            }
        }
    }

    /// Constant lookup **without** the ancestor rung.
    ///
    /// This is what resolves an ancestry edge's own target, and leaving
    /// ancestors out is what keeps that from being circular: you cannot find a
    /// class's superclass by looking through the ancestors it does not have
    /// yet. Ruby has the same bootstrapping problem and resolves the
    /// superclass expression in the enclosing lexical scope, which is exactly
    /// this.
    fn resolve_lexical(&self, written: &str, nesting: &[String]) -> Option<String> {
        let (head, rest) = split_path(written);
        let mut current = if let Some(head) = head.strip_prefix("::") {
            self.names.contains_key(head).then(|| head.to_string())
        } else {
            nesting
                .iter()
                .map(String::as_str)
                .chain([""])
                .map(|scope| qualify(scope, head))
                .find(|candidate| self.names.contains_key(candidate))
        }?;
        for segment in rest {
            current = self.descend(&current, segment)?;
        }
        Some(current)
    }

    /// A constant assigned another constant is a second name for one thing.
    /// `Bar` keeps its own declaration site — go-to-definition on `Bar` should
    /// land on `Bar = Foo` — but anywhere a *namespace* is wanted, the alias is
    /// followed through to it.
    fn namespace_of(&self, fqn: &str) -> String {
        let mut current = fqn.to_string();
        let mut seen = HashSet::new();
        while seen.insert(current.clone()) {
            let Some(alias) = self.names.get(&current).and_then(|e| e.alias_of.as_ref()) else {
                break;
            };
            match self.resolve_lexical(&alias.name, &alias.nesting) {
                Some(next) => current = next,
                None => break,
            }
        }
        current
    }

    /// One segment of a path: `A::B` finds `B` in `A` or in `A`'s ancestors —
    /// never in the lexical nesting, which only ever applies to the head.
    fn descend(&self, parent: &str, segment: &str) -> Option<String> {
        let parent = &self.namespace_of(parent);
        let direct = qualify(parent, segment);
        if self.names.contains_key(&direct) {
            return Some(direct);
        }
        self.ancestors(parent)
            .chain
            .iter()
            .map(|ancestor| qualify(ancestor, segment))
            .find(|candidate| self.names.contains_key(candidate))
    }

    /// Ruby's constant lookup, in full, with the evidence behind the answer.
    ///
    /// The ladder for the head of a path: every enclosing lexical scope's own
    /// constants, then the ancestors of the innermost scope, then the top
    /// level. Later segments descend through the previous one's ancestors
    /// instead — lexical nesting does not apply past the head.
    pub(crate) fn resolve(&self, written: &str, written_nesting: &[String]) -> Resolution {
        let nesting = self.scopes(written_nesting);
        let (head, rest) = split_path(written);
        let mut unresolved = Vec::new();

        let mut candidates: Vec<(String, Via)> = Vec::new();
        if let Some(rooted) = head.strip_prefix("::") {
            candidates.push((rooted.to_string(), Via::Root));
        } else {
            for scope in &nesting {
                candidates.push((qualify(scope, head), Via::Lexical));
            }
            if let Some(innermost) = nesting.first() {
                let chain = self.ancestors(innermost);
                unresolved = chain.unresolved.clone();
                for ancestor in &chain.chain {
                    candidates.push((qualify(ancestor, head), Via::Ancestor));
                }
            }
            candidates.push((head.to_string(), Via::Root));
        }

        let mut tried = 0;
        let mut checked = HashSet::new();
        let mut found = None;
        for (candidate, via) in candidates {
            if !checked.insert(candidate.clone()) {
                continue; // the innermost scope is both a lexical scope and its
                // own first ancestor; counting it twice would overstate the work
            }
            if self.names.contains_key(&candidate) {
                found = Some((candidate, via));
                break;
            }
            tried += 1;
        }

        let Some((mut current, mut via)) = found else {
            return Resolution {
                status: Status::Residue,
                fqn: None,
                resolved_via: None,
                confidence: 0.0,
                scopes_tried: tried,
                unresolved_ancestors: unresolved,
                sites: Vec::new(),
            };
        };

        for segment in rest {
            let Some(next) = self.descend(&current, segment) else {
                return Resolution {
                    status: Status::Residue,
                    fqn: None,
                    resolved_via: None,
                    confidence: 0.0,
                    scopes_tried: tried + 1,
                    unresolved_ancestors: self.ancestors(&current).unresolved.clone(),
                    sites: Vec::new(),
                };
            };
            current = next;
            via = Via::Path;
        }

        Resolution {
            status: Status::Resolved,
            sites: self.sites(&current).to_vec(),
            fqn: Some(current),
            resolved_via: Some(via),
            confidence: 1.0,
            scopes_tried: tried,
            unresolved_ancestors: unresolved,
        }
    }
}

/// `A::B::C` is a head and the segments under it. A leading `::` stays on the
/// head, because that is where it changes the meaning.
fn split_path(written: &str) -> (&str, Vec<&str>) {
    let rooted = written.starts_with("::");
    let body = if rooted { &written[2..] } else { written };
    let mut segments: Vec<&str> = body.split("::").collect();
    let first = segments.remove(0);
    let head = if rooted {
        &written[..2 + first.len()]
    } else {
        first
    };
    (head, segments)
}

/// Ruby source in, assembled namespace out — through the real extractor, so
/// tests written against this are conformance tests for the pair, not for the
/// tree alone.
/// Ruby source in, assembled namespace out — through the real extractor and
/// with the real core stub, so tests written against this exercise the same
/// path `Tree::build` takes.
#[cfg(test)]
pub(crate) fn for_test(sources: &[(&str, &str)]) -> Tree {
    let (mut decls, mut edges, mut methods) = core_rows();
    for (path, source) in sources {
        let (d, e, m) = rows_from(path, source);
        assert!(
            !d.is_empty() || !m.is_empty() || !e.is_empty() || source.trim().is_empty(),
            "fixture produced no facts: {path}"
        );
        decls.extend(d);
        edges.extend(e);
        methods.extend(m);
    }
    let mut tree = Tree::assemble(decls, edges);
    tree.add_methods(methods);
    tree
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(sources: &[(&str, &str)]) -> Tree {
        super::for_test(sources)
    }

    fn one(source: &str) -> Tree {
        tree(&[("a.rb", source)])
    }

    /// An ancestor chain with core's tail removed.
    ///
    /// Every class now ends `Object, Kernel, BasicObject`, which is correct and
    /// uninteresting to a test about linearization order. Dropping it keeps
    /// these assertions about the thing they are testing.
    fn chain(tree: &Tree, fqn: &str) -> Vec<String> {
        tree.ancestors(fqn)
            .chain
            .iter()
            .filter(|name| {
                tree.sites(name)
                    .first()
                    .is_none_or(|site| site.path != CORE_PATH)
            })
            .cloned()
            .collect()
    }

    /// What `name` resolves to when written inside `nesting` (innermost first,
    /// as the blob layer records it).
    fn at(tree: &Tree, name: &str, nesting: &[&str]) -> Option<String> {
        let nesting: Vec<String> = nesting.iter().map(|s| s.to_string()).collect();
        tree.resolve(name, &nesting).fqn
    }

    #[test]
    fn every_plain_class_ends_in_objects_tail() {
        let tree = one("class W\nend\n");
        assert_eq!(
            tree.ancestors("W").chain,
            ["W", "Object", "Kernel", "BasicObject"],
            "the implicit `< Object` is what makes Kernel reachable"
        );
        assert!(
            tree.ancestors("BasicObject").chain.len() == 1,
            "the root inherits nothing"
        );
    }

    #[test]
    fn a_module_gets_no_object_tail_because_it_has_no_superclass() {
        let tree = one("module M\nend\n");
        assert_eq!(tree.ancestors("M").chain, ["M"]);
    }

    #[test]
    fn core_constants_resolve_and_carry_their_real_hierarchy() {
        let tree = one("class W\nend\n");
        for name in ["ENV", "ArgumentError", "Hash", "Comparable"] {
            assert!(
                tree.resolve(name, &[]).fqn.is_some(),
                "{name} should be a known core constant"
            );
        }
        assert_eq!(
            tree.ancestors("KeyError").chain,
            [
                "KeyError",
                "IndexError",
                "StandardError",
                "Exception",
                "Object",
                "Kernel",
                "BasicObject"
            ],
            "the exception hierarchy is real, not flat"
        );
    }

    #[test]
    fn a_checkout_reopening_a_core_class_adds_to_it() {
        // ActiveSupport does exactly this to Object; core must not shadow it.
        let tree = one("class Object\n  def blank?\n  end\nend\n");
        assert_eq!(
            tree.lookup("Object", false, "blank?")
                .map(|m| m.owner.clone()),
            Some("Object".to_string())
        );
        assert!(
            tree.lookup("Object", false, "frozen?").is_some(),
            "and core's own methods survive the reopen"
        );
    }

    #[test]
    fn qualifies_a_nested_declaration_by_its_whole_lexical_path() {
        let tree = one("module A\n  module B\n    class C\n    end\n  end\nend\n");
        assert!(
            tree.names.contains_key("A::B::C"),
            "two levels of nesting qualify twice: {:?}",
            tree.names.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_compact_declaration_opens_one_scope_and_creates_only_its_last_segment() {
        let tree = one("module A\nend\nmodule A::B\n  class C\n  end\nend\n");
        assert!(tree.names.contains_key("A::B::C"));
        // `module A::B` does not put `A` in the nesting, so a constant written
        // inside it cannot see `A`'s.
        let tree = one("module A\n  X = 1\nend\nmodule A::B\n  Y = X\nend\n");
        assert_eq!(at(&tree, "X", &["A::B"]), None, "A is not in the nesting");
    }

    #[test]
    fn a_compact_prefix_is_resolved_rather_than_concatenated() {
        // `module A::B` inside `module X` lands under `X` when `X::A` exists…
        let tree = one("module X\n  module A\n  end\n  module A::B\n  end\nend\n");
        assert!(
            tree.names.contains_key("X::A::B"),
            "{:?}",
            tree.names.keys().collect::<Vec<_>>()
        );
        // …and at the top level when it does not.
        let tree = one("module A\nend\nmodule X\n  module A::B\n  end\nend\n");
        assert!(tree.names.contains_key("A::B"));
    }

    #[test]
    fn lexical_nesting_beats_an_ancestor() {
        let tree = one(
            "class Base\n  X = :from_ancestor\nend\nmodule A\n  X = :from_nesting\n  \
             class C < Base\n  end\nend\n",
        );
        assert_eq!(at(&tree, "X", &["C", "A"]).as_deref(), Some("A::X"));
    }

    #[test]
    fn an_ancestor_beats_the_top_level() {
        let tree = one("X = :top\nclass Base\n  X = :inherited\nend\nclass C < Base\nend\n");
        let r = tree.resolve("X", &["C".to_string()]);
        assert_eq!(r.fqn.as_deref(), Some("Base::X"));
        assert_eq!(r.resolved_via, Some(Via::Ancestor));
    }

    #[test]
    fn the_top_level_is_the_last_rung_not_the_first() {
        let tree = one("X = :top\nclass C\nend\n");
        let r = tree.resolve("X", &["C".to_string()]);
        assert_eq!(r.resolved_via, Some(Via::Root));
        assert!(r.scopes_tried > 0, "C::X was checked and missed first");
    }

    #[test]
    fn a_leading_colon_colon_skips_the_ladder_entirely() {
        let tree = one("X = :top\nclass C\n  X = :inner\nend\n");
        assert_eq!(at(&tree, "X", &["C"]).as_deref(), Some("C::X"));
        assert_eq!(at(&tree, "::X", &["C"]).as_deref(), Some("X"));
    }

    #[test]
    fn linearizes_prepends_then_self_then_includes_then_superclass() {
        let tree = one("module P\nend\nmodule I\nend\nclass Base\nend\n\
             class C < Base\n  include I\n  prepend P\nend\n");
        assert_eq!(chain(&tree, "C"), ["P", "C", "I", "Base"]);
    }

    #[test]
    fn the_last_mixin_applied_is_the_nearest() {
        let tree = one("module A\nend\nmodule B\nend\nclass C\n  include A\n  include B\nend\n");
        assert_eq!(chain(&tree, "C"), ["C", "B", "A"]);
    }

    #[test]
    fn a_multi_argument_include_applies_right_to_left() {
        // `include A, B` calls append_features(B) then append_features(A), so
        // A ends up nearer — the reverse of the two-statement form above.
        let tree = one("module A\nend\nmodule B\nend\nclass C\n  include A, B\nend\n");
        assert_eq!(chain(&tree, "C"), ["C", "A", "B"]);
    }

    #[test]
    fn an_include_already_reachable_through_the_superclass_is_a_no_op() {
        let tree = one("module M\nend\nclass Base\n  include M\nend\n\
             class C < Base\n  include M\nend\n");
        assert_eq!(
            chain(&tree, "C"),
            ["C", "Base", "M"],
            "M keeps its deeper position rather than being pulled forward"
        );
    }

    // ── Ported from Rubydex `resolution_tests.rs` (MIT). These are the cases
    // where a plausible implementation is silently wrong. Core classes are not
    // indexed here, so the `Object, Kernel, BasicObject` tails in the original
    // expectations are absent; nothing else is changed.

    #[test]
    fn a_multi_argument_prepend_also_applies_right_to_left() {
        let tree = one("module A\nend\nmodule B\nend\nclass Foo\n  prepend A, B\nend\n");
        assert_eq!(chain(&tree, "Foo"), ["A", "B", "Foo"]);
    }

    #[test]
    fn a_module_shared_by_two_includes_keeps_its_deepest_position() {
        let tree = one(
            "module A\nend\nmodule B\n  include A\nend\nmodule C\n  include A\nend\n\
             module Foo\n  include B\n  include C\nend\n",
        );
        assert_eq!(chain(&tree, "Foo"), ["Foo", "C", "B", "A"]);
    }

    #[test]
    fn a_module_shared_by_two_prepends_is_pulled_to_the_front() {
        let tree = one(
            "module A\nend\nmodule B\n  prepend A\nend\nmodule C\n  prepend A\nend\n\
             module Foo\n  prepend B\n  prepend C\nend\n",
        );
        assert_eq!(
            chain(&tree, "Foo"),
            ["A", "C", "B", "Foo"],
            "prepends re-order what is already there; includes never do"
        );
    }

    #[test]
    fn prepend_and_include_of_the_same_module_are_not_symmetrical() {
        // The case a single "seen" set gets wrong while looking right
        // everywhere else.
        let tree = one("module A\nend\nclass Foo\n  prepend A\n  include A\nend\n\
             class Bar\n  include A\n  prepend A\nend\n");
        assert_eq!(chain(&tree, "Foo"), ["A", "Foo"], "the include is a no-op");
        assert_eq!(
            chain(&tree, "Bar"),
            ["A", "Bar", "A"],
            "the prepend adds a second entry in front"
        );
    }

    #[test]
    fn includes_dedup_against_the_parent_chain_but_prepends_do_not() {
        let tree = one("module A\nend\nclass Parent\n  include A\nend\n\
             class Foo < Parent\n  prepend A\nend\n\
             class Bar < Parent\n  include A\nend\n");
        assert_eq!(chain(&tree, "Foo"), ["A", "Foo", "Parent", "A"]);
        assert_eq!(chain(&tree, "Bar"), ["Bar", "Parent", "A"]);
    }

    #[test]
    fn a_module_has_no_superclass_segment_at_all() {
        let tree = one("module Foo\nend\nmodule Bar\n  prepend Foo\nend\n");
        assert_eq!(chain(&tree, "Bar"), ["Foo", "Bar"]);
    }

    #[test]
    fn mixing_a_module_into_itself_collapses_instead_of_recursing() {
        for source in [
            "module Foo\n  include Foo\nend\n",
            "module Foo\n  prepend Foo\nend\n",
        ] {
            assert_eq!(chain(&one(source), "Foo"), ["Foo"]);
        }
    }

    #[test]
    fn a_compact_prefix_escapes_the_enclosing_nesting_when_it_resolves_outside() {
        let tree = one("module Bar\nend\nmodule Foo\n  class Bar::Baz\n  end\nend\n");
        assert!(tree.names.contains_key("Bar::Baz"));
        assert!(
            !tree.names.contains_key("Foo::Bar"),
            "the prefix resolved to the top-level Bar, so Foo gained nothing"
        );
    }

    #[test]
    fn a_rooted_declaration_is_owned_by_the_top_level() {
        let tree = one("module Foo\n  class ::Bar\n    class Baz\n    end\n  end\nend\n");
        assert!(tree.names.contains_key("Bar"));
        assert!(tree.names.contains_key("Bar::Baz"));
        assert!(!tree.names.contains_key("Foo::Bar"));
    }

    #[test]
    fn a_constant_alias_is_followed_wherever_a_namespace_is_wanted() {
        let tree = one("class Base\nend\nAliasedBase = Base\nclass Foo < AliasedBase\nend\n");
        assert_eq!(chain(&tree, "Foo"), ["Foo", "Base"]);

        // …but the alias keeps its own definition site, because that is where
        // go-to-definition on `AliasedBase` should land.
        let r = tree.resolve("AliasedBase", &[]);
        assert_eq!(r.fqn.as_deref(), Some("AliasedBase"));
        assert_eq!(r.sites.len(), 1);
    }

    #[test]
    fn a_declaration_under_an_alias_lands_under_what_it_aliases() {
        let tree = one("class Foo\nend\nALIAS = Foo\nclass ALIAS::Bar\nend\n");
        assert!(
            tree.names.contains_key("Foo::Bar"),
            "{:?}",
            tree.names.keys().collect::<Vec<_>>()
        );
        assert!(!tree.names.contains_key("ALIAS::Bar"));
    }

    #[test]
    fn an_alias_cycle_stops_instead_of_spinning() {
        let tree = one("A = B\nB = A\n");
        assert!(tree.resolve("A", &[]).fqn.is_some());
    }

    #[test]
    fn a_qualified_path_reaches_constants_a_mixin_brought_in() {
        let tree = one("module Foo\n  module Bar\n  end\nend\nclass Baz\n  include Foo\nend\n");
        assert_eq!(at(&tree, "Baz::Bar", &[]).as_deref(), Some("Foo::Bar"));
    }

    #[test]
    fn the_lexical_walk_continues_outward_through_a_singleton_scope() {
        let tree = one(
            "module A\n  module B\n    class Sibling\n    end\n    class Main\n      \
             class << self\n        def m\n          Sibling\n        end\n      end\n    end\n  end\nend\n",
        );
        // `class << self` opens no named scope, so the nesting seen inside is
        // still `[Main, B, A]`.
        assert_eq!(
            at(&tree, "Sibling", &["Main", "B", "A"]).as_deref(),
            Some("A::B::Sibling")
        );
        assert_eq!(at(&tree, "NotDefined", &["Main", "B", "A"]), None);
    }

    #[test]
    fn a_mixin_brings_its_own_ancestors_with_it() {
        let tree =
            one("module Deep\nend\nmodule M\n  include Deep\nend\nclass C\n  include M\nend\n");
        assert_eq!(chain(&tree, "C"), ["C", "M", "Deep"]);
    }

    #[test]
    fn an_inheritance_cycle_terminates_instead_of_hanging() {
        let tree = one("class A < B\nend\nclass B < A\nend\n");
        let chain = chain(&tree, "A");
        assert!(chain.contains(&"A".to_string()) && chain.contains(&"B".to_string()));
        assert_eq!(chain.len(), 2, "each name appears once: {chain:?}");
    }

    #[test]
    fn a_path_segment_searches_ancestors_but_never_the_lexical_nesting() {
        let tree = one("module Outer\n  Hidden = 1\n  module Api\n  end\nend\n\
             module Host\n  include Outer::Api\nend\n");
        // `Outer::Api` resolves; `Outer::Missing` does not, even though a scope
        // in the nesting has a `Missing`.
        let tree2 = one("module N\n  Missing = 1\n  module Outer\n  end\nend\n");
        assert_eq!(at(&tree2, "Outer::Missing", &["N"]), None);
        assert_eq!(at(&tree, "Outer::Api", &[]).as_deref(), Some("Outer::Api"));
    }

    #[test]
    fn a_path_segment_does_search_the_previous_segments_ancestors() {
        let tree = one("class Base\n  Inner = 1\nend\nclass C < Base\nend\n");
        assert_eq!(at(&tree, "C::Inner", &[]).as_deref(), Some("Base::Inner"));
    }

    #[test]
    fn reopening_a_class_is_one_name_with_several_sites() {
        let tree = tree(&[
            ("a.rb", "class Widget\nend\n"),
            ("b.rb", "class Widget\nend\n"),
        ]);
        let sites = tree.sites("Widget");
        assert_eq!(sites.len(), 2);
        assert_eq!(sites[0].path, "a.rb");
        assert_eq!(sites[1].path, "b.rb");
    }

    #[test]
    fn a_superclass_is_resolved_in_the_scope_that_wrote_it() {
        let tree = one("module A\n  class Base\n  end\n  class C < Base\n  end\nend\n");
        assert_eq!(chain(&tree, "A::C"), ["A::C", "A::Base"]);
    }

    #[test]
    fn an_ancestor_we_cannot_resolve_is_reported_not_dropped() {
        let tree = one("class Widget < ActiveRecord::Base\nend\n");
        let ancestry = tree.ancestors("Widget");
        assert_eq!(ancestry.chain, ["Widget"]);
        assert_eq!(
            ancestry.unresolved,
            ["ActiveRecord::Base"],
            "a gem superclass makes every later miss less trustworthy"
        );
    }

    #[test]
    fn a_name_nothing_declares_is_residue_carrying_its_evidence() {
        let tree = one("class Widget < ActiveRecord::Base\n  def go\n  end\nend\n");
        let r = tree.resolve("Missing", &["Widget".to_string()]);
        assert_eq!(r.status, Status::Residue);
        assert_eq!(r.confidence, 0.0);
        assert!(
            r.scopes_tried >= 2,
            "Widget::Missing and ::Missing were tried"
        );
        assert_eq!(
            r.unresolved_ancestors,
            ["ActiveRecord::Base"],
            "this 'no' is weaker than one with a complete chain, and says so"
        );
    }

    #[test]
    fn a_resolved_constant_is_exact_because_the_ladder_is_rubys_own() {
        let tree = one("module A\n  class C\n  end\nend\n");
        let r = tree.resolve("C", &["A".to_string()]);
        assert_eq!(r.status, Status::Resolved);
        assert_eq!(r.confidence, 1.0);
        assert_eq!(r.scopes_tried, 0, "found at the first rung");
    }
}

impl Tree {
    fn add_methods(&mut self, rows: Vec<MethodRow>) {
        for row in rows {
            let scopes = self.scopes(&row.nesting);
            // `def Foo.x` names its owner outright; everything else belongs to
            // the scope it is written in.
            let owner = match &row.target {
                Some(target) if row.singleton => self
                    .resolve_lexical(target, &scopes)
                    .map(|fqn| self.namespace_of(&fqn))
                    .unwrap_or_else(|| target.clone()),
                _ => scopes.first().cloned().unwrap_or_default(),
            };
            let index = self.methods.len();
            let def = MethodDef {
                arity: arity_of(&row.params),
                name: row.name.clone(),
                owner: owner.clone(),
                singleton: row.singleton,
                visibility: row.visibility,
                via: row.via,
                sig_returns: row.sig_returns,
                site: Site {
                    path: row.path,
                    line: row.line,
                    col: row.col,
                    kind: "method".into(),
                },
            };
            self.by_owner
                .entry((owner, row.singleton, row.name.clone()))
                .or_default()
                .push(index);
            self.by_name.entry(row.name).or_default().push(index);
            self.methods.push(def);
        }
    }

    /// The chain of `(owner, singleton)` pairs Ruby searches for a method.
    ///
    /// For an instance method that is just the ancestor chain. For a singleton
    /// method it is a **different** walk: up the *superclass* chain only —
    /// included modules contribute no class methods — inserting at each level
    /// the level's own singleton methods and then whatever it `extend`s.
    pub(crate) fn lookup_chain(&self, fqn: &str, singleton: bool) -> Vec<(String, bool)> {
        if !singleton {
            return self
                .ancestors(fqn)
                .chain
                .iter()
                .map(|owner| (owner.clone(), false))
                .collect();
        }
        let mut chain = Vec::new();
        let mut seen = HashSet::new();
        for class in self.superclass_chain(fqn) {
            if seen.insert((class.clone(), true)) {
                chain.push((class.clone(), true));
            }
            let extends = self
                .names
                .get(&class)
                .map(|entry| entry.extends.clone())
                .unwrap_or_default();
            for target in extends.iter().rev() {
                // `extend self` is the module-function idiom: the module
                // extends itself, so its own instance methods become singleton
                // ones. The target names no constant, so the owner is it.
                let module = if target.name == "self" {
                    Some(class.clone())
                } else {
                    self.resolve_lexical(&target.name, &target.nesting)
                        .map(|fqn| self.namespace_of(&fqn))
                };
                let Some(module) = module else { continue };
                for ancestor in &self.ancestors(&module).chain {
                    if seen.insert((ancestor.clone(), false)) {
                        chain.push((ancestor.clone(), false));
                    }
                }
            }
        }

        // `Foo.singleton_class.ancestors` does not stop at the superclass
        // walk: it continues into Class, Module, Object, Kernel, BasicObject as
        // ordinary instance methods. That tail is how `Foo.new` finds
        // `Class#new` and a class body's `prepend` finds `Module#prepend`.
        let tail = match self.kind_of(fqn) {
            Some("class") => "Class",
            // A module has no singleton superclass chain of its own.
            Some("module") => "Module",
            _ => return chain,
        };
        for ancestor in &self.ancestors(tail).chain {
            if seen.insert((ancestor.clone(), false)) {
                chain.push((ancestor.clone(), false));
            }
        }
        chain
    }

    /// Only the superclass links — no mixins. Class methods are inherited down
    /// this chain and nowhere else.
    fn superclass_chain(&self, fqn: &str) -> Vec<String> {
        let mut chain = Vec::new();
        let mut seen = HashSet::new();
        let mut current = fqn.to_string();
        while seen.insert(current.clone()) {
            chain.push(current.clone());
            let Some(superclass) = self.names.get(&current).and_then(|e| e.superclass.as_ref())
            else {
                break;
            };
            let Some(next) = self.resolve_lexical(&superclass.name, &superclass.nesting) else {
                break;
            };
            current = self.namespace_of(&next);
        }
        chain
    }

    /// The method a receiver of this type would run: the first owner in the
    /// chain that defines the name. Ruby's own rule, so within the indexed set
    /// this is exact rather than a guess.
    pub(crate) fn lookup(&self, fqn: &str, singleton: bool, name: &str) -> Option<&MethodDef> {
        for (owner, owner_singleton) in self.lookup_chain(fqn, singleton) {
            let key = (owner, owner_singleton, name.to_string());
            if let Some(found) = self.by_owner.get(&key).and_then(|hits| {
                hits.iter()
                    .rev()
                    .find(|i| self.methods[**i].is_definition())
            }) {
                return Some(&self.methods[*found]);
            }
        }
        None
    }

    /// Every method with this name, anywhere. The candidate pool for residue.
    pub(crate) fn named(&self, name: &str) -> Vec<&MethodDef> {
        self.by_name
            .get(name)
            .map(|hits| {
                hits.iter()
                    .map(|i| &self.methods[*i])
                    .filter(|m| m.is_definition())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The fully-qualified name of the scope a fact was written in.
    pub(crate) fn scope_fqn(&self, written_nesting: &[String]) -> Option<String> {
        self.scopes(written_nesting).into_iter().next()
    }

    /// The classes that mix in this module, directly or through another module.
    ///
    /// A module is never the receiver of a call written inside it — whatever
    /// includes it is. When the index knows exactly which class that is, the
    /// call has a determinate receiver after all, and this is how to find it.
    pub(crate) fn includers_of(&self, module: &str) -> Vec<String> {
        if self.includers.borrow().is_none() {
            let mut map: HashMap<String, Vec<String>> = HashMap::new();
            // Only classes: resolving a module receiver to another module
            // would just move the problem.
            let classes: Vec<String> = self
                .names
                .iter()
                .filter(|(_, entry)| entry.kind == "class")
                .map(|(fqn, _)| fqn.clone())
                .collect();
            for class in classes {
                for ancestor in &self.ancestors(&class).chain {
                    if ancestor != &class {
                        map.entry(ancestor.clone()).or_default().push(class.clone());
                    }
                }
            }
            for names in map.values_mut() {
                names.sort();
                names.dedup();
            }
            *self.includers.borrow_mut() = Some(map);
        }
        self.includers
            .borrow()
            .as_ref()
            .and_then(|map| map.get(module).cloned())
            .unwrap_or_default()
    }

    /// `class`, `module`, or `constant` — for a name the checkout declares.
    pub(crate) fn kind_of(&self, fqn: &str) -> Option<&str> {
        self.names
            .get(fqn)
            .map(|entry| entry.kind.as_str())
            .filter(|kind| !kind.is_empty())
    }

    pub(crate) fn is_known(&self, fqn: &str) -> bool {
        self.names.contains_key(fqn)
    }
}

/// Required positional arity, and whether more are accepted.
fn arity_of(params: &[Param]) -> (u32, bool) {
    use crate::core::ParamKind::*;
    let required = params
        .iter()
        .filter(|p| matches!(p.kind, Req | Post))
        .count() as u32;
    let variadic = params
        .iter()
        .any(|p| matches!(p.kind, Opt | Rest | Keyrest | Block));
    (required, variadic)
}

#[cfg(test)]
mod singleton_tests {
    use super::*;

    fn one(source: &str) -> Tree {
        for_test(&[("a.rb", source)])
    }

    /// Where a method lookup lands, as `Owner` — or nothing.
    fn find(tree: &Tree, fqn: &str, singleton: bool, name: &str) -> Option<String> {
        tree.lookup(fqn, singleton, name).map(|m| m.owner.clone())
    }

    #[test]
    fn a_singleton_method_is_found_however_it_was_written() {
        let tree = one(
            "class W\n  def self.built\n  end\n  class << self\n    def made\n    end\n  end\n  \
             def instance\n  end\nend\n",
        );
        assert_eq!(find(&tree, "W", true, "built").as_deref(), Some("W"));
        assert_eq!(find(&tree, "W", true, "made").as_deref(), Some("W"));
        assert_eq!(
            find(&tree, "W", true, "instance"),
            None,
            "an instance method is not on the class"
        );
        assert_eq!(find(&tree, "W", false, "instance").as_deref(), Some("W"));
    }

    #[test]
    fn class_methods_are_inherited_down_the_superclass_chain() {
        let tree = one("class Base\n  def self.build\n  end\nend\nclass W < Base\nend\n");
        assert_eq!(find(&tree, "W", true, "build").as_deref(), Some("Base"));
    }

    #[test]
    fn including_a_module_gives_no_class_methods_but_extending_does() {
        let tree = one("module M\n  def helper\n  end\nend\n\
             class Included\n  include M\nend\n\
             class Extended\n  extend M\nend\n");
        assert_eq!(
            find(&tree, "Included", true, "helper"),
            None,
            "include contributes instance methods only"
        );
        assert_eq!(
            find(&tree, "Included", false, "helper").as_deref(),
            Some("M")
        );
        assert_eq!(
            find(&tree, "Extended", true, "helper").as_deref(),
            Some("M")
        );
        assert_eq!(
            find(&tree, "Extended", false, "helper"),
            None,
            "extend contributes singleton methods only"
        );
    }

    #[test]
    fn extend_self_makes_a_modules_own_methods_callable_on_it() {
        let tree = one("module M\n  extend self\n  def helper\n  end\nend\n");
        assert_eq!(find(&tree, "M", true, "helper").as_deref(), Some("M"));
    }

    #[test]
    fn module_function_reaches_both_ways() {
        let tree = one("module M\n  module_function\n  def normalize\n  end\nend\n");
        assert_eq!(find(&tree, "M", true, "normalize").as_deref(), Some("M"));
        assert_eq!(find(&tree, "M", false, "normalize").as_deref(), Some("M"));
    }

    #[test]
    fn an_extended_modules_own_includes_come_along() {
        let tree = one(
            "module Deep\n  def deep\n  end\nend\nmodule M\n  include Deep\nend\n\
             class W\n  extend M\nend\n",
        );
        assert_eq!(find(&tree, "W", true, "deep").as_deref(), Some("Deep"));
    }

    #[test]
    fn a_prepended_module_wins_over_the_class_itself() {
        let tree =
            one("module P\n  def go\n  end\nend\nclass W\n  prepend P\n  def go\n  end\nend\n");
        assert_eq!(
            find(&tree, "W", false, "go").as_deref(),
            Some("P"),
            "prepend puts P ahead of W in the chain, so P#go runs"
        );
    }

    #[test]
    fn a_singleton_chain_continues_into_class_and_module() {
        // `Foo.singleton_class.ancestors` does not stop at the superclass
        // walk. Without this tail, `Foo.new` and a class body's `prepend` find
        // nothing.
        let tree = one("class W\nend\nmodule M\nend\n");
        assert_eq!(find(&tree, "W", true, "new").as_deref(), Some("Class"));
        assert_eq!(find(&tree, "W", true, "prepend").as_deref(), Some("Module"));
        assert_eq!(find(&tree, "W", true, "puts").as_deref(), Some("Kernel"));
        assert_eq!(
            find(&tree, "M", true, "new"),
            None,
            "a module is not a Class, so it has no `new`"
        );
        assert_eq!(find(&tree, "M", true, "include").as_deref(), Some("Module"));
    }

    #[test]
    fn a_bare_visibility_call_does_not_answer_where_a_method_is_defined() {
        // `private :inherited` is a def row (DEC-004) but asserts visibility
        // about a method defined elsewhere; it must not be the answer.
        let tree =
            one("class Base\n  def shared\n  end\nend\nclass W < Base\n  private :shared\nend\n");
        assert_eq!(find(&tree, "W", false, "shared").as_deref(), Some("Base"));
    }

    #[test]
    fn an_alias_and_an_attr_are_definitions_that_answer() {
        let tree = one(
            "class W\n  attr_reader :size\n  def full\n  end\n  alias_method :whole, :full\nend\n",
        );
        assert_eq!(find(&tree, "W", false, "size").as_deref(), Some("W"));
        assert_eq!(find(&tree, "W", false, "whole").as_deref(), Some("W"));
    }

    #[test]
    fn def_on_a_named_constant_belongs_to_that_constant() {
        let tree = one("class Other\nend\nclass W\n  def Other.helper\n  end\nend\n");
        assert_eq!(
            find(&tree, "Other", true, "helper").as_deref(),
            Some("Other")
        );
        assert_eq!(find(&tree, "W", true, "helper"), None);
    }

    #[test]
    fn arity_admits_what_a_method_can_actually_take() {
        let tree = one("class W\n  def exact(a, b)\n  end\n  def loose(a, *rest)\n  end\nend\n");
        let exact = tree.lookup("W", false, "exact").unwrap();
        assert!(exact.accepts(Some(2)) && !exact.accepts(Some(1)));
        let loose = tree.lookup("W", false, "loose").unwrap();
        assert!(loose.accepts(Some(1)) && loose.accepts(Some(5)));
        assert!(
            exact.accepts(None),
            "a splat at the call site rules nothing out"
        );
    }
}

/// The path reported for anything defined in the core stub. Deliberately not a
/// real path: there is no file to open, and saying so is better than handing a
/// caller a location that does not exist.
pub(crate) const CORE_PATH: &str = "<core>";

/// The implicit superclass of every class that does not name one.
const OBJECT: &str = "Object";

/// Ruby's core library as rows, read from `core.rb` through the ordinary
/// extractor.
///
/// Reparsed on every tree build. It is ~1 ms against a ~120 ms build, and a
/// cache would have to be invalidated by the same rule DEC-013 exists for.
fn core_rows() -> (Vec<DeclRow>, Vec<EdgeRow>, Vec<MethodRow>) {
    rows_from(CORE_PATH, include_str!("core.rb"))
}

/// One Ruby source's facts, in the row shapes the tree assembles from. Shared
/// by the core stub and by the test harness, so neither can drift from what
/// the store actually hands over.
fn rows_from(path: &str, source: &str) -> (Vec<DeclRow>, Vec<EdgeRow>, Vec<MethodRow>) {
    use crate::core::Kind;
    let facts = crate::extract::extract(source.as_bytes());
    debug_assert_eq!(facts.parse_errors, 0, "{path} must be valid Ruby");
    let mut decls = Vec::new();
    let mut edges = Vec::new();
    let mut methods = Vec::new();
    for d in facts.defs {
        if d.kind == Kind::Method {
            methods.push(MethodRow {
                name: d.name,
                nesting: d.nesting,
                singleton: d.singleton,
                visibility: d.visibility.as_str().to_string(),
                params: d.params,
                via: d.via,
                target: d.target,
                sig_returns: d.sig_returns,
                path: path.to_string(),
                line: d.pos.line,
                col: d.pos.col,
            });
        } else {
            decls.push(DeclRow {
                name: d.name,
                kind: d.kind.as_str().to_string(),
                nesting: d.nesting,
                target: d.target,
                path: path.to_string(),
                line: d.pos.line,
                col: d.pos.col,
            });
        }
    }
    for a in facts.ancestry {
        edges.push(EdgeRow {
            owner: a.owner,
            relation: a.relation.as_str().to_string(),
            target: a.target,
        });
    }
    (decls, edges, methods)
}
