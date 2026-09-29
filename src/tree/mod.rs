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
//! **Built once per key, then mapped.** The assembled namespace is a flat
//! layout (`snapshot`) written beside the store (`files`); every later query
//! whose inputs are unchanged maps it instead of assembling (DEC-060/065).
//!
//! Semantics follow Shopify's Rubydex (MIT) `docs/ruby-behaviors.md`.

mod corelib;
#[cfg(test)]
mod dump;
mod files;
mod memo;
mod snapshot;
mod variants;

pub(crate) use variants::public_name;
use variants::{PlacedEdge, joinable, nearest};

pub(crate) use files::forget as forget_snapshots;
pub(crate) use files::sweep as sweep_snapshots;

use crate::core::{Param, runtime};
use crate::store::{DeclRow, EdgeRow, MethodRow, Roots, Store};
use memo::{Memo, Once};
use serde::Serialize;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

/// A name's declaration site — the answer to "where is this?".
#[derive(Clone, Debug, Serialize)]
pub(crate) struct Site {
    pub(crate) path: String,
    pub(crate) line: u32,
    pub(crate) col: u32,
    pub(crate) kind: String,
}

impl Site {
    /// An `.rbi` is a *declaration* — Sorbet's description of a method, never
    /// its implementation. A checkout that commits `sorbet/rbi/gems/` holds a
    /// stub for every gem method it uses, and those stubs are indexed after
    /// the gems themselves, so without this they win every lookup and send
    /// the caller to a signature instead of the code.
    pub(crate) fn is_rbi(&self) -> bool {
        self.path.ends_with(".rbi")
    }

    /// One of Tapioca's per-model DSL files (DEC-019). Matched anywhere in the
    /// path, because site paths are absolute.
    pub(crate) fn is_dsl_rbi(&self) -> bool {
        self.path.contains(DSL_RBI)
    }
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

/// An edge a module's hook sends to whatever mixes the module in by `how`.
#[derive(Clone, Debug)]
struct MixedEdge {
    how: String,
    relation: String,
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
    /// Declarations that disagree about it are two programs, and the name is
    /// split before this is set (DEC-072), so every edge left here agrees.
    superclass: Option<Target>,
    /// `extend M` — M's *instance* methods become this scope's singleton
    /// methods. A different chain from `include`, which is why it is a
    /// different field rather than another mixin kind.
    extends: Vec<Target>,
    /// `singleton_class.prepend(M)` — M's instance methods come before this
    /// scope's own singleton methods (DEC-101).
    singleton_prepends: Vec<Target>,
    /// `Bar = Foo` — the right-hand side as written. `Bar` is a constant in its
    /// own right and keeps its own site; but anywhere a *namespace* is needed
    /// the alias is followed through.
    alias_of: Option<Target>,
}

/// A mixin, superclass or alias target as a query reads it, from either
/// representation of the namespace.
#[derive(Clone, Debug)]
struct Written<'a> {
    name: &'a str,
    nesting: Vec<&'a str>,
}

impl Target {
    fn written(&self) -> Written<'_> {
        Written {
            name: &self.name,
            nesting: self.nesting.iter().map(String::as_str).collect(),
        }
    }
}

/// The constant namespace: a map while it is being assembled, and a flat
/// snapshot once it is done (DEC-060).
///
/// Assembly has to read the namespace it is still writing — placing
/// `class A::B` looks `A` up — so the lookups below serve both. Everything
/// outside `assemble` only ever sees a frozen one.
enum Names {
    Building(HashMap<String, Entry>),
    Frozen(snapshot::Snapshot),
}

#[derive(Clone, Copy)]
enum EntryRef<'a> {
    Building(&'a Entry),
    Frozen(snapshot::NameRef<'a>),
}

impl Names {
    fn get(&self, fqn: &str) -> Option<EntryRef<'_>> {
        match self {
            Names::Building(map) => map.get(fqn).map(EntryRef::Building),
            Names::Frozen(snap) => snap.find(fqn).map(EntryRef::Frozen),
        }
    }

    fn contains(&self, fqn: &str) -> bool {
        self.get(fqn).is_some()
    }

    fn for_each<'a>(&'a self, mut visit: impl FnMut(&'a str, EntryRef<'a>)) {
        match self {
            Names::Building(map) => map
                .iter()
                .for_each(|(fqn, entry)| visit(fqn, EntryRef::Building(entry))),
            Names::Frozen(snap) => snap
                .names()
                .for_each(|name| visit(name.fqn(), EntryRef::Frozen(name))),
        }
    }

    fn building(&mut self) -> &mut HashMap<String, Entry> {
        match self {
            Names::Building(map) => map,
            Names::Frozen(_) => unreachable!("only assembly writes the namespace"),
        }
    }
}

impl<'a> EntryRef<'a> {
    fn kind(self) -> &'a str {
        match self {
            EntryRef::Building(e) => &e.kind,
            EntryRef::Frozen(n) => n.kind(),
        }
    }

    fn sites(self) -> Vec<Site> {
        match self {
            EntryRef::Building(e) => e.sites.clone(),
            EntryRef::Frozen(n) => n.sites(),
        }
    }

    fn mixins(self) -> Vec<(MixinKind, Written<'a>)> {
        match self {
            EntryRef::Building(e) => e
                .mixins
                .iter()
                .map(|m| (m.kind, m.target.written()))
                .collect(),
            EntryRef::Frozen(n) => n.mixins(),
        }
    }

    fn extends(self) -> Vec<Written<'a>> {
        match self {
            EntryRef::Building(e) => e.extends.iter().map(Target::written).collect(),
            EntryRef::Frozen(n) => n.extends(false),
        }
    }

    fn singleton_prepends(self) -> Vec<Written<'a>> {
        match self {
            EntryRef::Building(e) => e.singleton_prepends.iter().map(Target::written).collect(),
            EntryRef::Frozen(n) => n.extends(true),
        }
    }

    fn superclass(self) -> Option<Written<'a>> {
        match self {
            EntryRef::Building(e) => e.superclass.as_ref().map(Target::written),
            EntryRef::Frozen(n) => n.superclass(),
        }
    }

    fn alias_of(self) -> Option<Written<'a>> {
        match self {
            EntryRef::Building(e) => e.alias_of.as_ref().map(Target::written),
            EntryRef::Frozen(n) => n.alias_of(),
        }
    }
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
    #[serde(skip)]
    pub(crate) sig_overloads: Vec<crate::core::Overload>,
    /// Lexical scope stack at the definition, innermost first: where a name
    /// its `sig` writes is looked up.
    #[serde(skip)]
    pub(crate) nesting: Vec<String>,
    /// Required positional arity and whether the method takes more than that.
    pub(crate) arity: (u32, bool),
    pub(crate) site: Site,
    /// Made here from a body written elsewhere — `define_method(:x,
    /// instance_method(:y))` — so this site is a declaration.
    #[serde(skip)]
    pub(crate) body_elsewhere: bool,
    /// The method a `delegate_missing_to` hands every unknown name to.
    pub(crate) forwards_to: Option<String>,
    /// An alias whose `site` is the body it copied, not the alias line.
    #[serde(skip)]
    pub(crate) bound: bool,
}

impl MethodDef {
    /// The class a call with this many arguments, and a block or not,
    /// returns — when its `sig`s say so (DEC-077).
    pub(crate) fn returns_for(&self, argc: Option<u32>, block: bool) -> Option<&str> {
        crate::core::returns_for(
            self.sig_returns.as_deref(),
            &self.sig_overloads,
            argc,
            block,
        )
    }

    /// Is the body at this location (DEC-034)?
    ///
    /// A Sorbet stub is a bodiless `def` — an ordinary definition by every
    /// syntactic test, and a description of a method that runs somewhere else.
    /// `Site::is_rbi` has said exactly that since DEC-019, and `kind` shipped
    /// in session 30 without asking it.
    pub(crate) fn kind(&self) -> Kind {
        if self.bound {
            return Kind::Definition;
        }
        if self.site.is_rbi()
            || self.body_elsewhere
            || is_rspec_stub(&self.site.path)
            || is_stdlib_stub(&self.site.path)
        {
            return Kind::Declaration;
        }
        Kind::of(self.via.as_deref())
    }

    /// What declared it, when it is a declaration. A macro's name if one made
    /// it — an `.rbi` can hold an `attr_reader` too — and otherwise `rbi`.
    pub(crate) fn declared_via(&self) -> Option<String> {
        match self.kind() {
            Kind::Definition => None,
            Kind::Declaration => self.via.clone().or_else(|| {
                if is_rspec_stub(&self.site.path) {
                    Some("rspec".to_string())
                } else if is_stdlib_stub(&self.site.path) {
                    Some("rbs".to_string())
                } else {
                    self.site.is_rbi().then(|| "rbi".to_string())
                }
            }),
        }
    }

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

/// Shared by every thread that asks it: apart from its build, a tree only
/// fills memos and loads names, and each of those is a function of the tree
/// and its key (DEC-200), so it does not matter which thread fills it first
/// (DEC-250). A field added here is one of three kinds, and says which:
///
/// - **set at build** and read-only after — a plain field;
/// - **a memo** — a `Memo`, or a `OnceLock` for one value; computed with no
///   lock held and the first value stored wins;
/// - **per call in flight** (the linearization stack, `placing`) — a
///   thread-local, never a field, since another thread's call is not this
///   one's.
pub(crate) struct Tree {
    /// Tells this tree's frames from another's on a thread's stack.
    id: u64,
    /// The checkout this tree was built for. Everything outside it came from a
    /// gem or from core, which is a ranking signal: code in the repo you are
    /// standing in is likelier to be what you meant than a dependency's.
    root: String,
    /// The Ruby's stdlib the checkout runs on, when it indexed one (DEC-180).
    stdlib: Option<String>,
    /// Core and the stdlib's compiled half, from that Ruby's signatures
    /// (DEC-240). None without them: then nothing is known of core.
    stubs: Option<std::sync::Arc<corelib::Stubs>>,
    /// What the stdlib's Ruby methods return, from RBS, by (owner, singleton,
    /// name), as each is first asked: lent to the real definitions, never a
    /// location (DEC-220).
    stdlib_sigs: Memo<(String, bool, String), Option<MethodDef>>,
    names: Names,
    /// Where methods come from when the tree does not already have them.
    /// `None` for a tree built from rows in hand (fixtures), which is fully
    /// eager and never loads anything.
    loader: Option<Loader>,
    /// The rows a name has before anything is loaded: the RSpec stub's,
    /// and — for a tree that loads nothing — every row it has.
    base: HashMap<String, Vec<MethodRow>>,
    /// Each name's definitions, loaded whole on first need. Demand-loading
    /// is per *name*, because that is what every caller keys on: a lookup, a
    /// residue candidate list and an override search all ask about one name
    /// (DEC-025). A name is complete once loaded — nothing else adds a
    /// definition of it — so its table never changes (DEC-202).
    defs: Once<String, Arc<Defs>>,
    /// Class-side lookup chains by (fqn, as_self): every lookup on a class
    /// walks the same one, and building it resolves each level's extends
    /// (DEC-203). An instance's chain is its memoized ancestry.
    singleton_chains: Memo<(String, bool), Pairs>,
    /// Lookups by (fqn, singleton, name, as_self, placing), final once the
    /// name is loaded (DEC-203). A lookup made while placing does not look
    /// for string macros, so it is a different question.
    lookups: Memo<LookupKey, Option<Landing>>,
    /// A model that overrides `self.table_name` wants the columns of a table
    /// whose conventional class it is not, so that carrier's methods are keyed
    /// onto the model as well. Built once at build time from the `table_name`
    /// definitions alone, because a per-name load cannot see the whole table.
    carriers: HashMap<String, Vec<String>>,
    /// Classes that have each module in their ancestor chain. Built lazily,
    /// because the common path never asks: it costs a pass over every name and
    /// only a call inside a module needs it.
    includers: OnceLock<Includers>,
    /// Module → the names that `include` or `prepend` it by name. Resolving
    /// every mixin edge once is far cheaper than linearizing every class,
    /// which is what `includers` pays.
    mixers: OnceLock<HashMap<String, Vec<String>>>,
    /// Every name's chain, as it is when that name is the one asked (DEC-200).
    ancestors: Memo<String, Memoized>,
    /// `agreed_return`, per (name, argc, block): a pure function of the
    /// tree, asked once per call site (DEC-201).
    agreed_returns: Memo<ReturnKey, Option<Arc<AgreedReturn>>>,
    /// The markers of scopes that define methods the source does not name
    /// (DEC-130), when handed over whole by a tree that loads nothing;
    /// otherwise read from the store on first need.
    dynamic_rows: Mutex<Option<Vec<EdgeRow>>>,
    /// The markers placed: by the scope's fully-qualified name, and by the
    /// file that writes them (DEC-162).
    dynamic: OnceLock<Placed>,
    /// Every marker with its owner resolved, in the store's order: what
    /// placing reads, and what `made_for` reads without placing (DEC-235).
    markers: OnceLock<Markers>,
    /// The classes whose body runs each macro, with the names each hands
    /// it and its file, by (owner, macro).
    callers: Memo<(String, String), Callers>,
    /// The methods a string macro in another file makes on the classes that
    /// hand it literal names (DEC-212): by name, then class, then side,
    /// worked out per name as a lookup misses it (DEC-235).
    made: Memo<String, Arc<Made>>,
    /// Hook name → the classes that run it (DEC-098), read on first need.
    hooks: OnceLock<HashMap<String, Vec<String>>>,
}

// Every tiering worker asks the one tree (DEC-250).
const _: fn() = || {
    fn shared<T: Send + Sync>() {}
    shared::<Tree>();
};

/// One name's definitions, in the order they were loaded: core's, the
/// stdlib's, then the store's.
struct Defs {
    methods: Vec<MethodDef>,
    /// owner → definitions, instance side then singleton. A reopened class
    /// gives several. Walking a chain probes by the owner's `&str` and builds
    /// no key (DEC-231).
    by_owner: Owners,
    /// `named`'s answer, built when first asked.
    named: OnceLock<Arc<[MethodDef]>>,
}

impl Defs {
    /// The definition a landing names.
    fn at<'a>(&'a self, landing: &'a Landing) -> &'a MethodDef {
        match &landing.at {
            At::Def(at) => &self.methods[*at],
            At::Made(made) => made,
        }
    }
}

/// The definition a landing names, with the owner it was found through.
fn land(defs: &Defs, landing: &Landing) -> MethodDef {
    let mut method = defs.at(landing).clone();
    method.owner = landing.owner.clone();
    method
}

/// The markers placed, by owner and by file.
struct Placed {
    by_owner: HashMap<String, Vec<Dynamic>>,
    by_file: HashMap<String, Vec<Dynamic>>,
}

/// Every tree's `id`.
static TREES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

thread_local! {
    /// Linearizations in progress on this thread, innermost last, each
    /// tagged with its tree. A name asked for again while its own frame is
    /// here is a cycle: through a superclass or mixin edge (not valid Ruby),
    /// or through `descend` resolving a mixin's path via the class's own
    /// ancestors (DEC-200). Per thread because a frame is one call's: the
    /// tree never hands work to another thread while one is open.
    static LINEARIZING: RefCell<Vec<Frame>> = const { RefCell::new(Vec::new()) };
    /// The tree whose `place_dynamic` or `made_for` is running on this
    /// thread, whose own lookups must not wait on it; 0 for none.
    static PLACING: Cell<u64> = const { Cell::new(0) };
}

/// Every marker with the class or module it marks.
type Markers = Arc<[(String, Dynamic)]>;

/// A class whose body calls a macro: the class, the literal names the call
/// hands it, and the file it is written in.
type Caller = (String, Vec<Option<String>>, String);

type Callers = Arc<[Caller]>;

/// A name's string-macro methods by class, instance side then singleton.
type Made = HashMap<String, [Option<Dynamic>; 2]>;

/// A maker that runs a string as code, whose `def`s a caller's names spell.
fn evals_code(by: &str) -> bool {
    matches!(by, "class_eval" | "module_eval" | "instance_eval" | "eval")
}

/// Whether a string macro marker could make a method named `name` at some
/// caller (`expanded`), whatever names the caller hands it: each `{k}` of
/// its shape stands for any name. Decides which macros' callers a name's
/// `made_for` has to find.
fn may_expand_to(maker: &crate::core::Maker, name: &str) -> bool {
    fn wildcard(shape: &str) -> String {
        let Some(open) = shape.find('{') else {
            return shape.to_string();
        };
        let Some(close) = shape[open..].find('}').map(|at| open + at) else {
            return shape.to_string();
        };
        format!("{}*{}", &shape[..open], wildcard(&shape[close + 1..]))
    }
    maker.via.is_some()
        && maker.singleton.is_some()
        && evals_code(&maker.by)
        && maker
            .shape
            .as_deref()
            .is_some_and(|shape| crate::core::shape_matches(&wildcard(shape), name))
}

/// The side and name of the one method a string macro's `def` makes at a
/// caller in another file, when the caller hands it every name the `def`
/// spells (DEC-212). The same file's caller had the string read with the
/// names at extraction (DEC-163), where it could be.
fn expanded(made: &Dynamic, called_in: &str) -> Option<(bool, String)> {
    let maker = &made.maker;
    let string = evals_code(&maker.by);
    let name = maker.shape.as_deref()?;
    let spelled = !name.contains(['*', '{']);
    if !string || !spelled || called_in == made.path {
        return None;
    }
    Some((maker.singleton?, name.to_string()))
}

/// Where a scope defines methods its source does not name: a
/// `define_method` whose name no literal spells, a `class_eval` string that
/// was not read (DEC-130).
#[derive(Clone, Debug)]
pub(crate) struct Dynamic {
    /// The method that does it — `define_method`, `class_eval` — with the
    /// side and name shape it can make (DEC-160).
    pub(crate) maker: crate::core::Maker,
    pub(crate) path: String,
    pub(crate) line: u32,
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

/// The class every definition of a name that declares a return agrees on.
pub(crate) struct AgreedReturn {
    pub(crate) fqn: String,
    /// How many owners' definitions declared it.
    pub(crate) agreeing: usize,
    /// How many owners define the name as an instance method.
    pub(crate) total: usize,
}

/// Every class with an ancestor, by that ancestor: `classes` sorted by
/// name, and each ancestor's includers as indices into it, ascending.
struct Includers {
    classes: Vec<String>,
    by_ancestor: HashMap<String, Vec<u32>>,
}

/// `(owner, singleton)` pairs, in the order a lookup walks them.
type Pairs = Arc<[(String, bool)]>;

/// The `(owner, singleton)` pairs a method lookup walks, in order: an
/// instance's ancestry as it is memoized, or a class side's own walk.
#[derive(Clone)]
pub(crate) enum Chain {
    Instance(Arc<Ancestry>),
    Singleton(Pairs),
}

impl Chain {
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&str, bool)> {
        let len = match self {
            Chain::Instance(ancestry) => ancestry.chain.len(),
            Chain::Singleton(pairs) => pairs.len(),
        };
        (0..len).map(move |at| match self {
            Chain::Instance(ancestry) => (ancestry.chain[at].as_str(), false),
            Chain::Singleton(pairs) => (pairs[at].0.as_str(), pairs[at].1),
        })
    }
}

/// A name's definitions by owner: indices into the methods, instance side
/// then singleton.
type Owners = HashMap<String, [Vec<usize>; 2]>;

/// A lookup, as `lookup_along` keys it: (fqn, singleton, name, as_self,
/// placing).
type LookupKey = (String, bool, String, bool, bool);

/// A call to a name, as `agreed_return` keys it: (name, argc, block).
type ReturnKey = (String, Option<u32>, bool);

/// One linearization in progress: the chain so far, and whether a cycle
/// under it makes the answer depend on who asked (DEC-200).
struct Frame {
    /// The tree this frame is linearizing for.
    tree: u64,
    fqn: String,
    /// The outermost frame a cycle under this one reached. Below this frame's
    /// own depth, its chain was built against a caller's partial one.
    low: usize,
    /// A cycle closed somewhere under this frame.
    cyclic: bool,
    prepends: Vec<String>,
    includes: Vec<String>,
    parent: Vec<String>,
}

impl Frame {
    /// What Ruby's `ancestors` would say at this point in the body: the
    /// superclass's chain and the mixins read so far.
    fn so_far(&self) -> Vec<String> {
        let mut chain = self.prepends.clone();
        chain.push(self.fqn.clone());
        chain.extend(self.includes.iter().cloned());
        chain.extend(self.parent.iter().cloned());
        chain
    }
}

/// A memoized chain. A `cyclic` one was built from inside a cycle it closed
/// itself, so it is the answer only when nothing else is in flight: a caller
/// already in that cycle would have been cut where this one was not.
#[derive(Clone)]
struct Memoized {
    ancestry: Arc<Ancestry>,
    cyclic: bool,
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

/// Report where the *lookup* landed, which is Ruby's own `defined_class`. For
/// an ordinary inherited method that is already the stored owner; it differs
/// only for a method re-keyed onto a model by a `self.table_name` override,
/// where the stored owner is the carrier class the convention invented — a
/// name no code declares and an agent cannot look up (DEC-022). A split
/// name's variant is its name.
/// Where a lookup lands: a definition, by its index in the tree's methods,
/// and the owner it was found through — a carrier's column lands on the
/// model that took its table.
#[derive(Clone)]
struct Landing {
    at: At,
    owner: String,
}

/// A definition a lookup landed on: one of the name's loaded ones, by its
/// index in the name's `Defs`, or one a string macro made (DEC-212).
#[derive(Clone)]
enum At {
    Def(usize),
    Made(Arc<MethodDef>),
}

fn landed(at: usize, owner: &str) -> Landing {
    Landing {
        at: At::Def(at),
        owner: public_name(owner).to_string(),
    }
}

/// Rails writes these into a module the model includes when it is made
/// (`GeneratedAttributeMethods`, `GeneratedAssociationMethods`, an enum's or a
/// store's own), so the class's own `def` of the name wins wherever it is
/// written (DEC-138).
fn into_generated_module(method: &MethodDef) -> bool {
    matches!(
        method.via.as_deref(),
        Some(
            "schema"
                | "enum"
                | "attribute"
                | "alias_attribute"
                | "belongs_to"
                | "has_one"
                | "has_many"
                | "has_and_belongs_to_many"
                | "has_one_attached"
                | "has_many_attached"
                | "accepts_nested_attributes_for"
                | "has_secure_password"
                | "store"
                | "store_accessor"
        )
    )
}

/// Is the body of the method where we are pointing, or did something else make
/// it there?
///
/// The store has always known this — a `def` row's `via` names the macro that
/// created it — and the answer has never said so, which is why a caller could
/// not tell `belongs_to :supplier` (the line a reader wants) from the line Ruby
/// actually runs. DEC-033 is what that cost: 112 honest declaration answers had
/// to be scored as errors because nothing in the response distinguished them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Kind {
    /// The body is here. A literal `def`; `define_method`'s block, which *is*
    /// the body; and `module_function`'s copy, which points at the `def` it
    /// copied.
    Definition,
    /// The name was brought into being here and runs elsewhere. A macro that
    /// generates methods (`belongs_to`, `enum`, a schema column), an alias
    /// whose body belongs to another method, or a bare `private :foo` that
    /// asserts only visibility (DEC-004).
    Declaration,
}

impl Kind {
    /// The test is "is the body at this location", not "was a macro involved".
    ///
    /// Takes only `via`, so it cannot see an `.rbi`, whose defs are ordinary
    /// bodiless ones. Prefer `MethodDef::kind`, which knows where the site is.
    pub(crate) fn of(via: Option<&str>) -> Kind {
        match via {
            None
            | Some("module_function")
            | Some("define_method")
            | Some("define_singleton_method")
            | Some("class_eval")
            | Some("module_eval") => Kind::Definition,
            Some(_) => Kind::Declaration,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Status {
    Resolved,
    /// One answer, and real competitors for it. Reached when a rung resting on
    /// a convention rather than on something the code states picked a winner
    /// that other definitions could equally have been — `@account.local?` names
    /// `Account`, and thirty other classes define `local?` too.
    ///
    /// The distinction is not cosmetic: `status` is what a caller branches on,
    /// and a `resolved` carrying 0.03 confidence invites exactly the trust the
    /// confidence is trying to withhold.
    Ambiguous,
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
    /// `definition` on the wire, always present (DEC-080).
    #[serde(rename = "definition")]
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
    pub(crate) fn build(store: &Store, root: &str) -> anyhow::Result<Tree> {
        // Gems sit before the checkout so a gem may reopen core and the
        // checkout may reopen a gem — which is what Rails actually does. The
        // ordering is carried by `checkout.id`, which rises with insert order,
        // so one query per fact kind serves all of them.
        //
        // Read from the store rather than re-located from disk. Locating gems
        // means a lockfile and ~200 stats against `GEM_HOME` and friends, and
        // doing it at *query* time made the tree depend on the environment the
        // query happened to run in: a query with a different `GEM_HOME` than
        // the index silently lost every gem. The index already worked this out
        // and wrote it down (DEC-029).
        let roots = roots(store, root)?;
        // Core and the stdlib's compiled half, from the Ruby's signatures
        // the index stored (DEC-240).
        let stubs = stubs(store, &roots)?;

        let mut phases = Phases::default();
        // The namespace is read from the checkout's snapshot when one answers
        // to what the store holds now, and assembled and written otherwise
        // (DEC-065). Methods are not in it: they stay demand-loaded below.
        let snapshot = match files::dir(store) {
            Some(dir) => {
                let key = files::key(store, &roots)?;
                let path = dir.join(files::name(root, &key));
                match phases.time("snapshot-load", || files::open(&path, &key)) {
                    Ok(snapshot) => snapshot,
                    Err(miss) => {
                        phases.snapshot = miss.to_string();
                        // How often a query pays for the assembly — what
                        // decides whether building it earlier would pay.
                        crate::usage::flag("tree-built");
                        let names = Tree::namespace(store, &roots, stubs.as_deref(), &mut phases)?;
                        let bytes = snapshot::encode(&names, &key)?;
                        phases.mark("snapshot-encode");
                        // Freeing a namespace's worth of strings is a third
                        // of a second at 30×; nothing here waits for it.
                        std::thread::spawn(move || drop(names));
                        let snapshot = files::save(&dir, root, &key, bytes);
                        phases.mark("snapshot-write");
                        snapshot
                    }
                }
            }
            // An in-memory store has nowhere to keep one.
            None => freeze(&Tree::namespace(
                store,
                &roots,
                stubs.as_deref(),
                &mut phases,
            )?)?,
        };
        let mut tree = Tree::over(snapshot, root.to_string());
        tree.stdlib = roots.stdlib.clone();
        tree.stubs = stubs;
        // The RSpec stub's methods, before the index's, so a method rspec
        // defines itself wins over the stub's (DEC-087). Core's are loaded
        // by name, first, as the index's are (DEC-240).
        let mut methods = Vec::new();
        if tree.kind_of(crate::core::rspec::EXAMPLE_GROUP) == Some("class") {
            methods.extend(rspec_rows().1);
        }

        // The checkout's methods are *not* loaded here. Nothing needs all of
        // them, and fetching and indexing rails' 84,052 was 76 % of this build
        // (DEC-025). Two things still have to happen up front:
        match store.reopen()? {
            Some(own) => {
                // Which models override `self.table_name`, because a per-name
                // load cannot see the whole table to work it out later.
                let table_names =
                    phases.time("table-names", || store.methods_named(&roots, "table_name"))?;
                tree.carriers = tree.carriers_from(&table_names);
                // And the RSpec stub's, which a per-name load would never
                // find.
                phases.methods = methods.len();
                tree.add_base(methods);
                // Loaded here, and never from the store again.
                let mut rows = tree.base.get("table_name").cloned().unwrap_or_default();
                rows.extend(table_names);
                tree.defs
                    .set("table_name".to_string(), Arc::new(tree.defs_of(rows)));
                tree.loader = Some(Loader::new(own, roots));
                phases.mark("core-and-table-names");
            }
            // An in-memory store cannot be reopened, so there is nothing to
            // load from later: take everything now and stay eager.
            None => {
                tree.dynamic_rows = Mutex::new(Some(store.dynamic_markers(&roots)?));
                let mut rows = tree.all_stub_rows();
                rows.append(&mut methods);
                methods = rows;
                methods.extend(store.methods(&roots)?);
                phases.methods = methods.len();
                tree.add_methods(methods);
                phases.mark("index-methods");
            }
        }
        phases.report();
        Ok(tree)
    }

    /// One checkout's namespace alone — no core, no gems, nothing to load
    /// later — for reading its facts as a tree reads them: the stdlib's, when
    /// its signatures are generated (DEC-240).
    pub(crate) fn alone(store: &Store, root: &str) -> anyhow::Result<Tree> {
        let roots = Roots {
            list: vec![root.to_string()],
            ..Roots::default()
        };
        let names = Tree::namespace(store, &roots, None, &mut Phases::default())?;
        Ok(Tree::over(freeze(&names)?, root.to_string()))
    }

    /// The class or module a method row defines its method on.
    pub(crate) fn owner(&self, row: &MethodRow) -> String {
        self.owner_of(row)
    }

    /// Write this checkout's snapshot if none answers to what the store holds
    /// now, so the first query after an index maps it rather than assembling
    /// it (DEC-192). Whether it built one. A file under the right name is
    /// trusted to exist here; a query still verifies it, and rebuilds one that
    /// does not check out.
    pub(crate) fn prepare(store: &Store, root: &str) -> anyhow::Result<bool> {
        let Some(dir) = files::dir(store) else {
            return Ok(false);
        };
        let roots = roots(store, root)?;
        let key = files::key(store, &roots)?;
        if dir.join(files::name(root, &key)).exists() {
            return Ok(false);
        }
        let stubs = stubs(store, &roots)?;
        let mut phases = Phases::default();
        let names = Tree::namespace(store, &roots, stubs.as_deref(), &mut phases)?;
        let bytes = snapshot::encode(&names, &key)?;
        // The process ends soon after; freeing the namespace string by string
        // buys nothing (DEC-054).
        std::mem::forget(names);
        files::publish(&dir, root, &key, &bytes);
        Ok(true)
    }

    /// What this checkout's whole tree is a function of — its namespace and
    /// every method it loads on demand, across its own files and every gem
    /// its bundle names — as a stamp that moves whenever a rebuilt tree
    /// could answer differently (DEC-065). A method edit moves it and not
    /// the snapshot's key, so the rebuild maps the snapshot again (DEC-194).
    pub(crate) fn stamp(store: &Store, root: &str) -> anyhow::Result<[u8; 20]> {
        files::stamp(store, &roots(store, root)?)
    }

    /// Core's declarations and edges plus the store's, assembled. Core goes
    /// in first, so that a checkout reopening `class Object` adds to it rather
    /// than being shadowed by it, and so that every class ends up with an
    /// Object/Kernel/BasicObject tail.
    fn namespace(
        store: &Store,
        roots: &Roots,
        stubs: Option<&corelib::Stubs>,
        phases: &mut Phases,
    ) -> anyhow::Result<HashMap<String, Entry>> {
        let (mut decls, mut edges) = stubs.map(core_rows).unwrap_or_default();
        decls.extend(phases.time("declarations", || store.declarations(roots))?);
        edges.extend(phases.time("ancestry", || store.ancestry(roots))?);
        if let Some(stubs) = stubs {
            let (stub_decls, stub_edges) = compiled_classes(stubs, &decls);
            decls.extend(stub_decls);
            edges.extend(stub_edges);
        }
        // Last, so its mixins are the nearest: RSpec includes them after the
        // class body has run.
        if declares_example_group(&decls) {
            edges.extend(rspec_rows().0);
        }
        let programs = store.program_roots(roots)?;
        phases.decls = decls.len();
        let names = Tree::assemble(decls, edges, &programs);
        phases.mark("assemble");
        Ok(names)
    }

    /// A tree from rows in hand, with nothing to load later.
    #[cfg(test)]
    fn from_rows(decls: Vec<DeclRow>, edges: Vec<EdgeRow>, programs: &[String]) -> Tree {
        let names = Tree::assemble(decls, edges, programs);
        Tree::over(
            freeze(&names).expect("a test namespace fits"),
            String::new(),
        )
    }

    /// A tree answering from this namespace, with nothing memoized yet.
    fn over(snapshot: snapshot::Snapshot, root: String) -> Tree {
        Tree::with_names(Names::Frozen(snapshot), root)
    }

    fn with_names(names: Names, root: String) -> Tree {
        Tree {
            id: TREES.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            root,
            stdlib: None,
            stubs: None,
            stdlib_sigs: Memo::new(),
            names,
            base: HashMap::new(),
            defs: Once::new(),
            singleton_chains: Memo::new(),
            lookups: Memo::new(),
            loader: None,
            carriers: HashMap::new(),
            includers: OnceLock::new(),
            mixers: OnceLock::new(),
            ancestors: Memo::new(),
            agreed_returns: Memo::new(),
            dynamic_rows: Mutex::new(None),
            dynamic: OnceLock::new(),
            markers: OnceLock::new(),
            callers: Memo::new(),
            made: Memo::new(),
            hooks: OnceLock::new(),
        }
    }

    /// The namespace a set of declarations and edges add up to.
    ///
    /// Assembled in a scratch tree, because placing a declaration uses the
    /// same lookups a query does. Whatever that scratch tree memoized along
    /// the way was computed against a half-built namespace, and goes with it.
    fn assemble(
        decls: Vec<DeclRow>,
        edges: Vec<EdgeRow>,
        programs: &[String],
    ) -> HashMap<String, Entry> {
        let mut tree = Tree::with_names(Names::Building(HashMap::new()), String::new());
        // A scope's markers say nothing about its namespace (DEC-130).
        let edges: Vec<EdgeRow> = edges
            .into_iter()
            .filter(|edge| edge.relation != "dynamic")
            .collect();

        // Placing a name can depend on a name not placed yet: `class A::B`
        // needs `A`, and `A` may itself have been written compactly. So settle
        // the key set first, iterating until nothing new appears — each round
        // only adds, so it terminates. Sites are attached in the second pass,
        // which is why an early misplacement leaves no residue behind.
        // Where a declaration lands is read out of `self.names` in exactly one
        // case: a **compact path** (`class A::B`), whose prefix goes through
        // ordinary constant lookup and may only be placeable once something
        // later declares it. A declaration written with plain names — its own
        // and every scope it sits in — is placed by string arithmetic alone,
        // so its answer in round one is its answer forever.
        //
        // That is the whole optimisation, and it is deliberately an
        // over-approximation: "mentions `::` anywhere" is coarser than "could
        // actually still move", and cheap to convince yourself of. The
        // assembled namespace is byte-identical on rails, discourse and
        // widget_shop; a cleverer predicate would buy a few more milliseconds
        // and cost that confidence.
        let movable: Vec<bool> = decls
            .iter()
            .map(|decl| decl.name.contains("::") || decl.nesting.iter().any(|s| s.contains("::")))
            .collect();
        // Round one's answer for every declaration. Final for the ones that
        // cannot move, so the pass that attaches sites reuses it rather than
        // placing all of them a second time.
        let mut placed: Vec<String> = Vec::with_capacity(decls.len());

        let mut rounds = 0;
        let t0 = std::time::Instant::now();
        let mut first = true;
        loop {
            rounds += 1;
            let before = tree.names.building().len();
            // Round one settles every declaration; later rounds revisit only
            // the ones whose placement can still change.
            for (decl, _) in decls.iter().zip(&movable).filter(|(_, m)| first || **m) {
                let nesting = tree.scopes(&decl.nesting);
                let fqn = tree.place(&decl.name, &nesting);
                if first {
                    placed.push(fqn.clone());
                }
                // Kind and alias are settled here, not with the sites: placing
                // `class ALIAS::Bar` has to be able to follow `ALIAS` already.
                tree.declare_key(fqn, decl, nesting);
            }
            first = false;
            if tree.names.building().len() == before {
                break;
            }
        }
        let t1 = std::time::Instant::now();
        if std::env::var("TREKR_PROFILE").is_ok() {
            eprintln!(
                "  fixpoint: {rounds} rounds — {} declarations once, {} revisited — {:.0}ms, {} names",
                decls.len(),
                movable.iter().filter(|m| **m).count(),
                (t1 - t0).as_secs_f64() * 1000.0,
                tree.names.building().len()
            );
        }
        for ((decl, movable), placed) in decls.iter().zip(movable).zip(placed) {
            let fqn = if movable {
                tree.place_decl(decl)
            } else {
                placed
            };
            tree.declare(fqn, decl);
        }
        tree.imply_namespaces();

        // Which classes run each `on_load` hook (DEC-098), settled first
        // because a hook's mixins land on them.
        let mut hook_bases: HashMap<String, Vec<String>> = HashMap::new();
        for edge in edges.iter().filter(|edge| edge.relation == "load_hooks") {
            if let Some((base, _)) = tree.edge_owner(&edge.owner) {
                let bases = hook_bases.entry(edge.target.clone()).or_default();
                if !bases.contains(&base) {
                    bases.push(base);
                }
            }
        }
        // What a module's `included`/`extended`/`prepended` hook sends to
        // whatever mixes it in (DEC-102), by the module, settled once every
        // body's edges are attached.
        let mut mixed: HashMap<String, Vec<MixedEdge>> = HashMap::new();
        let edges: Vec<EdgeRow> = edges
            .into_iter()
            .filter(|edge| {
                let Some(how) = edge.owner.first().and_then(|s| runtime::mixed_by(s)) else {
                    return true;
                };
                let written = tree.scopes(&edge.owner[1..]);
                if let Some(module) = written.first() {
                    mixed.entry(module.clone()).or_default().push(MixedEdge {
                        how: how.to_string(),
                        relation: edge.relation.clone(),
                        target: Target {
                            name: edge.target.clone(),
                            nesting: written.clone(),
                        },
                    });
                }
                false
            })
            .collect();
        let mut edges: Vec<(bool, PlacedEdge)> = edges
            .into_iter()
            .filter(|edge| edge.relation != "load_hooks")
            .flat_map(|edge| {
                let runtime = edge.owner.first().is_some_and(|s| runtime::is_runtime(s));
                let hook = edge.owner.first().and_then(|s| runtime::hook_name(s));
                let owners: Vec<(String, Vec<String>)> = match hook {
                    // Both the hook's classes and the module are read where
                    // the block is written.
                    Some(hook) => {
                        let written = tree.scopes(&edge.owner[1..]);
                        let bases = hook_bases.get(hook).map(Vec::as_slice).unwrap_or_default();
                        bases
                            .iter()
                            .map(|base| (base.clone(), written.clone()))
                            .collect()
                    }
                    None => tree.edge_owner(&edge.owner).into_iter().collect(),
                };
                owners.into_iter().map(move |(scope, nesting)| {
                    // Ruby evaluates a superclass expression *outside* the
                    // class body: `class C < Base` looks up `Base` where `C`
                    // is written, not where `C`'s constants live. Every other
                    // relation is written inside.
                    let nesting = if edge.relation == "superclass" {
                        nesting.get(1..).unwrap_or_default().to_vec()
                    } else {
                        nesting
                    };
                    let placed = PlacedEdge {
                        scope,
                        relation: edge.relation.clone(),
                        target: Target {
                            name: edge.target.clone(),
                            nesting,
                        },
                        path: edge.path.clone(),
                    };
                    (runtime, placed)
                })
            })
            .collect();
        // A mixin sent to a class, or run by its hook, runs after the class
        // body that defines it, whichever file sorts first — and the order of
        // a class's mixins is the order of its ancestors.
        edges.sort_by_key(|(runtime, _)| *runtime);
        let edges: Vec<PlacedEdge> = edges.into_iter().map(|(_, edge)| edge).collect();

        // A name declared with two different superclasses is two classes in
        // two programs (DEC-072), so it is split before anything attaches to it.
        let split = tree.split_conflicts(&edges, programs);
        let mut superclasses: HashMap<String, Vec<Target>> = HashMap::new();
        for edge in edges {
            let owners: Vec<String> = match split.get(&edge.scope) {
                None => vec![edge.scope.clone()],
                Some(variants) if edge.relation == "superclass" => {
                    let group = tree.superclass_group(&edge.target);
                    variants
                        .iter()
                        .filter(|v| v.group == group)
                        .map(|v| v.key.clone())
                        .collect()
                }
                Some(variants) => nearest(
                    &edge.scope,
                    joinable(variants, &edge.path, programs).map(|v| (&v.anchors, v)),
                    &edge.path,
                )
                .into_iter()
                .map(|v| v.key.clone())
                .collect(),
            };
            let target = tree.aim(edge.target, &edge.path, &split);
            for owner in owners {
                if edge.relation == "superclass" {
                    superclasses.entry(owner).or_default().push(target.clone());
                    continue;
                }
                let entry = tree.names.building().entry(owner).or_default();
                match edge.relation.as_str() {
                    "prepend" => entry.mixins.push(Mixin {
                        kind: MixinKind::Prepend,
                        target: target.clone(),
                    }),
                    "include" => entry.mixins.push(Mixin {
                        kind: MixinKind::Include,
                        target: target.clone(),
                    }),
                    "extend" => entry.extends.push(target.clone()),
                    "singleton_prepend" => entry.singleton_prepends.push(target.clone()),
                    _ => continue,
                };
            }
        }
        // Every `class X < Y` of one class names the same superclass at run
        // time, or Ruby raises; what differs is whether the index can read it.
        // A computed one (`Impl = case …`) reads as nothing, so the first that
        // names a class is taken, whatever order the layers put them in.
        for (owner, mut written) in superclasses {
            let at = written
                .iter()
                .position(|target| tree.names_a_class(target))
                .unwrap_or(0);
            tree.names.building().entry(owner).or_default().superclass =
                Some(written.swap_remove(at));
        }
        tree.apply_mixed(&mixed);
        // Each half keeps the declarations nearest it. The name itself keeps
        // them all: it is still where `Post` is written.
        for (base, variants) in &split {
            let sites = tree.names.building()[base].sites.clone();
            for site in sites {
                let joining = joinable(variants, &site.path, programs);
                let near = nearest(base, joining.map(|v| (&v.anchors, v)), &site.path);
                for variant in near {
                    tree.names
                        .building()
                        .get_mut(&variant.key)
                        .expect("variants are declared by the split")
                        .sites
                        .push(site.clone());
                }
            }
        }
        std::mem::take(tree.names.building())
    }

    /// Give every scope that mixes in a module with a hook what the hook
    /// sends its `base` (DEC-102): `include Tracking` gains the `Helpers`
    /// that `Tracking.included` includes, right after `Tracking`, as Ruby
    /// inserts it. Only a direct mixer gets them, since the hook runs with it
    /// as `base`; a class that includes a module that includes `Tracking`
    /// has the includes through that module's chain, and not the extends.
    /// What a hook adds can have a hook of its own, and each (scope, module)
    /// pair is applied once, which is what ends the walk.
    fn apply_mixed(&mut self, mixed: &HashMap<String, Vec<MixedEdge>>) {
        if mixed.is_empty() {
            return;
        }
        let mut scopes: Vec<String> = self.names.building().keys().cloned().collect();
        scopes.sort();
        for scope in scopes {
            let mut applied: HashSet<(&str, String)> = HashSet::new();
            loop {
                let entry = &self.names.building()[&scope];
                let written = entry
                    .mixins
                    .iter()
                    .enumerate()
                    .map(|(at, m)| {
                        let how = match m.kind {
                            MixinKind::Prepend => "prepend",
                            MixinKind::Include => "include",
                        };
                        (how, Some(at), m.target.clone())
                    })
                    .chain(entry.extends.iter().map(|t| ("extend", None, t.clone())))
                    .collect::<Vec<_>>();
                let next = written.into_iter().find_map(|(how, at, target)| {
                    let module = self
                        .resolve_lexical(&target.name, &target.nesting)
                        .map(|fqn| self.namespace_of(&fqn))?;
                    let hooked = mixed.get(&module)?.iter().any(|e| e.how == how);
                    (hooked && !applied.contains(&(how, module.clone())))
                        .then_some((how, at, module))
                });
                let Some((how, at, module)) = next else {
                    break;
                };
                let entry = self
                    .names
                    .building()
                    .get_mut(&scope)
                    .expect("the scope was listed");
                let mut after = at.map(|at| at + 1);
                for edge in mixed[&module].iter().filter(|e| e.how == how) {
                    let target = edge.target.clone();
                    let kind = match edge.relation.as_str() {
                        "include" => MixinKind::Include,
                        "prepend" => MixinKind::Prepend,
                        "extend" => {
                            entry.extends.push(target);
                            continue;
                        }
                        "singleton_prepend" => {
                            entry.singleton_prepends.push(target);
                            continue;
                        }
                        _ => continue,
                    };
                    let mixin = Mixin { kind, target };
                    match after {
                        Some(at) => {
                            entry.mixins.insert(at, mixin);
                            after = Some(at + 1);
                        }
                        None => entry.mixins.push(mixin),
                    }
                }
                applied.insert((how, module));
            }
        }
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
    ///
    /// An RSpec example group is no constant scope, and is read past (DEC-084).
    fn scopes(&self, written: &[String]) -> Vec<String> {
        let mut qualified: Vec<String> = Vec::new();
        // Outermost first: each scope is placed in the ones already built.
        for name in written
            .iter()
            .rev()
            .filter(|name| !crate::core::rspec::is_group(name))
        {
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
            if self.names.contains(&candidate) {
                return qualify(&self.namespace_of(&candidate), last);
            }
        }
        // Unknown prefix — Ruby would raise, but a partial index reaches here
        // constantly because the prefix belongs to a gem. Top level is the
        // honest guess.
        qualify(prefix, last)
    }

    /// The scope an edge attaches to, and the nesting its target is written
    /// in: the body the owner opens, or, for a mixin sent to a constant
    /// (DEC-097), the class that constant names, with the nesting the call
    /// is written in. A receiver the tree does not hold has no chain to join.
    fn edge_owner(&self, owner: &[String]) -> Option<(String, Vec<String>)> {
        match owner.first().and_then(|s| runtime::sent_to(s)) {
            Some(receiver) => {
                let written = self.scopes(&owner[1..]);
                Some((self.find_scope(receiver, &written)?, written))
            }
            None => {
                let scopes = self.scopes(owner);
                Some((scopes.first().cloned().unwrap_or_default(), scopes))
            }
        }
    }

    /// The class or module a constant written in `scopes` names, looked up
    /// through the lexical scopes and the top level only: while edges are
    /// being attached, no chain is complete enough to search.
    fn find_scope(&self, written: &str, scopes: &[String]) -> Option<String> {
        let (head, rest) = split_path(written);
        let mut current = match head.strip_prefix("::") {
            Some(head) => self.names.contains(head).then(|| head.to_string()),
            None => scopes
                .iter()
                .map(String::as_str)
                .chain([""])
                .map(|scope| qualify(scope, head))
                .find(|candidate| self.names.contains(candidate)),
        }?;
        for segment in rest {
            let next = qualify(&self.namespace_of(&current), segment);
            current = self.names.contains(&next).then_some(next)?;
        }
        Some(self.namespace_of(&current))
    }

    /// Create the namespaces that declarations imply but nothing declares.
    ///
    /// Rails' autoloader invents a module from a directory: mastodon writes
    /// `class ActivityPub::TagManager` in `app/lib/activitypub/tag_manager.rb`
    /// and never writes `module ActivityPub` anywhere. Plain Ruby would raise;
    /// under Zeitwerk the constant exists, so an index that omits it cannot
    /// resolve a reference to it — which is most of what mastodon's residue
    /// turned out to be.
    ///
    /// These entries carry **no sites**, which is the truth: the name exists
    /// and no line of code declares it.
    fn imply_namespaces(&mut self) {
        let names = self.names.building();
        let mut implied: Vec<String> = Vec::new();
        for fqn in names.keys() {
            let mut prefix = fqn.as_str();
            while let Some((parent, _)) = prefix.rsplit_once("::") {
                if !names.contains_key(parent) {
                    implied.push(parent.to_string());
                }
                prefix = parent;
            }
        }
        for fqn in implied {
            let entry = names.entry(fqn).or_default();
            if entry.kind.is_empty() {
                entry.kind = "module".to_string();
            }
        }
    }

    /// Everything about a name except where it is written. Idempotent, so the
    /// placement loop can run it as many times as it needs to.
    fn declare_key(&mut self, fqn: String, decl: &DeclRow, nesting: Vec<String>) {
        let entry = self.names.building().entry(fqn).or_default();
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
        self.names
            .building()
            .entry(fqn)
            .or_default()
            .sites
            .push(Site {
                path: decl.path.clone(),
                line: decl.line,
                col: decl.col,
                kind: decl.kind.clone(),
            });
    }

    pub(crate) fn sites(&self, fqn: &str) -> Vec<Site> {
        self.names.get(fqn).map(EntryRef::sites).unwrap_or_default()
    }
}

impl Tree {
    /// Is `ancestor` somewhere in `fqn`'s chain other than `fqn` itself? A
    /// split name's variants count as the name.
    pub(crate) fn inherits(&self, fqn: &str, ancestor: &str) -> bool {
        let ancestor = public_name(ancestor);
        public_name(fqn) != ancestor
            && self
                .ancestors(fqn)
                .chain
                .iter()
                .any(|a| public_name(a) == ancestor)
    }

    /// Does some class that has `module` in its chain have `other` ahead of
    /// `landing`, the owner a lookup from the module found (or anywhere, when
    /// it found none)? A call on `self` in the module then runs on an object
    /// where Ruby finds `other`'s method first, though the module's own chain
    /// never names it. Behind the landing, it is shadowed.
    pub(crate) fn includer_reaches(
        &self,
        module: &str,
        other: &str,
        landing: Option<&str>,
    ) -> bool {
        self.includers_of(module).iter().any(|class| {
            let chain = &self.ancestors(class).chain;
            let at = |name: &str| {
                chain
                    .iter()
                    .position(|a| public_name(a) == public_name(name))
            };
            match (at(other), landing.map(at)) {
                (Some(other), Some(Some(landing))) => other < landing,
                (Some(_), _) => true,
                (None, _) => false,
            }
        })
    }

    /// The ancestor chain of a name, in Ruby's linearization order:
    /// `[prepends, self, includes, superclass's chain]`, with the first
    /// occurrence of each module winning.
    pub(crate) fn ancestors(&self, fqn: &str) -> Arc<Ancestry> {
        self.linearized(fqn, false)
    }

    /// Memoized per name, sub-chains included, so that each name is
    /// linearized once per tree however many classes inherit it (DEC-200).
    ///
    /// A name asked again while it is being linearized closes a cycle. Through
    /// a superclass or mixin edge (`structural`) it answers empty, which lets a
    /// half-written index finish. Through a lookup — `include Kramdown::Html`
    /// in a class nested as `Kramdown::Parser::Kramdown`, where `Kramdown`
    /// names the class itself and `::Html` is found through its ancestors — it
    /// answers the chain so far, as Ruby's `ancestors` would at that line.
    ///
    /// Every answer is the one a fresh tree gives when this name is asked
    /// first: a chain built against an outer frame's partial chain is not
    /// memoized, and one that closed a cycle of its own is reused only when
    /// nothing else is in flight.
    fn linearized(&self, fqn: &str, structural: bool) -> Arc<Ancestry> {
        if let Some(memo) = self.ancestors.get(fqn)
            && (!memo.cyclic || !self.in_flight())
        {
            return memo.ancestry;
        }
        let depth = LINEARIZING.with_borrow_mut(|frames| {
            if let Some(at) = frames
                .iter()
                .position(|frame| frame.tree == self.id && frame.fqn == fqn)
            {
                let so_far = (!structural).then(|| frames[at].so_far());
                let top = frames.last_mut().expect("a frame is in flight");
                top.low = top.low.min(at);
                top.cyclic = true;
                return Err(Arc::new(Ancestry {
                    chain: so_far.unwrap_or_default(),
                    unresolved: Vec::new(),
                }));
            }
            let depth = frames.len();
            frames.push(Frame {
                tree: self.id,
                fqn: fqn.to_string(),
                low: depth,
                cyclic: false,
                prepends: Vec::new(),
                includes: Vec::new(),
                parent: Vec::new(),
            });
            Ok(depth)
        });
        let depth = match depth {
            Ok(depth) => depth,
            Err(cut) => return cut,
        };
        let mut out = Ancestry::default();
        out.chain = self.linearize(fqn, &mut out);
        let frame = LINEARIZING.with_borrow_mut(|frames| {
            let frame = frames.pop().expect("this call's frame");
            if let Some(caller) = frames.last_mut().filter(|caller| caller.tree == self.id) {
                caller.low = caller.low.min(frame.low);
                caller.cyclic |= frame.cyclic;
            }
            frame
        });
        let ancestry = Arc::new(out);
        if frame.low >= depth {
            let memo = Memoized {
                ancestry: ancestry.clone(),
                cyclic: frame.cyclic,
            };
            self.ancestors.publish(fqn.to_string(), memo);
        }
        ancestry
    }

    /// Whether this thread is linearizing a name of this tree.
    fn in_flight(&self) -> bool {
        LINEARIZING.with_borrow(|frames| frames.iter().any(|frame| frame.tree == self.id))
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
    ///
    /// Works on the innermost frame's lists, which are what a lookup that
    /// comes back to this name sees as its chain so far.
    fn linearize(&self, fqn: &str, out: &mut Ancestry) -> Vec<String> {
        let entry = self.names.get(fqn);

        // A split name has no ancestry of its own to give: which superclass it
        // has depends on which program is running (DEC-072).
        let conflicting = self.conflicting_superclasses(fqn);
        if !conflicting.is_empty() {
            for name in conflicting {
                if !out.unresolved.contains(&name) {
                    out.unresolved.push(name);
                }
            }
            return vec![fqn.to_string()];
        }

        // The parent chain is needed before includes, because includes dedup
        // against it.
        let parent: Vec<String> = match entry.and_then(EntryRef::superclass) {
            Some(target) => self.chain_of(&target, out),
            // Every class without an explicit superclass inherits Object, and
            // that tail is most of what core indexing buys: it is how `puts`
            // and `raise` become findable from an ordinary class body.
            None if self.inherits_object(fqn, entry) => self.sub_chain(OBJECT, out),
            None => Vec::new(),
        };
        self.innermost(|frame| frame.parent = parent);

        for (kind, target) in entry.map(EntryRef::mixins).unwrap_or_default() {
            let mut ids = self.chain_of(&target, out);
            self.innermost(|frame| match kind {
                MixinKind::Prepend => {
                    let prepends = &mut frame.prepends;
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
                        !frame.prepends.contains(id)
                            && !frame.includes.contains(id)
                            && !frame.parent.contains(id)
                    });
                    for id in ids.into_iter().rev() {
                        frame.includes.insert(0, id);
                    }
                }
            });
        }

        self.innermost(|frame| {
            let mut chain = std::mem::take(&mut frame.prepends);
            chain.push(fqn.to_string());
            chain.append(&mut frame.includes);
            chain.append(&mut frame.parent);
            chain
        })
    }

    /// The frame `linearize` is working in: the last one, since every frame
    /// pushed under it has been popped by the time it resumes.
    fn innermost<T>(&self, work: impl FnOnce(&mut Frame) -> T) -> T {
        LINEARIZING.with_borrow_mut(|frames| {
            work(frames.last_mut().expect("linearize runs inside its frame"))
        })
    }

    /// A superclass's or mixin's whole chain, with what it could not resolve
    /// added to `out`'s, in the order a single walk would have met them.
    fn sub_chain(&self, fqn: &str, out: &mut Ancestry) -> Vec<String> {
        let sub = self.linearized(fqn, true);
        for name in &sub.unresolved {
            if !out.unresolved.contains(name) {
                out.unresolved.push(name.clone());
            }
        }
        sub.chain.clone()
    }

    /// Does this name get Ruby's implicit `< Object`?
    ///
    /// Only classes — a module has no superclass at all — and not the two
    /// roots, whose own chain the core stub states outright.
    fn inherits_object(&self, fqn: &str, entry: Option<EntryRef>) -> bool {
        entry.is_some_and(|e| e.kind() == "class")
            && fqn != OBJECT
            && fqn != "BasicObject"
            && self.names.contains(OBJECT)
    }

    /// One mixin or superclass target: its own whole chain, or nothing plus a
    /// note that we could not see it.
    fn chain_of(&self, target: &Written, out: &mut Ancestry) -> Vec<String> {
        match self.resolve_lexical(target.name, &target.nesting) {
            Some(fqn) => {
                let fqn = self.namespace_of(&fqn);
                self.sub_chain(&fqn, out)
            }
            // `class Widget < ActiveRecord::Base` in a checkout with no gems
            // indexed. The chain stops here, and the answer says so.
            None => {
                if !out.unresolved.iter().any(|u| u == target.name) {
                    out.unresolved.push(target.name.to_string());
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
    fn resolve_lexical(&self, written: &str, nesting: &[impl AsRef<str>]) -> Option<String> {
        let (head, rest) = split_path(written);
        let mut current = if let Some(head) = head.strip_prefix("::") {
            self.names.contains(head).then(|| head.to_string())
        } else {
            nesting
                .iter()
                .map(AsRef::as_ref)
                .chain([""])
                .map(|scope| qualify(scope, head))
                .find(|candidate| self.names.contains(candidate))
        }?;
        for segment in rest {
            current = self.descend(&current, segment)?;
        }
        Some(current)
    }

    /// Does a superclass as written resolve, before any ancestry is attached,
    /// to a class?
    fn names_a_class(&self, target: &Target) -> bool {
        self.resolve_lexical(&target.name, &target.nesting)
            .map(|fqn| self.namespace_of(&fqn))
            .is_some_and(|fqn| self.names.get(&fqn).is_some_and(|e| e.kind() == "class"))
    }

    /// A constant assigned another constant is a second name for one thing.
    /// `Bar` keeps its own declaration site — go-to-definition on `Bar` should
    /// land on `Bar = Foo` — but anywhere a *namespace* is wanted, the alias is
    /// followed through to it.
    fn namespace_of(&self, fqn: &str) -> String {
        let mut current = fqn.to_string();
        let mut seen = HashSet::new();
        while seen.insert(current.clone()) {
            let Some(alias) = self.names.get(&current).and_then(EntryRef::alias_of) else {
                break;
            };
            match self.resolve_lexical(alias.name, &alias.nesting) {
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
        if self.names.contains(&direct) {
            return Some(direct);
        }
        self.ancestors(parent)
            .chain
            .iter()
            .map(|ancestor| qualify(public_name(ancestor), segment))
            .find(|candidate| self.names.contains(candidate))
    }

    /// Ruby's constant lookup, in full, with the evidence behind the answer.
    ///
    /// The ladder for the head of a path: every enclosing lexical scope's own
    /// constants, then the ancestors of the innermost scope, then the top
    /// level. Later segments descend through the previous one's ancestors
    /// instead — lexical nesting does not apply past the head.
    pub(crate) fn resolve(&self, written: &str, written_nesting: &[String]) -> Resolution {
        self.resolve_from(written, written_nesting, None)
    }

    /// The class a return type `method` declares denotes, looked up where
    /// that type was written rather than where the method is called.
    pub(crate) fn returned_class(&self, method: &MethodDef, written: &str) -> Option<String> {
        // Rails finds an association's class from the model's name
        // (`compute_type`), so `class Admin::Post` still sees `Admin::User`.
        let nesting = if matches!(method.via.as_deref(), Some("belongs_to" | "has_one")) {
            let mut prefixes: Vec<String> = Vec::new();
            let mut name = method.owner.as_str();
            loop {
                prefixes.push(format!("::{name}"));
                match name.rsplit_once("::") {
                    Some((outer, _)) => name = outer,
                    None => break,
                }
            }
            prefixes
        } else {
            method.nesting.clone()
        };
        self.resolve(written, &nesting).fqn
    }

    /// `resolve`, for a reference written in the file at `path`: inside a
    /// split class's body the ancestors searched are those of the variant that
    /// file declares, since the name itself has none (DEC-072).
    pub(crate) fn resolve_at(
        &self,
        written: &str,
        written_nesting: &[String],
        path: &str,
    ) -> Resolution {
        self.resolve_from(written, written_nesting, Some(path))
    }

    fn resolve_from(
        &self,
        written: &str,
        written_nesting: &[String],
        path: Option<&str>,
    ) -> Resolution {
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
                let innermost = match path {
                    Some(path) => self.variant_at(innermost, path),
                    None => innermost.clone(),
                };
                let chain = self.ancestors(&innermost);
                unresolved = chain.unresolved.clone();
                for ancestor in &chain.chain {
                    candidates.push((qualify(public_name(ancestor), head), Via::Ancestor));
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
            if self.names.contains(&candidate) {
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
/// The stubs tests are served: core, from the rbs fixture a test Ruby
/// carries (DEC-240).
#[cfg(test)]
pub(crate) fn test_stubs() -> std::sync::Arc<corelib::Stubs> {
    static STUBS: std::sync::OnceLock<std::sync::Arc<corelib::Stubs>> = std::sync::OnceLock::new();
    STUBS
        .get_or_init(|| {
            let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rbs");
            let stubs = crate::rbs::core_only(&dir);
            corelib::Stubs::from_row(crate::store::Rbs {
                key: "testfixture".into(),
                version: "9.9.9".into(),
                dir: dir.to_string_lossy().into_owned(),
                core: stubs.core,
                stdlib: stubs.stdlib,
                sigs: stubs.sigs,
            })
        })
        .clone()
}

/// Ruby source in, assembled namespace out — through the real extractor and
/// with core stubbed from the test Ruby's signatures, so tests written
/// against this exercise the same path `Tree::build` takes.
#[cfg(test)]
pub(crate) fn for_test(sources: &[(&str, &str)]) -> Tree {
    let stubs = test_stubs();
    let (mut decls, mut edges) = core_rows(&stubs);
    let mut methods = Vec::new();
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
    let markers = edges
        .iter()
        .filter(|edge| edge.relation == "dynamic")
        .map(|edge| EdgeRow {
            owner: edge.owner.clone(),
            relation: edge.relation.clone(),
            target: edge.target.clone(),
            path: edge.path.clone(),
            line: edge.line,
        })
        .collect();
    let mut tree = Tree::from_rows(decls, edges, &[]);
    tree.dynamic_rows = Mutex::new(Some(markers));
    tree.stubs = Some(stubs);
    // Core's methods first, as a per-name load puts them.
    let mut rows = tree.all_stub_rows();
    rows.append(&mut methods);
    tree.add_methods(rows);
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

    /// A lookup that misses works out only the string macros that could
    /// spell its name (DEC-235): a name none could make places no marker
    /// and finds no macro's callers, and one that a macro makes lands as it
    /// did when every marker was placed first.
    #[test]
    fn a_miss_finds_the_callers_of_only_the_macros_that_could_make_it() {
        let sources = [
            (
                "macros.rb",
                "module Macros\n  def self.included(base)\n    base.extend(ClassMethods)\n  end\n\n  module ClassMethods\n    def add_helper(name)\n      class_eval <<~RUBY, __FILE__, __LINE__ + 1\n        def #{name}_helper\n        end\n      RUBY\n    end\n\n    def add_flags(*names)\n      names.each do |name|\n        class_eval \"def #{name}?; end\"\n      end\n    end\n  end\nend\n",
            ),
            (
                "widget.rb",
                "class Widget\n  include Macros\n  add_helper :color\n  add_flags :active\nend\n",
            ),
        ];
        let dir = std::env::temp_dir().join(format!("trekr-made-for-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut store = Store::open(&dir.join("t.db")).unwrap();
        let mut files = crate::scan::Files::new();
        let mut facts = Vec::new();
        for (path, source) in sources {
            let oid = crate::scan::hash_blob(source.as_bytes());
            files.insert(path.to_string(), oid.clone());
            facts.push((oid, crate::extract::extract(source.as_bytes())));
        }
        store.write("/repo", &files, facts, 0).unwrap();
        let tree = Tree::build(&store, "/repo").unwrap();

        assert!(tree.lookup("Widget", false, "zz_nope").is_none());
        assert!(tree.dynamic.get().is_none(), "no marker placed");
        assert!(tree.callers.values().is_empty(), "no macro's callers found");

        let made = tree.lookup("Widget", false, "color_helper").expect("made");
        assert_eq!(
            (made.owner.as_str(), made.site.path.as_str()),
            ("Widget", "/repo/macros.rb")
        );
        assert_eq!(tree.callers.values().len(), 1, "add_helper's alone");
        assert!(tree.lookup("Widget", false, "active?").is_some());
        assert!(tree.dynamic.get().is_none(), "still none placed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Streaming the methods a demand-loaded tree has not fetched lists
    /// exactly what an eager tree holds: a name already loaded is not listed
    /// twice, a `private :x` assertion is not a definition, and a carrier's
    /// columns are listed under the model that took its table.
    #[test]
    fn listing_every_method_streams_what_an_eager_tree_holds() {
        let sources = [
            (
                "widget.rb",
                "class Widget\n  self.table_name = \"gadgets\"\n  def save; end\n  private :save\n  def to_s; end\nend\n",
            ),
            (
                "gadget.rb",
                "class Gadget\n  def color; end\n  def save(x); end\nend\n",
            ),
        ];
        let dir = std::env::temp_dir().join(format!("trekr-each-method-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut store = Store::open(&dir.join("t.db")).unwrap();
        let mut files = crate::scan::Files::new();
        let mut facts = Vec::new();
        for (path, source) in sources {
            let oid = crate::scan::hash_blob(source.as_bytes());
            files.insert(path.to_string(), oid.clone());
            facts.push((oid, crate::extract::extract(source.as_bytes())));
        }
        store.write("/repo", &files, facts, 0).unwrap();

        // The store's tree has no Ruby, so no core: the checkout's own.
        let listed = |tree: &Tree| {
            let mut all = Vec::new();
            tree.each_method(|owner, singleton, m| {
                if !m.is_definition() || is_core(&m.site.path) {
                    return;
                }
                let file = m.site.path.rsplit('/').next().unwrap().to_string();
                all.push((
                    owner.to_string(),
                    singleton,
                    m.name.clone(),
                    file,
                    m.site.line,
                ));
            });
            all.sort();
            all
        };
        let lazy = Tree::build(&store, "/repo").unwrap();
        lazy.named("color");
        let eager = tree(&sources);
        let got = listed(&lazy);
        assert_eq!(got, listed(&eager));
        let color = (
            "Widget".to_string(),
            false,
            "color".to_string(),
            "gadget.rb".to_string(),
            2,
        );
        assert!(
            got.contains(&color),
            "the carrier's column, keyed onto the model"
        );
        // Widget's own, Gadget's, and Gadget's again under Widget; `private :save` is none.
        assert_eq!(got.iter().filter(|m| m.2 == "save").count(), 3);
        let _ = std::fs::remove_dir_all(&dir);
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
                    .is_none_or(|site| !is_core(&site.path))
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
            tree.is_known("A::B::C"),
            "two levels of nesting qualify twice: {:?}",
            tree.declared()
        );
    }

    #[test]
    fn a_compact_declaration_opens_one_scope_and_creates_only_its_last_segment() {
        let tree = one("module A\nend\nmodule A::B\n  class C\n  end\nend\n");
        assert!(tree.is_known("A::B::C"));
        // `module A::B` does not put `A` in the nesting, so a constant written
        // inside it cannot see `A`'s.
        let tree = one("module A\n  X = 1\nend\nmodule A::B\n  Y = X\nend\n");
        assert_eq!(at(&tree, "X", &["A::B"]), None, "A is not in the nesting");
    }

    #[test]
    fn a_compact_prefix_is_resolved_rather_than_concatenated() {
        // `module A::B` inside `module X` lands under `X` when `X::A` exists…
        let tree = one("module X\n  module A\n  end\n  module A::B\n  end\nend\n");
        assert!(tree.is_known("X::A::B"), "{:?}", tree.declared());
        // …and at the top level when it does not.
        let tree = one("module A\nend\nmodule X\n  module A::B\n  end\nend\n");
        assert!(tree.is_known("A::B"));
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

    /// A class whose mixin path runs through its own ancestors: `Widget` in
    /// `include Widget::Helpers` is the class itself, and `Helpers` is found
    /// through the `::Widget` it included a line earlier — Ruby resolves it.
    const INCLUDES_THROUGH_ITSELF: &str = "module Widget\n  module Helpers\n  end\nend\n\
        module Widget\n  module Parser\n    class Base\n    end\n\
        class Widget < Base\n      include ::Widget\n      include Widget::Helpers\n    end\n\
        class Fancy < Widget\n    end\n  end\nend\n";

    #[test]
    fn a_mixin_found_through_the_class_itself_sees_its_chain_so_far() {
        let tree = one(INCLUDES_THROUGH_ITSELF);
        assert_eq!(
            chain(&tree, "Widget::Parser::Widget")[..4],
            [
                "Widget::Parser::Widget",
                "Widget::Helpers",
                "Widget",
                "Widget::Parser::Base"
            ]
        );
        assert!(
            tree.ancestors("Widget::Parser::Widget")
                .unresolved
                .is_empty()
        );
    }

    /// Memoized chains are the ones each name gets when asked first, so the
    /// order of asking — a subclass before its parent, one side of a cycle
    /// before the other — changes no answer (DEC-200).
    #[test]
    fn a_chain_does_not_depend_on_what_was_asked_before_it() {
        let cases = [
            (
                INCLUDES_THROUGH_ITSELF,
                vec!["Widget::Parser::Widget", "Widget::Parser::Fancy"],
            ),
            (
                // Two classes whose mixin paths each run through the other's
                // ancestors, and two modules that include each other.
                "module Shared\n  module Inner\n  end\nend\n\
                 class Alpha\n  include Shared\n  include Beta::Inner\nend\n\
                 class Beta\n  include Shared\n  include Alpha::Inner\nend\n\
                 module Gamma\n  include Delta\nend\nmodule Delta\n  include Gamma\nend\n\
                 class Epsilon < Alpha\n  include Delta\nend\n",
                vec!["Alpha", "Beta", "Gamma", "Delta", "Epsilon"],
            ),
        ];
        for (source, names) in cases {
            let first: Vec<Vec<String>> = names
                .iter()
                .map(|name| one(source).ancestors(name).chain.clone())
                .collect();
            for order in [names.clone(), names.iter().rev().copied().collect()] {
                let tree = one(source);
                for name in &order {
                    let at = names.iter().position(|n| n == name).unwrap();
                    assert_eq!(
                        tree.ancestors(name).chain,
                        first[at],
                        "{name} after {order:?}"
                    );
                }
            }
            // Threads asking one tree at once, each in its own order, answer
            // as a fresh tree does (DEC-250): a cycle's frames are per thread.
            for _ in 0..20 {
                let tree = one(source);
                std::thread::scope(|scope| {
                    for start in 0..8 {
                        let (tree, names, first) = (&tree, &names, &first);
                        scope.spawn(move || {
                            for step in 0..names.len() {
                                let at = (start + step) % names.len();
                                assert_eq!(tree.ancestors(names[at]).chain, first[at]);
                            }
                        });
                    }
                });
            }
        }
    }

    #[test]
    fn a_compact_prefix_escapes_the_enclosing_nesting_when_it_resolves_outside() {
        let tree = one("module Bar\nend\nmodule Foo\n  class Bar::Baz\n  end\nend\n");
        assert!(tree.is_known("Bar::Baz"));
        assert!(
            !tree.is_known("Foo::Bar"),
            "the prefix resolved to the top-level Bar, so Foo gained nothing"
        );
    }

    #[test]
    fn a_namespace_rails_invents_from_a_directory_still_resolves() {
        // mastodon writes `class ActivityPub::TagManager` and never writes
        // `module ActivityPub`. Zeitwerk creates it; an index that omits it
        // cannot resolve a bare reference to it.
        let tree = one("class ActivityPub::TagManager\nend\n");
        let found = tree.resolve("ActivityPub", &[]);
        assert_eq!(found.fqn.as_deref(), Some("ActivityPub"));
        assert!(
            found.sites.is_empty(),
            "the name exists and no line of code declares it — say so"
        );
        assert!(tree.is_known("ActivityPub::TagManager"));
    }

    #[test]
    fn an_implied_namespace_does_not_overwrite_a_real_one() {
        let tree = one("module Real\nend\nclass Real::Thing\nend\n");
        assert_eq!(tree.sites("Real").len(), 1, "the declaration still wins");
    }

    #[test]
    fn a_rooted_declaration_is_owned_by_the_top_level() {
        let tree = one("module Foo\n  class ::Bar\n    class Baz\n    end\n  end\nend\n");
        assert!(tree.is_known("Bar"));
        assert!(tree.is_known("Bar::Baz"));
        assert!(!tree.is_known("Foo::Bar"));
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
        assert!(tree.is_known("Foo::Bar"), "{:?}", tree.declared());
        assert!(!tree.is_known("ALIAS::Bar"));
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
    fn conflicting_superclasses_make_two_classes_rather_than_one_merged_one() {
        let tree = tree(&[
            ("/r/lib/base.rb", "class Base\nend\nmodule Naming\nend\n"),
            ("/r/models/post.rb", "class Post < Base\nend\n"),
            (
                "/r/fakes/fake.rb",
                "Post = Struct.new(:title) do\n  include Naming\nend\n",
            ),
        ]);
        let model = tree.variant_at("Post", "/r/models/post_test.rb");
        let fake = tree.variant_at("Post", "/r/fakes/fake_test.rb");
        assert_eq!(tree.ancestors(&model).chain[1..3], ["Base", "Object"]);
        assert_eq!(tree.ancestors(&fake).chain[1..3], ["Naming", "Struct"]);
        let neither = tree.ancestors("Post");
        assert_eq!(neither.chain, ["Post"], "no superclass wins by sort order");
        assert_eq!(neither.unresolved, ["Base", "Struct"]);
    }

    #[test]
    fn an_rbi_describing_another_superclass_does_not_split_the_class() {
        let tree = tree(&[
            (
                "/gems/g/lib/map.rb",
                "class Backend\nend\nImpl = Backend\nclass Map < Impl\nend\n",
            ),
            ("/app/sorbet/rbi/gems/g.rbi", "class Map < Backend\nend\n"),
            ("/app/sorbet/rbi/other.rbi", "class Map < Hash\nend\n"),
        ]);
        assert!(tree.variants_of("Map").is_empty());
    }

    /// A gem layers before the app, so its computed superclass is read before
    /// the app's `.rbi` says what it computed to (DEC-210).
    #[test]
    fn a_computed_superclass_gives_way_to_one_that_names_a_class() {
        let tree = tree(&[
            (
                "/gems/g/lib/map.rb",
                "class Backend\nend\nImpl = case RUBY_ENGINE\n  when \"ruby\" then Backend\n  end\nclass Map < Impl\nend\n",
            ),
            ("/app/sorbet/rbi/gems/g.rbi", "class Map < Backend\nend\n"),
        ]);
        assert_eq!(chain(&tree, "Map")[..2], ["Map", "Backend"]);
    }

    #[test]
    fn one_superclass_written_two_ways_is_one_class() {
        let tree = tree(&[
            ("/r/a.rb", "class Base\nend\nclass Post < Base\nend\n"),
            ("/r/b/c.rb", "class Post < ::Base\nend\n"),
        ]);
        assert!(tree.variants_of("Post").is_empty());
        assert_eq!(chain(&tree, "Post")[..2], ["Post", "Base"]);
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
    /// Which class or module a definition belongs to.
    ///
    /// `def Foo.x` names its owner outright; everything else belongs to the
    /// scope it is written in. `target` means different things depending on how
    /// the def arose — an explicit receiver, an alias's source, a `table_name`
    /// override — and only the first is an owner.
    fn owner_of(&self, row: &MethodRow) -> String {
        let scopes = self.scopes(&row.nesting);
        match &row.target {
            Some(target) if row.singleton && row.via.is_none() => self
                .resolve_lexical(target, &scopes)
                .map(|fqn| self.namespace_of(&fqn))
                .unwrap_or_else(|| target.clone()),
            // Only a singleton method can be in `class << X`, and a module's
            // sites are not free to ask of every row.
            _ if row.singleton => self.opened(&row.nesting, &scopes).unwrap_or_default(),
            _ => scopes.first().cloned().unwrap_or_default(),
        }
    }

    /// The carrier→models map, from the `table_name` definitions alone.
    ///
    /// A per-name load cannot see the whole method table, so this one relation
    /// is settled up front. It is cheap: `table_name` is a single name.
    fn carriers_from(&self, rows: &[MethodRow]) -> HashMap<String, Vec<String>> {
        let mut carriers: HashMap<String, Vec<String>> = HashMap::new();
        for row in rows {
            if row.via.as_deref() != Some("table_name") {
                continue;
            }
            let Some(table) = row.target.as_deref() else {
                continue;
            };
            let owner = self.owner_of(row);
            let carrier = crate::extract::table_to_class(table);
            if carrier != owner {
                carriers.entry(carrier).or_default().push(owner);
            }
        }
        carriers
    }

    /// The keys a definition is found under: its owner, or the variants of a
    /// split owner nearest the file it is written in (DEC-072).
    fn owners_of(&self, row: &MethodRow) -> Vec<String> {
        let owner = self.owner_of(row);
        match self.nearest_variants(&owner, &row.path) {
            nearest if nearest.is_empty() => vec![owner],
            nearest => nearest,
        }
    }

    /// Rows every name starts from before anything is loaded, by name.
    fn add_base(&mut self, rows: Vec<MethodRow>) {
        for row in rows {
            self.base.entry(row.name.clone()).or_default().push(row);
        }
    }

    /// One name's rows as its definitions, keyed by owner.
    fn defs_of(&self, rows: Vec<MethodRow>) -> Defs {
        let mut methods = Vec::with_capacity(rows.len());
        let mut by_owner: Owners = HashMap::new();
        for row in rows {
            let owners = self.owners_of(&row);
            let method = self.method_def(row);
            let index = methods.len();
            let side = usize::from(method.singleton);
            for owner in owners {
                by_owner.entry(owner.clone()).or_default()[side].push(index);
                // The carrier owns the schema's methods but is never *declared*,
                // so it cannot be an ancestor — an include edge to it would not
                // resolve. The columns are keyed onto the model instead, and
                // nothing phantom enters the constant namespace.
                for model in self.carriers.get(&owner).into_iter().flatten() {
                    by_owner.entry(model.clone()).or_default()[side].push(index);
                }
            }
            methods.push(method);
        }
        Defs {
            methods,
            by_owner,
            named: OnceLock::new(),
        }
    }

    /// A row as the tree holds it: with its owner resolved.
    fn method_def(&self, row: MethodRow) -> MethodDef {
        let body_elsewhere = row.target.is_some()
            && matches!(
                row.via.as_deref(),
                Some("define_method") | Some("define_singleton_method")
            );
        // An alias bound to a body in its file answers with that body: it is
        // the code that runs, and a later `def` of the name does not move it.
        let bound = row
            .target_pos
            .filter(|_| matches!(row.via.as_deref(), Some("alias") | Some("alias_method")));
        let forwards_to = row.target.clone().filter(|_| {
            matches!(
                row.via.as_deref(),
                Some("delegate_missing_to") | Some("delegate")
            )
        });
        MethodDef {
            forwards_to,
            bound: bound.is_some(),
            // A body written elsewhere takes whatever that body takes.
            arity: if body_elsewhere {
                (0, true)
            } else {
                arity_of(&row.params)
            },
            body_elsewhere,
            owner: self.owner_of(&row),
            nesting: row.nesting,
            name: row.name,
            singleton: row.singleton,
            visibility: row.visibility,
            via: row.via,
            sig_returns: row.sig_returns,
            sig_overloads: row.sig_overloads,
            site: Site {
                path: row.path,
                line: bound.map_or(row.line, |at| at.line),
                col: bound.map_or(row.col, |at| at.col),
                kind: "method".into(),
            },
        }
    }

    /// Every method with this name, fetched once: a thread that asks while
    /// another fetches it waits for that one's answer. Fetching asks the
    /// store and the namespace, never for another name's methods, so no two
    /// fetches wait on each other.
    ///
    /// A tree with no loader was built from rows in hand and already has
    /// everything it will ever have.
    fn ensure(&self, name: &str) -> Arc<Defs> {
        self.defs.get_or_init(name, || {
            let mut rows = self.base.get(name).cloned().unwrap_or_default();
            if let Some(loader) = &self.loader {
                // Core's methods of this name go in first, then the stdlib's
                // compiled ones, so a class the app or a gem reopens answers
                // with its own (DEC-220, DEC-240).
                if let Some(stubs) = &self.stubs {
                    for defs in [stubs.core_defs().get(name), stubs.stdlib_defs().get(name)]
                        .into_iter()
                        .flatten()
                    {
                        rows.extend(defs.iter().flat_map(|def| self.stub_rows(def)));
                    }
                }
                if let Some(more) = loader.methods_named(name) {
                    rows.extend(more);
                }
            }
            Arc::new(self.defs_of(rows))
        })
    }

    /// The name's definitions as far as they are known without loading it:
    /// what the tree holds once it is loaded, and its base rows before. For
    /// a listing only; an answer asks `ensure`, which does not depend on
    /// what was asked before it.
    fn peek(&self, name: &str) -> Arc<Defs> {
        match self.defs.peek(name) {
            Some(defs) => defs,
            None if self.loader.is_none() => self.ensure(name),
            None => Arc::new(self.defs_of(self.base.get(name).cloned().unwrap_or_default())),
        }
    }

    /// Every method of the stubs, core's first: for a tree that loads
    /// everything at once.
    fn all_stub_rows(&self) -> Vec<MethodRow> {
        let Some(stubs) = &self.stubs else {
            return Vec::new();
        };
        let mut defs: Vec<&corelib::StubDef> = Vec::new();
        for by_name in [stubs.core_defs(), stubs.stdlib_defs()] {
            let mut names: Vec<&String> = by_name.keys().collect();
            names.sort();
            defs.extend(names.into_iter().flat_map(|name| &by_name[name]));
        }
        defs.into_iter()
            .flat_map(|def| self.stub_rows(def))
            .collect()
    }

    /// A stub's method as rows, sited in its own file — none when the tree
    /// does not know its owner, since another Ruby may lack the library.
    fn stub_rows(&self, def: &corelib::StubDef) -> Vec<MethodRow> {
        let (_, _, mut rows) = rows_from(&def.path, &def.source);
        rows.retain(|row| self.kind_of(&self.owner_of(row)).is_some());
        for row in &mut rows {
            row.line += def.shift;
        }
        rows
    }

    /// Where Ruby's lookup along `chain` finds nothing, the first class in
    /// it that a string macro in another file made `name` on (DEC-212): the
    /// method is that class's, and its site the macro's `class_eval`. Only
    /// on a miss, so what the chain's own source defines is never reordered.
    fn made_along(&self, chain: &Chain, name: &str) -> Option<Landing> {
        if self.placing() {
            return None;
        }
        let made = self.made_for(name);
        if made.is_empty() {
            return None;
        }
        let (owner, singleton, how) = chain.iter().find_map(|(owner, side)| {
            let how = made.get(owner)?[usize::from(side)].as_ref()?;
            Some((owner.to_string(), side, how.clone()))
        })?;
        let made = MethodDef {
            name: name.to_string(),
            owner: owner.clone(),
            singleton,
            visibility: "public".to_string(),
            via: Some(how.maker.by.clone()),
            sig_returns: None,
            sig_overloads: Vec::new(),
            nesting: Vec::new(),
            // The string's parameters are not stored.
            arity: (0, true),
            site: Site {
                path: how.path,
                line: how.line,
                col: 1,
                kind: "method".into(),
            },
            body_elsewhere: false,
            forwards_to: None,
            bound: false,
        };
        Some(Landing {
            at: At::Made(Arc::new(made)),
            owner: public_name(&owner).to_string(),
        })
    }

    /// Whether this thread is placing markers for this tree.
    fn placing(&self) -> bool {
        PLACING.get() == self.id
    }

    /// Run `work` as placing, and put back what was.
    fn as_placing<T>(&self, work: impl FnOnce() -> T) -> T {
        let was = PLACING.replace(self.id);
        let done = work();
        PLACING.set(was);
        done
    }

    /// Load every method at once — for a tree built from rows rather than from
    /// a store.
    fn add_methods(&mut self, rows: Vec<MethodRow>) {
        self.carriers = self.carriers_from(&rows);
        self.add_base(rows);
    }

    /// The chain of `(owner, singleton)` pairs Ruby searches for a method.
    ///
    /// For an instance method that is just the ancestor chain. For a singleton
    /// method it is a **different** walk: up the *superclass* chain only —
    /// included modules contribute no class methods — inserting at each level
    /// the level's own singleton methods and then whatever it `extend`s.
    pub(crate) fn lookup_chain(&self, fqn: &str, singleton: bool) -> Vec<(String, bool)> {
        self.chain_for(fqn, singleton, false)
            .iter()
            .map(|(owner, side)| (owner.to_string(), side))
            .collect()
    }

    /// `as_self`: the chain a call on `self` written in `fqn`'s body walks.
    /// In a concern that is its includer's, as far as the index can say:
    /// code there that reaches the class side runs in `included do`, on the
    /// class that included it, which has the concern's `ClassMethods`
    /// (DEC-105).
    fn chain_for(&self, fqn: &str, singleton: bool, as_self: bool) -> Chain {
        if !singleton {
            return Chain::Instance(self.ancestors(fqn));
        }
        let key = (fqn.to_string(), as_self);
        if let Some(chain) = self.singleton_chains.get(&key) {
            return Chain::Singleton(chain);
        }
        let chain: Pairs = self.class_side(fqn, as_self).into();
        Chain::Singleton(self.singleton_chains.publish(key, chain))
    }

    fn class_side(&self, fqn: &str, as_self: bool) -> Vec<(String, bool)> {
        let mut chain = Vec::new();
        let mut seen = HashSet::new();
        for class in self.superclass_chain(fqn) {
            let entry = self.names.get(&class);
            // The last prepended runs first, ahead of the class's own.
            let prepends = entry.map(EntryRef::singleton_prepends).unwrap_or_default();
            for target in prepends.iter().rev() {
                let Some(module) = self.resolve_lexical(target.name, &target.nesting) else {
                    continue;
                };
                for ancestor in &self.ancestors(&self.namespace_of(&module)).chain {
                    if seen.insert((ancestor.clone(), false)) {
                        chain.push((ancestor.clone(), false));
                    }
                }
            }
            if seen.insert((class.clone(), true)) {
                chain.push((class.clone(), true));
            }
            let extends = entry.map(EntryRef::extends).unwrap_or_default();
            for target in extends.iter().rev() {
                // `extend self` is the module-function idiom: the module
                // extends itself, so its own instance methods become singleton
                // ones. The target names no constant, so the owner is it.
                let module = if target.name == "self" {
                    Some(class.clone())
                } else {
                    self.resolve_lexical(target.name, &target.nesting)
                        .map(|fqn| self.namespace_of(&fqn))
                };
                let Some(module) = module else { continue };
                for ancestor in &self.ancestors(&module).chain {
                    if seen.insert((ancestor.clone(), false)) {
                        chain.push((ancestor.clone(), false));
                    }
                }
            }

            // ActiveSupport::Concern extends a module's nested `ClassMethods`
            // into whatever includes it. That is a *tree* fact — the module and
            // the class that includes it are different blobs — and it is how
            // most Rails class methods come to exist. Not into a concern: it
            // defers its own and its dependencies' to its includer (DEC-105).
            let deferred = !as_self && self.is_concern(&class);
            for module_name in self.ancestors(&class).chain.clone() {
                if deferred {
                    break;
                }
                let Some(class_methods) = self.concern_class_methods(&module_name) else {
                    continue;
                };
                for ancestor in &self.ancestors(&class_methods).chain {
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

    /// Does this module extend `ActiveSupport::Concern`, as written?
    fn is_concern(&self, module_name: &str) -> bool {
        self.names.get(module_name).is_some_and(|entry| {
            entry.kind() == "module"
                && entry
                    .extends()
                    .iter()
                    .any(|target| target.name.ends_with("Concern"))
        })
    }

    /// A concern's nested `ClassMethods`, if this module is one.
    ///
    /// `ActiveSupport::Concern` extends `M::ClassMethods` into every class that
    /// includes `M` — no `extend` is ever written, so nothing in the blob layer
    /// records it. Gated on the module actually extending a Concern so that a
    /// plain module happening to contain a `ClassMethods` is not treated as
    /// one; the check is on the name as *written*, because a checkout without
    /// ActiveSupport indexed still writes `extend ActiveSupport::Concern`.
    fn concern_class_methods(&self, module_name: &str) -> Option<String> {
        let entry = self.names.get(module_name)?;
        if entry.kind() != "module" {
            return None;
        }
        let is_concern = entry
            .extends()
            .iter()
            .any(|target| target.name.ends_with("Concern"));
        let nested = qualify(module_name, "ClassMethods");
        (is_concern && self.names.contains(&nested)).then_some(nested)
    }

    /// Only the superclass links — no mixins. Class methods are inherited down
    /// this chain and nowhere else.
    fn superclass_chain(&self, fqn: &str) -> Vec<String> {
        let mut chain = Vec::new();
        let mut seen = HashSet::new();
        let mut current = fqn.to_string();
        while seen.insert(current.clone()) {
            chain.push(current.clone());
            let Some(superclass) = self.names.get(&current).and_then(EntryRef::superclass) else {
                break;
            };
            let Some(next) = self.resolve_lexical(superclass.name, &superclass.nesting) else {
                break;
            };
            current = self.namespace_of(&next);
        }
        chain
    }

    /// The method a receiver of this type would run: the first owner in the
    /// chain that defines the name. Ruby's own rule, so within the indexed set
    /// this is exact rather than a guess.
    pub(crate) fn lookup(&self, fqn: &str, singleton: bool, name: &str) -> Option<MethodDef> {
        self.lookup_along(fqn, singleton, name, false)
    }

    /// `lookup` for a call on `self` written in `fqn`'s body (DEC-105).
    pub(crate) fn lookup_self(&self, fqn: &str, singleton: bool, name: &str) -> Option<MethodDef> {
        self.lookup_along(fqn, singleton, name, true)
    }

    fn lookup_along(
        &self,
        fqn: &str,
        singleton: bool,
        name: &str,
        as_self: bool,
    ) -> Option<MethodDef> {
        let defs = self.ensure(name);
        self.landing(fqn, singleton, name, as_self)
            .map(|landing| land(&defs, &landing))
    }

    fn landing(&self, fqn: &str, singleton: bool, name: &str, as_self: bool) -> Option<Landing> {
        let defs = self.ensure(name);
        let key = (
            fqn.to_string(),
            singleton,
            name.to_string(),
            as_self,
            self.placing(),
        );
        if let Some(found) = self.lookups.get(&key) {
            return found;
        }
        let found = self.look_along(&defs, fqn, singleton, name, as_self);
        self.lookups.publish(key, found)
    }

    fn look_along(
        &self,
        defs: &Defs,
        fqn: &str,
        singleton: bool,
        name: &str,
        as_self: bool,
    ) -> Option<Landing> {
        // A split name asked about as itself runs whichever variant is loaded,
        // so it has an answer only when every variant gives the same one.
        let variants = self.variants_of(fqn);
        if !variants.is_empty() {
            let found: Vec<Option<Landing>> = variants
                .iter()
                .map(|variant| self.landing(variant, singleton, name, as_self))
                .collect();
            let first = found.first()?.as_ref()?;
            let site = &defs.at(first).site;
            let agree = found.iter().all(|other| {
                other.as_ref().is_some_and(|l| {
                    let other = &defs.at(l).site;
                    other.path == site.path && other.line == site.line
                })
            });
            return agree.then(|| first.clone());
        }
        let chain = self.chain_for(fqn, singleton, as_self);
        // Ruby's ancestor order, but real source wins the whole chain before a
        // declaration wins any of it.
        //
        // A committed `sorbet/rbi/` does not merely duplicate methods — it
        // describes them in owners that **do not exist at runtime**
        // (`Widget::CommonRelationMethods`), and those owners sit early in the
        // chain. Preferring real source only *within* an owner therefore fixed
        // the site and left the stub winning the lookup outright.
        //
        // The cost is a genuine override declared *only* in an `.rbi`, which
        // now loses to a real definition further down the chain. Measured, the
        // shadow case dominates that one, and residue candidates still
        // disclose the alternative.
        self.first_in_chain(defs, &chain, 0, true)
            .or_else(|| self.first_in_chain(defs, &chain, 0, false))
            .or_else(|| self.made_along(&chain, name))
    }

    /// What `super` in `owner`'s `name` runs, for a receiver of type `fqn`:
    /// the first definition *after* `owner` in `fqn`'s lookup chain.
    ///
    /// `None` when `owner` is not in that chain at all — the question does not
    /// apply — and `Some(None)` when nothing after it defines the name.
    pub(crate) fn after_in_chain(
        &self,
        fqn: &str,
        singleton: bool,
        owner: &str,
        name: &str,
    ) -> Option<Option<MethodDef>> {
        let defs = self.ensure(name);
        let chain = self.chain_for(fqn, singleton, false);
        // A `def self.x` sits in `lookup_chain` as `(class, true)`, a `def x`
        // as `(owner, false)` — so both halves have to match.
        let at = chain
            .iter()
            .position(|(o, s)| o == owner && s == singleton)?;
        Some(
            self.first_in_chain(&defs, &chain, at + 1, true)
                .or_else(|| self.first_in_chain(&defs, &chain, at + 1, false))
                .map(|landing| land(&defs, &landing)),
        )
    }

    /// What a `super` in `found`, a method on `fqn`'s class side, runs: the
    /// next `name` up that chain (DEC-165).
    pub(crate) fn after_on_class_side(
        &self,
        fqn: &str,
        found: &MethodDef,
        name: &str,
    ) -> Option<MethodDef> {
        let defs = self.ensure(name);
        let chain = self.chain_for(fqn, true, false);
        let at = chain
            .iter()
            .position(|(o, s)| o == found.owner && s == found.singleton)?;
        self.first_in_chain(&defs, &chain, at + 1, true)
            .or_else(|| self.first_in_chain(&defs, &chain, at + 1, false))
            .map(|landing| land(&defs, &landing))
    }

    /// The first definition of `name` along `chain` from `from` on;
    /// `real_only` skips `.rbi` declarations entirely.
    fn first_in_chain(
        &self,
        defs: &Defs,
        chain: &Chain,
        from: usize,
        real_only: bool,
    ) -> Option<Landing> {
        let owners = &defs.by_owner;
        if owners.is_empty() {
            return None;
        }
        let methods = &defs.methods;
        // What Rails generates into a module the class includes as it is
        // made — a column, an `enum`, an association — sits behind the class
        // and every module it includes later, so it is held until the chain
        // reaches the next class (DEC-138). Among those, the model's
        // declaration redefines the column's, as Rails' attribute API does.
        let mut generated: Option<(usize, &str)> = None;
        for (at, (owner, owner_singleton)) in chain.iter().skip(from).enumerate() {
            if at > 0 && generated.is_some() && self.kind_of(owner) == Some("class") {
                break;
            }
            let Some(sides) = owners.get(owner) else {
                continue;
            };
            let hits = &sides[usize::from(owner_singleton)];
            let usable = |i: &&usize| {
                let method = &methods[**i];
                method.is_definition() && !(real_only && method.site.is_rbi())
            };
            let in_module = |i: &&usize| !owner_singleton && into_generated_module(&methods[**i]);
            if let Some(own) = hits.iter().rev().find(|i| usable(i) && !in_module(i)) {
                return Some(landed(*own, owner));
            }
            let from_model = |i: &&usize| methods[**i].via.as_deref() != Some("schema");
            let best = hits
                .iter()
                .rev()
                .find(|i| usable(i) && from_model(i))
                .or_else(|| hits.iter().rev().find(usable));
            let replaces = |held: usize| methods[held].via.as_deref() == Some("schema");
            if let Some(best) = best
                && generated.is_none_or(|(held, _)| replaces(held) && from_model(&best))
            {
                generated = Some((*best, owner));
            }
        }
        generated.map(|(index, owner)| landed(index, owner))
    }

    /// What a method that declares no return returns, from a declaration of
    /// the same method that does — an `.rbi`, or the RSpec stub (DEC-087).
    /// Sorbet reads an `.rbi`'s `sig` as the real method's, and so does this.
    /// The declaration comes back with the type, since a type is looked up
    /// where it is written.
    ///
    /// A stdlib method the core stub also writes — `Set#size` is stubbed for
    /// a Ruby where `Set` is core — takes the stub's return, since both
    /// describe the one method (DEC-182), and one RBS types takes RBS's
    /// (DEC-220). A gem's override of either does not: it may return
    /// something else.
    pub(crate) fn declared_returns(
        &self,
        method: &MethodDef,
        argc: Option<u32>,
        block: bool,
    ) -> Option<(MethodDef, String)> {
        // Loaded already, by the lookup that found `method`.
        let defs = self.ensure(&method.name);
        let methods = &defs.methods;
        let in_stdlib = self.in_stdlib(&method.site.path);
        let hits = defs
            .by_owner
            .get(&method.owner)
            .map(|sides| &sides[usize::from(method.singleton)]);
        let stub = hits.and_then(|hits| {
            hits.iter().rev().find_map(|i| {
                let declared = &methods[*i];
                let describes = declared.site.is_rbi()
                    || is_rspec_stub(&declared.site.path)
                    || (in_stdlib && corelib::is_core(&declared.site.path));
                if !describes {
                    return None;
                }
                let returns = declared.returns_for(argc, block)?.to_string();
                Some((declared.clone(), returns))
            })
        });
        // Core's stub writes some stdlib methods too (`Time.parse`); both it
        // and the real one are the method RBS describes.
        let rubys = in_stdlib || corelib::is_core(&method.site.path);
        stub.or_else(|| {
            let key = (method.owner.clone(), method.singleton, method.name.clone());
            let declared = self.stdlib_signature(&key).filter(|_| rubys)?;
            let returns = declared.returns_for(argc, block)?.to_string();
            Some((declared.clone(), returns))
        })
    }

    /// The return types RBS gives the stdlib's Ruby methods, by (owner,
    /// singleton, name) (DEC-220).
    fn stdlib_signature(&self, key: &(String, bool, String)) -> Option<MethodDef> {
        // Lent only where the stdlib they describe is indexed.
        self.stdlib.as_ref()?;
        let stubs = self.stubs.as_ref()?;
        if let Some(memo) = self.stdlib_sigs.get(key) {
            return memo;
        }
        let method = stubs.sig_defs().get(key).and_then(|def| {
            let (_, _, rows) = rows_from(&def.path, &def.source);
            rows.into_iter().next().map(|row| self.method_def(row))
        });
        self.stdlib_sigs.publish(key.clone(), method)
    }

    /// The class a call to `name` returns whatever its receiver, when every
    /// definition that says agrees: `something.gsub(/x/, "")` could be any
    /// `gsub`, and the index holds only String's. A definition that declares
    /// nothing counts in `total` and not in `agreeing`; two that declare
    /// different classes leave no answer.
    ///
    /// An untyped receiver is taken to be an instance, since a class mostly
    /// arrives as a constant and is typed: `Dir.[]` alone says nothing of
    /// `h[:a]`. But `self.class.build` and `factory.build` are classes that
    /// arrived another way, so a class method declaring something else
    /// objects to the instance methods' answer, though it never makes one.
    pub(crate) fn agreed_return(
        &self,
        name: &str,
        argc: Option<u32>,
        block: bool,
    ) -> Option<Arc<AgreedReturn>> {
        let key = (name.to_string(), argc, block);
        if let Some(memo) = self.agreed_returns.get(&key) {
            return memo;
        }
        let agreed = self.vote_on_return(name, argc, block).map(Arc::new);
        self.agreed_returns.publish(key, agreed)
    }

    fn vote_on_return(&self, name: &str, argc: Option<u32>, block: bool) -> Option<AgreedReturn> {
        let returned = |method: &MethodDef| match method.returns_for(argc, block) {
            Some(returns) => self.returned_class(method, returns),
            None => self
                .declared_returns(method, argc, block)
                .and_then(|(declarer, returns)| self.returned_class(&declarer, &returns)),
        };
        let named = self.named(name);
        let (singletons, instances): (Vec<_>, Vec<_>) = named.iter().partition(|m| m.singleton);
        let mut owners: HashSet<String> = HashSet::new();
        let mut votes: Vec<Option<String>> = Vec::new();
        for method in instances {
            if owners.insert(method.owner.clone()) {
                votes.push(returned(method));
            }
        }
        let mut declared = votes.iter().flatten();
        let fqn = declared.next()?.clone();
        if declared.any(|other| *other != fqn) {
            return None;
        }
        if singletons
            .into_iter()
            .filter_map(returned)
            .any(|other| other != fqn)
        {
            return None;
        }
        Some(AgreedReturn {
            agreeing: votes.iter().flatten().count(),
            total: votes.len(),
            fqn,
        })
    }

    /// A path in the checkout, as a site carries it: absolute.
    pub(crate) fn site_path(&self, relative: &str) -> String {
        if self.root.is_empty() || std::path::Path::new(relative).is_absolute() {
            relative.to_string()
        } else {
            format!("{}/{relative}", self.root)
        }
    }

    /// Is this site in the Ruby's stdlib the checkout runs on (DEC-180)?
    pub(crate) fn in_stdlib(&self, site_path: &str) -> bool {
        self.stdlib
            .as_deref()
            .is_some_and(|stdlib| crate::core::paths::under(stdlib, site_path))
    }

    /// Is this site inside the checkout, rather than a gem or core?
    ///
    /// Exact rather than a guess at the path shape: site paths are absolute and
    /// the tree knows the root it was built for.
    pub(crate) fn in_checkout(&self, site_path: &str) -> bool {
        crate::core::paths::under(&self.root, site_path)
    }

    /// Every method with this name, anywhere. The candidate pool for residue.
    ///
    /// Shared rather than cloned per call: a call site asks for its name's
    /// pool, and `[]` has thousands of definitions in a large checkout. A name
    /// is complete once `ensure` has loaded it, so the list never goes stale.
    pub(crate) fn named(&self, name: &str) -> Arc<[MethodDef]> {
        let defs = self.ensure(name);
        defs.named
            .get_or_init(|| {
                defs.methods
                    .iter()
                    .filter(|m| m.is_definition())
                    .cloned()
                    .collect()
            })
            .clone()
    }

    /// The first scope in `fqn`'s lookup chain that defines methods its source
    /// does not name and may have made `name`, with every such marker there
    /// (DEC-130): the reason "nothing defines it" is a guess there. A marker
    /// counts on the side it makes methods on, and only for a name of its
    /// shape (DEC-160).
    pub(crate) fn dynamic_in_chain(
        &self,
        fqn: &str,
        singleton: bool,
        name: &str,
    ) -> Option<(String, Vec<Dynamic>)> {
        let placed = &self.placed().by_owner;
        if placed.is_empty() {
            return None;
        }
        self.chain_for(fqn, singleton, false)
            .iter()
            .find_map(|(owner, side)| {
                let makers: Vec<Dynamic> = placed
                    .get(owner)?
                    .iter()
                    .filter(|how| how.maker.may_make(name, side))
                    .cloned()
                    .collect();
                (!makers.is_empty()).then(|| (variants::public_name(owner).to_string(), makers))
            })
    }

    /// The markers written in the file at `path` (relative to the checkout)
    /// that may have made `name`, on either side: the likelier maker of a
    /// name defined nowhere than a gem (DEC-162).
    pub(crate) fn dynamic_in_file(&self, path: &str, name: &str) -> Vec<Dynamic> {
        let at = format!("{}/{path}", self.root);
        self.placed()
            .by_file
            .get(&at)
            .map(|makers| {
                makers
                    .iter()
                    .filter(|how| how.maker.may_make(name, false) || how.maker.may_make(name, true))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Markers as a reason says them: each method that does it, the shape
    /// of the names when it is known, and where.
    pub(crate) fn dynamic_note(&self, makers: &[Dynamic]) -> String {
        makers
            .iter()
            .map(|how| {
                let path = match crate::core::paths::under(&self.root, &how.path) {
                    true => how.path[self.root.len() + 1..].to_string(),
                    false => crate::core::paths::pretty(&how.path),
                };
                let shape = match &how.maker.shape {
                    Some(shape) => format!(" `{shape}`"),
                    None => String::new(),
                };
                let via = match &how.maker.via {
                    Some(via) => format!(" in `{via}`"),
                    None => String::new(),
                };
                format!("{}{shape}{via}, {path}:{}", how.maker.by, how.line)
            })
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// Every marker placed, once: a thread that asks meanwhile waits.
    fn placed(&self) -> &Placed {
        self.dynamic
            .get_or_init(|| self.as_placing(|| self.place_dynamic()))
    }

    fn place_dynamic(&self) -> Placed {
        let markers = self.markers();
        let mut placed: HashMap<String, Vec<Dynamic>> = HashMap::new();
        let mut by_file: HashMap<String, Vec<Dynamic>> = HashMap::new();
        let mut macros: Vec<&(String, Dynamic)> = Vec::new();
        for marker in markers.iter() {
            let (owner, how) = marker;
            by_file
                .entry(how.path.clone())
                .or_default()
                .push(how.clone());
            match how.maker.via.is_some() {
                true => macros.push(marker),
                false => placed.entry(owner.clone()).or_default().push(how.clone()),
            }
        }
        for (owner, how) in macros {
            let name = how.maker.via.clone().unwrap_or_default();
            for (caller, args, called_in) in self.macro_callers(owner, &name).iter() {
                // The names this call hands the macro are the names it makes.
                let shapes = match how.maker.shape.as_deref() {
                    Some(shape) => crate::core::handed(shape, args)
                        .into_iter()
                        .map(Some)
                        .collect(),
                    None => vec![None],
                };
                let makers = placed.entry(caller.clone()).or_default();
                for shape in shapes {
                    let mut made = how.clone();
                    made.maker.shape = shape;
                    // A method it makes by name is `made_for`'s, not a hedge.
                    if expanded(&made, called_in).is_some() {
                        continue;
                    }
                    if !makers.iter().any(|known| {
                        known.maker == made.maker
                            && known.path == made.path
                            && known.line == made.line
                    }) {
                        makers.push(made);
                    }
                }
            }
        }
        Placed {
            by_owner: placed,
            by_file,
        }
    }

    /// The methods string macros in other files make named `name`, by class
    /// and side (DEC-212). Only the macros whose shape could spell the name
    /// have their callers found, so a miss on a name no macro makes costs a
    /// pass over the markers and no lookup (DEC-235).
    fn made_for(&self, name: &str) -> Arc<Made> {
        if let Some(made) = self.made.get(name) {
            return made;
        }
        let made = self.as_placing(|| self.make_for(name));
        self.made.publish(name.to_string(), Arc::new(made))
    }

    fn make_for(&self, name: &str) -> Made {
        let markers = self.markers();
        let mut made = Made::new();
        for (owner, how) in markers.iter() {
            if !may_expand_to(&how.maker, name) {
                continue;
            }
            let (Some(via), Some(shape)) = (&how.maker.via, &how.maker.shape) else {
                continue;
            };
            for (caller, args, called_in) in self.macro_callers(owner, via).iter() {
                for shape in crate::core::handed(shape, args) {
                    let mut maker = how.clone();
                    maker.maker.shape = Some(shape);
                    // Later wins, as a later call's `def` would.
                    if let Some((singleton, spelled)) = expanded(&maker, called_in)
                        && spelled == name
                    {
                        made.entry(caller.clone()).or_default()[usize::from(singleton)] =
                            Some(maker);
                    }
                }
            }
        }
        made
    }

    /// Every marker, read once, with the class or module it marks.
    fn markers(&self) -> Markers {
        self.markers.get_or_init(|| self.read_markers()).clone()
    }

    fn read_markers(&self) -> Markers {
        let handed = self
            .dynamic_rows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let rows = match handed {
            Some(rows) => rows,
            None => self
                .loader
                .as_ref()
                .and_then(|loader| {
                    loader
                        .with(|store, roots| store.dynamic_markers(roots))
                        .ok()
                })
                .unwrap_or_default(),
        };
        let mut markers = Vec::new();
        for row in rows {
            // A marker sent to a constant is that class's (DEC-160).
            let Some((owner, _)) = self.edge_owner(&row.owner) else {
                continue;
            };
            let how = Dynamic {
                maker: crate::core::Maker::parse(&row.target),
                path: row.path,
                line: row.line,
            };
            // A core class the stdlib reopens (`Object` in psych's core_ext)
            // is compiled into Ruby itself, and the core stub says what it
            // has: the extension adds nothing to hedge (DEC-181).
            if how.maker.is_compiled()
                && self
                    .sites(&owner)
                    .iter()
                    .any(|site| corelib::is_core(&site.path))
            {
                continue;
            }
            markers.push((owner, how));
        }
        markers.into()
    }

    /// The classes whose body calls the macro `name`, an instance method of
    /// `owner`, and so run it on themselves (DEC-162): a body's call on
    /// itself outside any method, where the class side reaches `owner`.
    /// Each with the literal names that call hands it.
    fn macro_callers(&self, owner: &str, name: &str) -> Callers {
        let key = (owner.to_string(), name.to_string());
        if let Some(callers) = self.callers.get(&key) {
            return callers;
        }
        let callers: Callers = self.find_macro_callers(owner, name).into();
        self.callers.publish(key, callers)
    }

    fn find_macro_callers(&self, owner: &str, name: &str) -> Vec<Caller> {
        let Some(loader) = self.loader.as_ref() else {
            return Vec::new();
        };
        let calls = loader
            .with(|store, roots| store.body_calls(roots, name))
            .unwrap_or_default();
        let mut reached: HashMap<String, bool> = HashMap::new();
        let mut callers = Vec::new();
        for call in calls {
            let Some(caller) = self.scope_fqn(&call.nesting) else {
                continue;
            };
            // The call runs this macro only where the class side's lookup
            // of the name lands on it.
            let reaches = *reached.entry(caller.clone()).or_insert_with(|| {
                self.lookup(&caller, true, name)
                    .is_some_and(|found| found.owner == owner && !found.singleton)
            });
            if reaches {
                callers.push((caller, call.args, call.path));
            }
        }
        callers
    }

    /// The classes that run the `on_load` hook `name`, in the order the
    /// index layers them (DEC-214).
    pub(crate) fn hooked(&self, name: &str) -> Vec<String> {
        let hooks = self.hooks.get_or_init(|| {
            let rows = self
                .loader
                .as_ref()
                .and_then(|loader| loader.with(|store, roots| store.load_hooks(roots)).ok())
                .unwrap_or_default();
            let mut hooks: HashMap<String, Vec<String>> = HashMap::new();
            for row in rows {
                if let Some((base, _)) = self.edge_owner(&row.owner) {
                    let bases = hooks.entry(row.target).or_default();
                    if !bases.contains(&base) {
                        bases.push(base);
                    }
                }
            }
            hooks
        });
        hooks.get(name).cloned().unwrap_or_default()
    }

    /// The fully-qualified name of the scope a fact was written in.
    pub(crate) fn scope_fqn(&self, written_nesting: &[String]) -> Option<String> {
        self.opened(written_nesting, &self.scopes(written_nesting))
    }

    /// The innermost of `scopes`, placed from `written`, as the class it
    /// opens. `class << Time` inside `class Time` opens the Time that name
    /// finds, not a `Time::Time` the placing would make: a scope that is no
    /// declared class or module — a constant in its body implies it as a
    /// module with no site — is looked up instead (DEC-241).
    fn opened(&self, written: &[String], scopes: &[String]) -> Option<String> {
        let innermost = scopes.first()?.clone();
        let declared = |fqn: &str| match self.kind_of(fqn) {
            None => false,
            Some("module") => !self.sites(fqn).is_empty(),
            Some(_) => true,
        };
        if declared(&innermost) {
            return Some(innermost);
        }
        let name = written.first().map_or("", String::as_str);
        let found = scopes[1..]
            .iter()
            .map(String::as_str)
            .chain([""])
            .map(|scope| qualify(scope, name.trim_start_matches("::")))
            .find(|candidate| declared(candidate))
            .map(|fqn| self.namespace_of(&fqn));
        Some(found.unwrap_or(innermost))
    }

    /// The classes that mix in this module, directly or through another module.
    ///
    /// A module is never the receiver of a call written inside it — whatever
    /// includes it is. When the index knows exactly which class that is, the
    /// call has a determinate receiver after all, and this is how to find it.
    pub(crate) fn includers_of(&self, module: &str) -> Vec<String> {
        let includers = self.includers.get_or_init(|| {
            // Only classes: resolving a module receiver to another module
            // would just move the problem.
            let mut classes: Vec<String> = Vec::new();
            self.names.for_each(|fqn, entry| {
                if entry.kind() == "class" {
                    classes.push(fqn.to_string());
                }
            });
            classes.sort();
            let mut by_ancestor: HashMap<String, Vec<u32>> = HashMap::new();
            for (at, class) in classes.iter().enumerate() {
                for ancestor in &self.ancestors(class).chain {
                    if ancestor == class {
                        continue;
                    }
                    match by_ancestor.get_mut(ancestor) {
                        Some(includers) => includers.push(at as u32),
                        None => {
                            by_ancestor.insert(ancestor.clone(), vec![at as u32]);
                        }
                    }
                }
            }
            // In class order already; a module both included and prepended
            // is in a chain twice.
            for includers in by_ancestor.values_mut() {
                includers.dedup();
            }
            Includers {
                classes,
                by_ancestor,
            }
        });
        includers
            .by_ancestor
            .get(module)
            .map(|ids| {
                ids.iter()
                    .map(|&at| includers.classes[at as usize].clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The classes that `include` or `prepend` this module themselves, or
    /// through a module that does — not their subclasses.
    ///
    /// Enough to answer where a module method's `super` goes: a subclass can
    /// only add ancestors *before* its parent, so the part of its chain after
    /// the module is its parent's, unless it mixes the module in again — and
    /// then it is on this list itself.
    pub(crate) fn mixers_of(&self, module: &str) -> Vec<String> {
        let map = self.mixers.get_or_init(|| {
            let mut map: HashMap<String, Vec<String>> = HashMap::new();
            self.names.for_each(|fqn, entry| {
                for (_, target) in entry.mixins() {
                    if let Some(found) = self.resolve_lexical(target.name, &target.nesting) {
                        map.entry(self.namespace_of(&found))
                            .or_default()
                            .push(fqn.to_string());
                    }
                }
            });
            map
        });
        let mut classes: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut pending: Vec<String> = vec![module.to_string()];
        while let Some(next) = pending.pop() {
            for mixer in map.get(&next).into_iter().flatten() {
                if !seen.insert(mixer.clone()) {
                    continue;
                }
                match self.kind_of(mixer) {
                    Some("class") => classes.push(mixer.clone()),
                    _ => pending.push(mixer.clone()),
                }
            }
        }
        classes.sort();
        classes
    }

    /// `class`, `module`, or `constant` — for a name the checkout declares.
    pub(crate) fn kind_of(&self, fqn: &str) -> Option<&str> {
        self.names
            .get(fqn)
            .map(EntryRef::kind)
            .filter(|kind| !kind.is_empty())
    }

    /// The class or module a constant names when a call is sent to it,
    /// following `Bar = Foo` through to Foo. `None` for a constant bound to a
    /// value (`NAMES = %w[a b]`), which is not a class of its own name.
    pub(crate) fn namespace_named(&self, fqn: &str) -> Option<String> {
        let namespace = self.namespace_of(fqn);
        (self.kind_of(&namespace) != Some("constant")).then_some(namespace)
    }

    pub(crate) fn is_known(&self, fqn: &str) -> bool {
        self.names.contains(fqn)
    }

    /// Every declared class, module and constant, with its kind — for a
    /// caller that has to *list* a namespace rather than resolve one name
    /// (LSP completion, DEC-040).
    pub(crate) fn declared(&self) -> Vec<(String, String)> {
        let mut declared = Vec::new();
        self.names.for_each(|fqn, entry| {
            declared.push((fqn.to_string(), entry.kind().to_string()));
        });
        declared
    }

    /// Every method row in the tree's checkouts, under each `(owner,
    /// singleton)` it is keyed by — including a model a `table_name`
    /// carrier's columns were re-keyed onto. That includes a bare `private
    /// :name`, which is no definition but decides who may call one; `via`
    /// tells them apart.
    ///
    /// Completion has to list, not look up, and this is its one pass over
    /// every method. Names not yet loaded are streamed from the store and
    /// **not** kept: loading them all into the tree was most of an LSP
    /// session's memory, and only the listing ever needs them all at once.
    pub(crate) fn each_method(&self, mut visit: impl FnMut(&str, bool, &MethodDef)) {
        let loaded: HashSet<String> = self.defs.done().into_iter().collect();
        let unloaded = self.base.keys().filter(|name| !loaded.contains(*name));
        for name in loaded.iter().chain(unloaded) {
            let defs = self.peek(name);
            for (owner, sides) in &defs.by_owner {
                for (side, hits) in sides.iter().enumerate() {
                    for method in hits.iter().map(|i| &defs.methods[*i]) {
                        visit(owner, side == 1, method);
                    }
                }
            }
        }
        let Some(loader) = &self.loader else { return };
        // The stubs' methods not yet loaded, core's first, as a per-name load
        // would index them.
        if let Some(stubs) = &self.stubs {
            for by_name in [stubs.core_defs(), stubs.stdlib_defs()] {
                for def in by_name
                    .iter()
                    .filter(|(name, _)| !loaded.contains(*name))
                    .flat_map(|(_, defs)| defs)
                {
                    for row in self.stub_rows(def) {
                        let method = self.method_def(row);
                        visit(&method.owner.clone(), method.singleton, &method);
                    }
                }
            }
        }
        let _ = loader.with(|store, roots| {
            store.each_method(roots, |row| {
                // A loaded name was visited above, from the table.
                if loaded.contains(&row.name) {
                    return;
                }
                let owners = self.owners_of(&row);
                let method = self.method_def(row);
                for owner in &owners {
                    visit(owner, method.singleton, &method);
                    for model in self.carriers.get(owner).into_iter().flatten() {
                        visit(model, method.singleton, &method);
                    }
                }
            })
        });
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
    fn a_singleton_class_opened_by_name_inside_that_class_is_its_own() {
        // A constant in the body makes the tree place a `Stamp::Stamp` scope.
        let tree = one(
            "class Stamp\n  class << Stamp\n    FORMAT = 1\n    def parse(text)\n    end\n    \
             alias strict parse\n  end\nend\nmodule Outer\n  class Inner\n  end\n  \
             class << Inner\n    def build\n    end\n  end\nend\n",
        );
        assert_eq!(
            find(&tree, "Stamp", true, "parse").as_deref(),
            Some("Stamp")
        );
        assert_eq!(
            find(&tree, "Stamp", true, "strict").as_deref(),
            Some("Stamp")
        );
        assert_eq!(
            find(&tree, "Outer::Inner", true, "build").as_deref(),
            Some("Outer::Inner")
        );
        assert_eq!(
            tree.scope_fqn(&["Stamp".into(), "Stamp".into()]).as_deref(),
            Some("Stamp"),
            "a call in the body is typed as its methods are placed"
        );
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
    fn a_concerns_class_methods_reach_the_class_that_includes_it() {
        // ActiveSupport::Concern extends M::ClassMethods into every includer,
        // and no `extend` is ever written — so nothing in the blob layer
        // records it. It is how most Rails class methods come to exist.
        let tree = one("module Concern\nend\n\
             module Trackable\n  extend Concern\n  \
             module ClassMethods\n    def track_all\n    end\n  end\n  \
             def track\n  end\nend\n\
             class Widget\n  include Trackable\nend\n");
        assert_eq!(
            find(&tree, "Widget", true, "track_all").as_deref(),
            Some("Trackable::ClassMethods"),
            "a class method, though nothing extends it"
        );
        assert_eq!(
            find(&tree, "Widget", false, "track").as_deref(),
            Some("Trackable"),
            "and the ordinary instance methods still arrive by include"
        );
    }

    #[test]
    fn a_plain_module_with_a_class_methods_is_not_treated_as_a_concern() {
        // The gate is on actually extending a Concern, so a module that merely
        // happens to nest a `ClassMethods` does not leak it.
        let tree = one(
            "module Plain\n  module ClassMethods\n    def surprise\n    end\n  end\nend\n\
             class Widget\n  include Plain\nend\n",
        );
        assert_eq!(find(&tree, "Widget", true, "surprise"), None);
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

pub(crate) use corelib::{
    dir_name as core_dir_name, file_of as core_file_of, is_core, is_rspec_stub, is_stdlib_stub,
    materialize as materialize_core, sweep as sweep_core, sweep_legacy as sweep_legacy_core,
};

/// Tapioca writes one `.rbi` per model describing the methods Rails generates
/// at runtime — AR attributes, associations, enums. They are real methods with
/// no source, and Sorbet's own go-to-definition lands you in the generated
/// file. Landing at the model instead is the point.
pub(crate) const DSL_RBI: &str = "sorbet/rbi/dsl/";

/// Where a tree gets methods it has not been asked for yet.
///
/// Its own connection, so the tree owns everything it needs rather than
/// borrowing the caller's store for its lifetime — which a cached tree, held
/// across queries by a resident session, could not do.
///
/// Any thread may load, each on a connection of its own: a connection is
/// taken from the idle ones, or opened when none is, and put back after.
/// A connection cannot be shared between threads, and one behind a lock
/// would make every load wait on every other.
struct Loader {
    idle: Mutex<Vec<Store>>,
    /// Where another connection opens.
    path: Option<std::path::PathBuf>,
    roots: Roots,
}

impl Loader {
    fn new(store: Store, roots: Roots) -> Loader {
        Loader {
            path: store.path().map(std::path::Path::to_path_buf),
            idle: Mutex::new(vec![store]),
            roots,
        }
    }

    /// Run `work` on a connection no other thread is using.
    fn with<T>(
        &self,
        work: impl FnOnce(&Store, &Roots) -> rusqlite::Result<T>,
    ) -> anyhow::Result<T> {
        let idle = |pool: &Mutex<Vec<Store>>| {
            pool.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pop()
        };
        let store = match idle(&self.idle) {
            Some(store) => store,
            None => self.open()?,
        };
        let done = work(&store, &self.roots);
        self.idle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(store);
        Ok(done?)
    }

    /// Another connection to the store the first one reads.
    fn open(&self) -> anyhow::Result<Store> {
        match &self.path {
            Some(path) => Ok(Store::open(path)?),
            None => anyhow::bail!("an in-memory store has no second connection"),
        }
    }

    /// Every method named `name`, `None` when the store could not say.
    fn methods_named(&self, name: &str) -> Option<Vec<MethodRow>> {
        self.with(|store, roots| store.methods_named(roots, name))
            .ok()
    }
}

/// Where a tree build spent its time, when anyone asked.
///
/// `--profile` has reported the *index* since session 3; a query's own cost was
/// invisible, and the cold start is now the thing being worked on. Enabled by
/// `TREKR_PROFILE=1` or the `--profile` flag, and silent otherwise so the
/// timing itself costs nothing to leave in.
#[derive(Default)]
struct Phases {
    marks: Vec<(&'static str, std::time::Duration)>,
    last: Option<std::time::Instant>,
    decls: usize,
    methods: usize,
    /// Why the snapshot was not read, when it was not.
    snapshot: String,
}

impl Phases {
    fn on() -> bool {
        std::env::var("TREKR_PROFILE").is_ok_and(|v| v != "0")
    }

    fn time<T, E>(
        &mut self,
        label: &'static str,
        work: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E> {
        if !Self::on() {
            return work();
        }
        let start = std::time::Instant::now();
        let out = work();
        self.marks.push((label, start.elapsed()));
        self.last = Some(std::time::Instant::now());
        out
    }

    fn mark(&mut self, label: &'static str) {
        if !Self::on() {
            return;
        }
        let now = std::time::Instant::now();
        let since = self.last.map(|t| now - t).unwrap_or_default();
        self.marks.push((label, since));
        self.last = Some(now);
    }

    fn report(&self) {
        if !Self::on() || self.marks.is_empty() {
            return;
        }
        let total: std::time::Duration = self.marks.iter().map(|(_, d)| *d).sum();
        let phases: Vec<String> = self
            .marks
            .iter()
            .map(|(label, d)| format!("{label} {:.0}ms", d.as_secs_f64() * 1000.0))
            .collect();
        let snapshot = match self.snapshot.as_str() {
            "" => "",
            _ => " — snapshot missed: ",
        };
        eprintln!(
            "tree: {} | total {:.0}ms ({} declarations, {} methods){snapshot}{}",
            phases.join(" · "),
            total.as_secs_f64() * 1000.0,
            self.decls,
            self.methods,
            self.snapshot,
        );
    }
}

/// The checkouts a tree is built from, in the order it layers them: the
/// Ruby's stdlib, the bundle's gems, then the checkout itself — and the
/// stdlib files the bundle's own copies hide (DEC-180).
fn roots(store: &Store, root: &str) -> rusqlite::Result<Roots> {
    store.tree_roots(root)
}

/// A namespace laid out flat, held on the heap.
fn freeze(names: &HashMap<String, Entry>) -> anyhow::Result<snapshot::Snapshot> {
    let key = snapshot::Key::default();
    let bytes = snapshot::encode(names, &key)?;
    Ok(
        snapshot::Snapshot::parse(snapshot::Bytes::Owned(bytes), &key)
            .expect("a namespace just encoded parses"),
    )
}

impl Tree {
    /// Is Ruby core loaded — did the index find a Ruby, and signatures for
    /// it (DEC-240)? Without it, "defined nowhere" cannot speak for core.
    pub(crate) fn has_core(&self) -> bool {
        self.stubs.is_some()
    }
}

/// The implicit superclass of every class that does not name one.
const OBJECT: &str = "Object";

/// The stubs a tree for these roots is served: its Ruby's, when the index
/// read an rbs gem for it (DEC-240).
fn stubs(store: &Store, roots: &Roots) -> anyhow::Result<Option<std::sync::Arc<corelib::Stubs>>> {
    let Some(stdlib) = &roots.stdlib else {
        return Ok(None);
    };
    Ok(store.rbs(stdlib)?.map(corelib::Stubs::from_row))
}

/// Ruby core's classes, modules and constants as rows, from its per-owner
/// files with their methods left out: those are loaded by name, as the
/// index's are (DEC-240).
fn core_rows(stubs: &corelib::Stubs) -> (Vec<DeclRow>, Vec<EdgeRow>) {
    let (mut decls, mut edges) = (Vec::new(), Vec::new());
    for file in stubs.core_files() {
        let skeleton = corelib::Stubs::skeleton(file);
        let (d, e, _) = rows_from(&stubs.site_path(file), &skeleton);
        decls.extend(d);
        edges.extend(e);
    }
    (decls, edges)
}

/// The stdlib's compiled half (DEC-220), as rows.
fn stdlib_rows(stubs: &corelib::Stubs) -> (Vec<DeclRow>, Vec<EdgeRow>, Vec<MethodRow>) {
    let (mut decls, mut edges, mut methods) = (Vec::new(), Vec::new(), Vec::new());
    for file in stubs.stdlib_files() {
        let (d, e, m) = rows_from(&stubs.site_path(file), &file.text);
        decls.extend(d);
        edges.extend(e);
        methods.extend(m);
    }
    (decls, edges, methods)
}

/// The classes only the stdlib's compiled half declares (`Digest::SHA256`),
/// with their edges. A class some Ruby file declares keeps its own sites and
/// ancestry: the stub does not add a second place it is written.
fn compiled_classes(stubs: &corelib::Stubs, decls: &[DeclRow]) -> (Vec<DeclRow>, Vec<EdgeRow>) {
    let written = |nesting: &[String], name: Option<&str>| {
        let mut parts: Vec<&str> = nesting.iter().rev().map(String::as_str).collect();
        parts.extend(name);
        parts.join("::").trim_start_matches("::").to_string()
    };
    let declared: HashSet<String> = decls
        .iter()
        .map(|decl| written(&decl.nesting, Some(&decl.name)))
        .collect();
    let (stub_decls, stub_edges, _) = stdlib_rows(stubs);
    let own: Vec<DeclRow> = stub_decls
        .into_iter()
        .filter(|decl| !declared.contains(&written(&decl.nesting, Some(&decl.name))))
        .collect();
    let owners: HashSet<String> = own
        .iter()
        .map(|decl| written(&decl.nesting, Some(&decl.name)))
        .collect();
    let edges = stub_edges
        .into_iter()
        .filter(|edge| owners.contains(&written(&edge.owner, None)))
        .collect();
    (own, edges)
}

/// The RSpec stub's edges and methods (DEC-087). No declarations: every
/// class and module it names is rspec's own, and a site here would add a
/// place each of them is written.
fn rspec_rows() -> (Vec<EdgeRow>, Vec<MethodRow>) {
    let file = corelib::rspec_file();
    let (_, edges, methods) = rows_from(corelib::RSPEC_STUB, &file.text);
    (edges, methods)
}

/// Does the index hold rspec-core? Its `ExampleGroup` is what the stub wires.
fn declares_example_group(decls: &[DeclRow]) -> bool {
    decls.iter().any(|decl| {
        if decl.kind != "class" {
            return false;
        }
        let mut written: Vec<&str> = decl.nesting.iter().rev().map(String::as_str).collect();
        written.push(&decl.name);
        written.join("::").trim_start_matches("::") == crate::core::rspec::EXAMPLE_GROUP
    })
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
                sig_overloads: d.sig_overloads,
                path: path.to_string(),
                line: d.pos.line,
                col: d.pos.col,
                target_pos: d.target_pos,
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
            path: path.to_string(),
            line: a.pos.line,
        });
    }
    (decls, edges, methods)
}

#[cfg(test)]
mod stdlib_stub_tests {
    use super::*;

    /// What a row says, with its owner as written: a cut `def` is wrapped in
    /// its owner's compact name, the whole file in nested blocks.
    fn said(row: &MethodRow) -> String {
        let owner: Vec<&str> = row.nesting.iter().rev().map(String::as_str).collect();
        format!(
            "{} {} {} {} {:?} {:?} {:?} {}:{}:{}",
            owner.join("::"),
            row.singleton,
            row.name,
            row.visibility,
            row.params,
            row.sig_returns,
            row.sig_overloads,
            row.path,
            row.line,
            row.col
        )
    }

    fn cut_rows<'a>(defs: impl Iterator<Item = &'a corelib::StubDef>) -> Vec<String> {
        let mut cut: Vec<String> = defs
            .flat_map(|def| {
                let (_, _, mut rows) = rows_from(&def.path, &def.source);
                for row in &mut rows {
                    row.line += def.shift;
                }
                rows
            })
            .map(|row| said(&row))
            .collect();
        cut.sort();
        cut
    }

    /// Loading a stub a name at a time must read every method exactly as
    /// parsing it whole does, or a query would see a different core or
    /// stdlib than the one served.
    #[test]
    fn a_cut_def_extracts_as_the_whole_file_does() {
        let core = test_stubs();
        let mut whole: Vec<String> = core
            .core_files()
            .iter()
            .flat_map(|file| rows_from(&core.site_path(file), &file.text).2)
            .map(|row| said(&row))
            .collect();
        whole.sort();
        assert!(whole.len() > 500, "the fixture's core is read");
        assert_eq!(whole, cut_rows(core.core_defs().values().flatten()));

        // The stdlib's halves, in the shape the generator writes them.
        let stubs = corelib::Stubs::from_row(crate::store::Rbs {
            key: "cut-test".into(),
            version: "9.9.9".into(),
            dir: String::new(),
            core: String::new(),
            stdlib: "class Gauge\n  sig { returns(::String) }\n  def self.read(path)\n  end\n\n  \
                     private\n\n  def reset\n  end\n\n  class Dial < ::Gauge\n    def turn(by = nil)\n    \
                     end\n  end\nend\n"
                .into(),
            sigs: "module Meter\n  class Tick\n    sig { params(block: NilClass).returns(::Enumerator) }\n    \
                   sig { params(block: T.proc.void).returns(::Array) }\n    def each(&block)\n    end\n  \
                   end\nend\n"
                .into(),
        });
        let mut whole: Vec<String> = stdlib_rows(&stubs).2.iter().map(said).collect();
        whole.sort();
        assert_eq!(whole.len(), 3);
        assert_eq!(whole, cut_rows(stubs.stdlib_defs().values().flatten()));

        let (path, text) = stubs.sigs_text();
        let mut whole: Vec<String> = rows_from(&path, text).2.iter().map(said).collect();
        whole.sort();
        assert_eq!(whole, cut_rows(stubs.sig_defs().values()));
    }
}

#[cfg(test)]
mod rbi_preference_tests {
    use super::*;

    fn method(owner: &str, name: &str, path: &str) -> crate::store::MethodRow {
        crate::store::MethodRow {
            name: name.into(),
            nesting: vec![owner.into()],
            singleton: false,
            visibility: "public".into(),
            params: Vec::new(),
            via: None,
            target: None,
            sig_returns: None,
            sig_overloads: Vec::new(),
            path: path.into(),
            line: 1,
            col: 1,
            target_pos: None,
        }
    }

    /// A checkout that commits `sorbet/rbi/gems/` holds a stub for every gem
    /// method it calls, indexed after the gem itself. Without a preference the
    /// stub wins and go-to-definition lands on a signature.
    ///
    /// Paths here are **absolute**, as real ones are: an earlier version of
    /// this rule matched with `starts_with("sorbet/rbi/…")` and was silently
    /// dead against every real path in the store.
    #[test]
    fn real_source_beats_an_rbi_stub_for_the_same_method() {
        let mut tree = Tree::from_rows(
            vec![DeclRow {
                name: "Widget".into(),
                kind: "class".into(),
                nesting: Vec::new(),
                target: None,
                path: "/app/widget.rb".into(),
                line: 1,
                col: 1,
            }],
            Vec::new(),
            &[],
        );
        tree.add_methods(vec![
            method("Widget", "save", "/gems/activerecord/lib/persistence.rb"),
            // Indexed later, as the app's own files are.
            method(
                "Widget",
                "save",
                "/app/sorbet/rbi/gems/activerecord@8.1.rbi",
            ),
        ]);
        let found = tree.lookup("Widget", false, "save").expect("found");
        assert_eq!(
            found.site.path, "/gems/activerecord/lib/persistence.rb",
            "the implementation, not the declaration"
        );
    }

    /// Tapioca describes methods in owners that do not exist at runtime, and
    /// those owners sit *early* in the chain. Preferring real source only
    /// within one owner left the stub winning the lookup outright.
    #[test]
    fn a_stub_owner_does_not_beat_real_source_further_down_the_chain() {
        let mut tree = Tree::from_rows(
            vec![
                DeclRow {
                    name: "Base".into(),
                    kind: "class".into(),
                    nesting: Vec::new(),
                    target: None,
                    path: "/gems/ar/lib/base.rb".into(),
                    line: 1,
                    col: 1,
                },
                DeclRow {
                    name: "Widget".into(),
                    kind: "class".into(),
                    nesting: Vec::new(),
                    target: Some("Base".into()),
                    path: "/app/widget.rb".into(),
                    line: 1,
                    col: 1,
                },
            ],
            vec![EdgeRow {
                owner: vec!["Widget".into()],
                relation: "superclass".into(),
                target: "Base".into(),
                path: "/app/widget.rb".into(),
                line: 1,
            }],
            &[],
        );
        tree.add_methods(vec![
            // The real implementation, on the superclass.
            method("Base", "find", "/gems/ar/lib/core.rb"),
            // The stub, on the class itself — earlier in the chain.
            method("Widget", "find", "/app/sorbet/rbi/dsl/widget.rbi"),
        ]);
        let found = tree.lookup("Widget", false, "find").expect("found");
        assert_eq!(
            found.site.path, "/gems/ar/lib/core.rb",
            "the implementation, even though the declaration is nearer"
        );
    }

    /// When the stub is all there is, it is still the best answer available.
    #[test]
    fn an_rbi_stub_is_kept_when_nothing_else_defines_the_method() {
        let mut tree = Tree::from_rows(Vec::new(), Vec::new(), &[]);
        tree.add_methods(vec![method(
            "Widget",
            "only_declared",
            "/app/sorbet/rbi/gems/thing@1.rbi",
        )]);
        assert!(tree.lookup("Widget", false, "only_declared").is_some());
    }
}

#[cfg(test)]
mod reentrant_linearization_tests {
    /// Resolving a *path* asks for a name's ancestors, and that name can be one
    /// already being linearized. `linearize`'s cycle guard is per-call, so the
    /// re-entry started a fresh stack and recursed until the process died.
    ///
    /// The real instance was `File` → `IO` → `IO::EAGAINWaitReadable` → `File`,
    /// out of Ruby core plus a checkout's committed RBIs. `trekr --def`
    /// **aborted** on three positions in widget_shop, and the gold scorer had
    /// been recording those aborts as the far more innocent "no name at this
    /// position".
    ///
    /// Neither path here is directly declared, which is what forces `descend`
    /// to consult ancestors rather than hit the name outright — an earlier
    /// version of this test declared them and passed with the guard removed.
    #[test]
    fn two_classes_whose_superclass_paths_need_each_other_terminate() {
        let tree = crate::tree::for_test(&[("a.rb", "class A < B::C\nend\nclass B < A::D\nend\n")]);
        // Terminating at all is the assertion.
        assert!(tree.ancestors("A").chain.contains(&"A".to_string()));
        assert!(tree.ancestors("B").chain.contains(&"B".to_string()));
    }
}
