//! The fact vocabulary — what one Ruby blob says about itself.
//!
//! Every type here is a **pure function of a blob's bytes**: no paths, no repo
//! identity, no cross-file resolution. That is the blob-layer contract (PLAN
//! §4), and it is what lets N worktrees share one index. If something in here
//! ever needs to know where the file lives, it belongs in the tree layer.

use serde::Serialize;

/// A git blob object id — 40 hex chars of SHA-1 over `blob <len>\0` + bytes.
///
/// Also the identity of a fact set: same bytes, same OID, same facts, forever.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub(crate) struct Oid(pub(crate) String);

impl std::fmt::Display for Oid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Path comparisons that do not assume a shape.
///
/// Every one of these was hand-rolled at its call site once, and two of them
/// were silently dead for a session because the shape changed underneath them
/// (DEC-026). They live here so there is one place to audit, and so their
/// tests use real absolute paths rather than convenient short ones.
pub(crate) mod paths {
    /// Is `path` inside `root`? Boundary-aware: `/a/repo` does not contain
    /// `/a/repo2/x.rb`, which a bare `starts_with` says it does.
    pub(crate) fn under(root: &str, path: &str) -> bool {
        if root.is_empty() {
            return false;
        }
        let root = root.strip_suffix('/').unwrap_or(root);
        path.len() > root.len()
            && path.starts_with(root)
            && path.as_bytes().get(root.len()) == Some(&b'/')
    }

    /// A path as a person should read it: `$HOME` shown as `~`.
    ///
    /// **Display only.** `--json` and `--ndjson` never write `~`: a path there
    /// is relative to the absolute `root` beside it (DEC-076), because a
    /// machine consumer that has to expand `~` is one that will forget to, and
    /// LSP `Location` URIs are absolute `file://` by protocol. Every
    /// human-facing print site goes through here so the next output surface
    /// cannot quietly forget — `tests/cli_e2e.rs` pins that with one assertion
    /// over all of them.
    pub(crate) fn pretty(path: &str) -> String {
        let Some(home) = std::env::var_os("HOME") else {
            return path.to_string();
        };
        let home = home.to_string_lossy();
        let home = home.strip_suffix('/').unwrap_or(&home);
        if home.is_empty() {
            return path.to_string();
        }
        if path == home {
            return "~".to_string();
        }
        // Boundary-aware, like everything else here: `/Users/dan` must not
        // claim `/Users/danger/x`.
        match under(home, path) {
            true => format!("~{}", &path[home.len()..]),
            false => path.to_string(),
        }
    }

    /// Does this absolute path name that checkout-relative file? Boundary-aware
    /// again: `b.rb` is not the file `/x/ab.rb`.
    pub(crate) fn names_file(absolute: &str, relative: &str) -> bool {
        if relative.is_empty() {
            return false;
        }
        match absolute.len().checked_sub(relative.len()) {
            None => false,
            Some(0) => absolute == relative,
            Some(cut) => {
                absolute.ends_with(relative) && absolute.as_bytes().get(cut - 1) == Some(&b'/')
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Boundary-aware like its neighbours: a home of `/Users/dan` must not
        /// claim `/Users/danger/x`, which a bare `strip_prefix` would.
        #[test]
        fn pretty_shortens_home_and_nothing_else() {
            // SAFETY: single-threaded test, restored before it returns.
            let before = std::env::var_os("HOME");
            unsafe { std::env::set_var("HOME", "/Users/dan") };

            assert_eq!(pretty("/Users/dan/code/app.rb"), "~/code/app.rb");
            assert_eq!(pretty("/Users/dan"), "~");
            assert_eq!(pretty("/Users/danger/x.rb"), "/Users/danger/x.rb");
            assert_eq!(pretty("/opt/homebrew/bin/trekr"), "/opt/homebrew/bin/trekr");
            // A trailing slash on HOME is a real shape and must not double up.
            unsafe { std::env::set_var("HOME", "/Users/dan/") };
            assert_eq!(pretty("/Users/dan/code/app.rb"), "~/code/app.rb");

            match before {
                Some(value) => unsafe { std::env::set_var("HOME", value) },
                None => unsafe { std::env::remove_var("HOME") },
            }
        }

        #[test]
        fn a_sibling_whose_name_extends_the_root_is_not_inside_it() {
            let root = "/Users/dev/code/widget_shop";
            assert!(under(
                root,
                "/Users/dev/code/widget_shop/app/models/widget.rb"
            ));
            assert!(!under(
                root,
                "/Users/dev/code/widget_shop-nosorbet/app/models/widget.rb"
            ));
            assert!(!under(root, root), "a root does not contain itself");
            assert!(!under("", "/anything"));
            assert!(under(
                "/Users/dev/code/widget_shop/",
                "/Users/dev/code/widget_shop/a.rb"
            ));
        }

        #[test]
        fn a_file_is_not_named_by_a_suffix_of_another_files_name() {
            let site = "/Users/dev/code/app/models/widget.rb";
            assert!(names_file(site, "app/models/widget.rb"));
            assert!(names_file(site, "widget.rb"));
            assert!(!names_file(site, "idget.rb"), "not a path boundary");
            assert!(!names_file(site, ""));
            assert!(!names_file("widget.rb", "app/models/widget.rb"));
        }
    }
}

/// Everything one blob declares, references, and calls.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Facts {
    pub(crate) defs: Vec<Def>,
    pub(crate) ancestry: Vec<Ancestry>,
    pub(crate) const_refs: Vec<ConstRef>,
    pub(crate) calls: Vec<Call>,
    /// Calls a class or module body makes on itself, outside any method:
    /// what a macro written elsewhere runs on (DEC-162).
    pub(crate) body_calls: Vec<BodyCall>,
    /// Local and instance variable assignments. Extracted but **not stored**:
    /// what a local holds is a question about one file, and `--def` already
    /// reparses that file. Keeping it out of the schema keeps 2 M rows out of
    /// the database for a fact that never crosses a file boundary.
    pub(crate) assigns: Vec<Assign>,
    /// `include_context "x"` and its kin: the group that includes a top-level
    /// shared group, by its nesting, and the shared group's module (DEC-092).
    /// A resolve-time fact, like `assigns`: never stored.
    pub(crate) shared_includes: Vec<(Vec<String>, String)>,
    /// `it_behaves_like "x"` written with no block: the group it is written
    /// in, and the shared group's module, which RSpec includes into a nested
    /// group of its own that the file writes nothing in. Read by `--dead`
    /// and `--refs` for an includer's member a shared body reads (DEC-490).
    /// Resolve-time, never stored.
    pub(crate) nested_includes: Vec<(Vec<String>, String)>,
    /// A shared group written inside a group, which RSpec scopes to it: its
    /// body's nesting, a group of the file's (DEC-092), and the module its
    /// name would make, which an include in reach names (DEC-490).
    /// Resolve-time, never stored.
    pub(crate) local_shared: Vec<LocalShared>,
    /// Each example group that describes a constant, by its nesting, and the
    /// constant as written — its own argument or its parent's. What an
    /// implicit `subject` is made from (DEC-114). Resolve-time, never stored.
    pub(crate) described: Vec<(Vec<String>, String)>,
    /// The literal a shared group is included by — `"raw http server"` in
    /// `include_context "raw http server"` — where it starts, its length on
    /// the line, and the module it names, so a click on it is a click on the
    /// group (DEC-124). Resolve-time, never stored.
    pub(crate) shared_names: Vec<(Pos, u32, String)>,
    /// The shapes of calls a string of code makes with a name it
    /// interpolates from a value no literal states (`helper_*`), which are
    /// therefore not calls here: why `--dead` hedges on a method of that
    /// shape (DEC-163). Never stored.
    pub(crate) unread_calls: Vec<String>,
    /// Each string of code read in place, once: its rendered text, and the
    /// file offset each of its bytes came from, `None` for a byte a value
    /// was substituted for. What an editor's variable answers read too
    /// (DEC-167). Never stored.
    pub(crate) strings: Vec<StringCode>,
    /// Each `ActiveSupport.on_load` block whose `self` is the hooked class
    /// (not `yield: true`), for typing the calls in it (DEC-214). Never
    /// stored.
    pub(crate) hook_blocks: Vec<HookBlock>,
    /// Each template a `render`, `extends` or `partial` names, where it is
    /// named (DEC-524). Resolve-time, never stored.
    pub(crate) templates: Vec<TemplateRef>,
    /// Prism reported syntax errors; the facts above are what survived.
    pub(crate) parse_errors: usize,
    pub(crate) lines: usize,
    /// The bytes these facts were read from. Only a query needs them — to
    /// work out, when asked, which writes a local's read can see.
    pub(crate) source: Option<std::sync::Arc<[u8]>>,
    /// Each local read → the writes that may have set it, worked out once on
    /// first use (`Facts::reaching`).
    pub(crate) flow: std::sync::OnceLock<std::collections::HashMap<Pos, Vec<Pos>>>,
}

impl Facts {
    /// Which writes the local read at `read` can see, by the flow analysis
    /// `analyze` does over the source — `None` when there is no source to run
    /// it on, or the position is not a local read.
    pub(crate) fn reaching(
        &self,
        read: Pos,
        analyze: impl FnOnce(&[u8]) -> std::collections::HashMap<Pos, Vec<Pos>>,
    ) -> Option<&[Pos]> {
        let source = self.source.as_ref()?;
        self.flow
            .get_or_init(|| analyze(source))
            .get(&read)
            .map(Vec::as_slice)
    }

    /// A digest of everything about this blob that the **tree layer** reads:
    /// its definitions and its ancestry edges. Calls, constant references and
    /// assignments are resolve-time facts and are deliberately excluded.
    ///
    /// This is what makes an edit's effect on the tree decidable without
    /// rebuilding it. Two blobs with the same surface assemble the same tree,
    /// so a checkout whose surfaces have not moved can keep the tree it has.
    ///
    /// **Positions are included**, and that is a deliberate cost. The tree
    /// carries each definition's site, so a definition that merely *moved*
    /// still changes an answer. Measured over 5,158 modified blobs in rails,
    /// discourse and CRuby: 71 % of edits leave the definition structure
    /// alone, but only 46 % also leave every definition on its original line.
    /// Including positions trades those 25 points for being correct by
    /// construction rather than by a metadata patch that has to be right.
    pub(crate) fn surface(&self) -> u64 {
        self.digest(false)
    }

    /// The part of `surface` the tree snapshot holds: declarations — classes,
    /// modules, constants — and the ancestry edges it assembles, with their
    /// positions, and not methods, which a tree loads from the store on
    /// demand, nor the dynamic markers it reads the same way. A method edit
    /// moves `surface` and leaves this, so a snapshot survives it (DEC-194).
    pub(crate) fn namespace(&self) -> u64 {
        self.digest(true)
    }

    fn digest(&self, namespace_only: bool) -> u64 {
        // FNV-1a: no dependency, and the only property needed is that an
        // unrelated edit is overwhelmingly unlikely to land on the same value.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let mut eat = |bytes: &[u8]| {
            for byte in bytes {
                hash ^= *byte as u64;
                hash = hash.wrapping_mul(0x100_0000_01b3);
            }
        };
        for def in self.defs.iter().filter(|d| !d.is_group_member()) {
            if namespace_only {
                if def.kind == Kind::Method {
                    continue;
                }
                // What `Store::declarations` reads, and nothing else.
                eat(def.name.as_bytes());
                eat(def.kind.as_str().as_bytes());
                for scope in &def.nesting {
                    eat(scope.as_bytes());
                    eat(b";");
                }
                eat(def.target.as_deref().unwrap_or("").as_bytes());
                eat(&def.pos.line.to_le_bytes());
                eat(&def.pos.col.to_le_bytes());
                continue;
            }
            eat(def.name.as_bytes());
            eat(def.kind.as_str().as_bytes());
            for scope in &def.nesting {
                eat(scope.as_bytes());
                eat(b";");
            }
            eat(&[def.singleton as u8]);
            eat(def.visibility.as_str().as_bytes());
            for param in &def.params {
                eat(param.kind.as_str().as_bytes());
                eat(param.name.as_bytes());
            }
            eat(def.via.as_deref().unwrap_or("").as_bytes());
            eat(def.target.as_deref().unwrap_or("").as_bytes());
            eat(def.sig_returns.as_deref().unwrap_or("").as_bytes());
            if let Some(at) = def.target_pos {
                eat(&at.line.to_le_bytes());
                eat(&at.col.to_le_bytes());
            }
            eat(&def.pos.line.to_le_bytes());
            eat(&def.pos.col.to_le_bytes());
            eat(&def.end_line.to_le_bytes());
        }
        for edge in &self.ancestry {
            if namespace_only && matches!(edge.relation, Relation::Dynamic | Relation::Macro) {
                continue;
            }
            for scope in &edge.owner {
                eat(scope.as_bytes());
                eat(b";");
            }
            eat(edge.relation.as_str().as_bytes());
            eat(edge.target.as_bytes());
            eat(&edge.pos.line.to_le_bytes());
            eat(&edge.pos.col.to_le_bytes());
        }
        hash
    }
}

/// Where a fact sits in the source. 1-based line, 1-based column, matching
/// what an editor shows and what `file:line:col` means everywhere else.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub(crate) struct Pos {
    pub(crate) line: u32,
    pub(crate) col: u32,
}

/// The four things a Ruby name can denote. Deliberately not "kind of node" —
/// `attr_reader :x` and `def x` are both a `Method`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Kind {
    Class,
    Module,
    Method,
    Constant,
}

impl Kind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Kind::Class => "class",
            Kind::Module => "module",
            Kind::Method => "method",
            Kind::Constant => "constant",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Visibility {
    #[default]
    Public,
    Private,
    Protected,
}

impl Visibility {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Visibility::Public => "public",
            Visibility::Private => "private",
            Visibility::Protected => "protected",
        }
    }
}

/// The `via` of a `def` in a `class_eval` block written in a method body: a
/// definition, but one that exists only once the method has run.
pub(crate) const DEFERRED_EVAL: &str = "class_eval in a method";

/// The `via` of a `def` in a block at the top of a file: whatever the block
/// runs on gets it — `main`'s singleton under `instance_eval` — and only if
/// the block runs, so it is not taken for Object's (DEC-445).
pub(crate) const TOP_LEVEL_BLOCK: &str = "def in a block at the top level";

/// A definition: a name this blob binds, and everything the tree layer needs
/// to place it in a namespace without re-reading the source.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct Def {
    pub(crate) name: String,
    pub(crate) kind: Kind,
    /// Lexical scope stack at the definition, innermost first. `module A::B`
    /// contributes one entry (`A::B`), not two — Ruby does not open `A` for
    /// constant lookup there, and the stack is the only place that shows it.
    pub(crate) nesting: Vec<String>,
    /// A method on the singleton: `def self.x`, `def Foo.x`, or any `def`
    /// inside `class << self`.
    pub(crate) singleton: bool,
    pub(crate) visibility: Visibility,
    /// Ruby's own `Method#parameters` vocabulary, one per parameter:
    /// `req` `opt` `rest` `post` `keyreq` `key` `keyrest` `block` `nokey`.
    pub(crate) params: Vec<Param>,
    /// The macro that produced this def (`attr_reader`, `alias_method`, …).
    /// `None` for a literal `def`/`class`/`module`/assignment.
    pub(crate) via: Option<String>,
    /// What this name stands for: the aliased method for an alias, the
    /// right-hand constant for `Bar = Foo`, the explicit receiver for
    /// `def Foo.x`. Unresolved — a name as written.
    pub(crate) target: Option<String>,
    /// Return type named by an inline Sorbet `sig`. 64% of sigs name a usable
    /// class vs 3.9% from syntax alone (PLAN §2) — cheap and high-yield.
    pub(crate) sig_returns: Option<String>,
    /// For an alias: the body it copied, when that is a `def` earlier in this
    /// blob. Ruby binds an alias to the method as it is *then*, so a later
    /// `def` of the same name does not move it.
    pub(crate) target_pos: Option<Pos>,
    /// Per call shape, when several `sig`s — or one naming `NilClass` for
    /// its block — say the return depends on it: `map` returns an Enumerator
    /// without a block and an Array with one (DEC-077). `sig_returns` is then
    /// `None`, since no one class holds for every call.
    ///
    /// Not stored: only core writes these, and core is never in the store.
    pub(crate) sig_overloads: Vec<Overload>,
    /// Parameter name → class, from the `params(...)` half of a `sig`.
    ///
    /// Not stored: a parameter can only be a receiver inside the method that
    /// declares it, and `--def` reparses that file anyway. Keeping it out of
    /// the schema is the same call as `Facts::assigns` (DEC-012).
    pub(crate) sig_params: Vec<(String, String)>,
    /// What a `let` or `subject` block returns, in the shapes an assignment
    /// is typed from (DEC-096). Not stored: a group's own methods never are.
    #[serde(skip)]
    pub(crate) value: Option<ValueShape>,
    /// A `def` whose owner the source does not settle (DEC-562). Not stored:
    /// the tree places it where it is written, and only `--dead` asks.
    #[serde(skip)]
    pub(crate) unsettled: Option<Unsettled>,
    pub(crate) pos: Pos,
    pub(crate) end_line: u32,
}

/// Where a `def` is written when what it is defined on is not the scope
/// around it (DEC-562).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Unsettled {
    /// In the block handed to the call written here, which may run it as
    /// another object: `instance_eval`, `Class.new`, a gem's DSL.
    Block(Pos),
    /// `def clock.x`: a method of the one object the local `clock` holds,
    /// read at `at`.
    Object { local: String, at: Pos },
}

impl Def {
    /// A method an RSpec example group defines — a `let`, a `subject`, a
    /// `def` in its body. Visible only inside the group, so it is kept out of
    /// the store and out of everything the tree reads (DEC-084).
    pub(crate) fn is_group_member(&self) -> bool {
        self.kind == Kind::Method && rspec::in_group(&self.nesting)
    }
}

/// One `sig` among several, as the calls it describes (DEC-077).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Overload {
    /// How many positional arguments the call passes; `None` for any number.
    pub(crate) argc: Option<u32>,
    /// Whether the call passes a block; `None` for either.
    pub(crate) block: Option<bool>,
    pub(crate) returns: Option<String>,
}

impl Overload {
    pub(crate) fn covers(&self, argc: Option<u32>, block: bool) -> bool {
        self.argc.is_none_or(|n| argc == Some(n)) && self.block.is_none_or(|b| b == block)
    }
}

/// The class a call to a method returns, from its `sig`s: the one every
/// overload covering the call agrees on.
pub(crate) fn returns_for<'a>(
    sig_returns: Option<&'a str>,
    overloads: &'a [Overload],
    argc: Option<u32>,
    block: bool,
) -> Option<&'a str> {
    if overloads.is_empty() {
        return sig_returns;
    }
    let mut covering = overloads.iter().filter(|o| o.covers(argc, block));
    let first = covering.next()?.returns.as_deref()?;
    covering
        .all(|o| o.returns.as_deref() == Some(first))
        .then_some(first)
}

/// Methods that hand back their receiver unchanged, so the type survives them,
/// in an assignment or a chain. From rwr's D61 measurement; `then` and
/// `presence` are deliberately absent because they do not preserve the type.
pub(crate) const IDENTITY: [&str; 5] = ["freeze", "dup", "clone", "itself", "tap"];

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Param {
    pub(crate) kind: ParamKind,
    pub(crate) name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ParamKind {
    Req,
    Opt,
    Rest,
    Post,
    Keyreq,
    Key,
    Keyrest,
    Block,
    Nokey,
}

impl ParamKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ParamKind::Req => "req",
            ParamKind::Opt => "opt",
            ParamKind::Rest => "rest",
            ParamKind::Post => "post",
            ParamKind::Keyreq => "keyreq",
            ParamKind::Key => "key",
            ParamKind::Keyrest => "keyrest",
            ParamKind::Block => "block",
            ParamKind::Nokey => "nokey",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<ParamKind> {
        Some(match s {
            "req" => ParamKind::Req,
            "opt" => ParamKind::Opt,
            "rest" => ParamKind::Rest,
            "post" => ParamKind::Post,
            "keyreq" => ParamKind::Keyreq,
            "key" => ParamKind::Key,
            "keyrest" => ParamKind::Keyrest,
            "block" => ParamKind::Block,
            "nokey" => ParamKind::Nokey,
            _ => return None,
        })
    }
}

/// An edge that puts one name into another's ancestor chain. `class Foo < Bar`,
/// `include`, `prepend`, and `extend` are one shape — a scope, a relation, and
/// an unresolved target name — so they are one table. Linearization order
/// (`[prepends, self, includes, superclass]`) is the tree layer's business.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct Ancestry {
    /// The scope stack **including the receiving class or module itself**,
    /// innermost first. Not the same as where the target name is written: a
    /// superclass expression is evaluated outside the body it opens, so the
    /// tree layer drops the first entry for that one relation.
    pub(crate) owner: Vec<String>,
    pub(crate) relation: Relation,
    /// Target constant as written (`Bar`, `A::B`, `::Foo`), or `self` for
    /// `extend self`.
    pub(crate) target: String,
    pub(crate) pos: Pos,
}

/// Ancestry written by a call rather than in a body: `Widget.include(Helpers)`
/// (DEC-097). The receiving class is a constant the call names, so its edge's
/// owner starts with a segment saying so, and the tree looks the constant up
/// in the rest of the nesting instead of placing a body there.
///
/// An `ActiveSupport.on_load(:name)` block's mixins land on whatever runs the
/// hook (DEC-098), so their owner starts with an `(on_load name)` segment.
///
/// A mixin sent to the `base` of `def self.included(base)` lands on whatever
/// includes the module (DEC-102): `(mixed include)` before the module's own
/// nesting, and `prepend` or `extend` for the other two hooks.
pub(crate) mod runtime {
    pub(crate) const SENT: &str = "(sent ";
    const HOOK: &str = "(on_load ";
    const MIXED: &str = "(mixed ";

    /// The owner segment for a mixin sent to the constant `receiver`, as
    /// written.
    pub(crate) fn sent(receiver: &str) -> String {
        format!("{SENT}{receiver})")
    }

    /// The constant a `sent` segment names.
    pub(crate) fn sent_to(segment: &str) -> Option<&str> {
        segment.strip_prefix(SENT)?.strip_suffix(')')
    }

    /// Is this owner segment a mixin sent or hooked rather than written in
    /// a body?
    pub(crate) fn is_runtime(segment: &str) -> bool {
        segment.starts_with(SENT) || segment.starts_with(HOOK)
    }

    /// The owner segment for a mixin in an `on_load(:name)` block.
    pub(crate) fn hook(name: &str) -> String {
        format!("{HOOK}{name})")
    }

    /// The hook a `hook` segment names.
    pub(crate) fn hook_name(segment: &str) -> Option<&str> {
        segment.strip_prefix(HOOK)?.strip_suffix(')')
    }

    /// The owner segment for a mixin that lands on whatever mixes the module
    /// in by `how` — `include`, `prepend` or `extend`.
    pub(crate) fn mixed(how: &str) -> String {
        format!("{MIXED}{how})")
    }

    /// How a `mixed` segment's module is mixed in.
    pub(crate) fn mixed_by(segment: &str) -> Option<&str> {
        segment.strip_prefix(MIXED)?.strip_suffix(')')
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Relation {
    Superclass,
    Include,
    Prepend,
    Extend,
    /// `singleton_class.prepend(M)`: M's methods come before the owner's own
    /// class methods (DEC-101). Ruby has no one method for it.
    SingletonPrepend,
    /// The owner runs the `ActiveSupport.on_load` blocks the target names:
    /// `ActiveSupport.run_load_hooks(:active_record, Base)` (DEC-098). No
    /// ancestor itself, it says where a hook's mixins land.
    LoadHooks,
    /// The owner defines methods whose names the source does not state:
    /// `define_method(name)` with a name no literal spells, or a
    /// `class_eval` string that was not read. The target is the method that
    /// does it. No ancestor either: it is why "no such method" is a guess.
    Dynamic,
    /// A class macro's mixin: the owner's method includes, prepends or
    /// extends the target into whichever class body calls it (DEC-313). The
    /// target is a `MacroMixin`, encoded.
    Macro,
}

impl Relation {
    /// The same mixin sent to the singleton class: `include` there is
    /// `extend`, and an `extend` there reaches the singleton's own singleton,
    /// which nothing here models.
    pub(crate) fn on_singleton(self) -> Option<Relation> {
        match self {
            Relation::Include => Some(Relation::Extend),
            Relation::Prepend => Some(Relation::SingletonPrepend),
            _ => None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Relation::Superclass => "superclass",
            Relation::Include => "include",
            Relation::Prepend => "prepend",
            Relation::Extend => "extend",
            Relation::SingletonPrepend => "singleton_prepend",
            Relation::LoadHooks => "load_hooks",
            Relation::Dynamic => "dynamic",
            Relation::Macro => "macro",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Relation> {
        Some(match s {
            "superclass" => Relation::Superclass,
            "include" => Relation::Include,
            "prepend" => Relation::Prepend,
            "extend" => Relation::Extend,
            "singleton_prepend" => Relation::SingletonPrepend,
            "load_hooks" => Relation::LoadHooks,
            "dynamic" => Relation::Dynamic,
            "macro" => Relation::Macro,
            _ => return None,
        })
    }
}

/// What a `macro` edge's target says (DEC-313): `def self.delegate_all;
/// include AutomaticDelegation; end` is `include|.delegate_all|AutomaticDelegation`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MacroMixin {
    /// `include`, `prepend` or `extend`.
    pub(crate) how: Relation,
    /// The macro is a class method of its owner, not an instance method of
    /// a module that classes extend.
    pub(crate) singleton: bool,
    /// The macro's name: what a class body calls.
    pub(crate) method: String,
    /// The constant mixed in, as written in the macro.
    pub(crate) target: String,
}

impl MacroMixin {
    pub(crate) fn encode(&self) -> String {
        let side = if self.singleton { '.' } else { '#' };
        format!(
            "{}|{side}{}|{}",
            self.how.as_str(),
            self.method,
            self.target
        )
    }

    pub(crate) fn parse(text: &str) -> Option<MacroMixin> {
        let mut parts = text.splitn(3, '|');
        let how = Relation::parse(parts.next()?)?;
        let called = parts.next()?;
        let singleton = called.starts_with('.');
        let method = called.get(1..)?.to_string();
        let target = parts.next()?.to_string();
        Some(MacroMixin {
            how,
            singleton,
            method,
            target,
        })
    }
}

/// What a `dynamic` edge's target says (DEC-130, DEC-160): the method that
/// makes the unnamed methods, which side of the owner they land on, and the
/// shape their names take when the source spells part of it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Maker {
    /// `define_method`, `class_eval`, `class_eval string`.
    pub(crate) by: String,
    /// `Some(true)` for class methods only, `Some(false)` for instance
    /// methods only, `None` for either: a string of code can make both.
    pub(crate) singleton: Option<bool>,
    /// The name with every part the source does not spell as `*`:
    /// `_render_with_renderer_*`. `None` when nothing is spelled.
    pub(crate) shape: Option<String>,
    /// The method that does it, when that is an instance method run on a
    /// class — a macro: the methods are made on each class whose body calls
    /// it, not on the scope it is written in (DEC-162).
    pub(crate) via: Option<String>,
    /// The body is the block the macro's caller hands it: `def test(name,
    /// &block) define_method(…, &block)`. A call in that block runs on what
    /// the made method runs on (DEC-260).
    pub(crate) block: bool,
}

/// The maker a `method_missing` that sends names on is marked with: it may
/// run any class's method, not only its own class's (DEC-261).
pub(crate) const FORWARDER: &str = "method_missing";

/// The maker of a partly compiled stdlib class's methods (DEC-181), before
/// the extension's name.
pub(crate) const COMPILED: &str = "compiled extension";

impl Maker {
    /// A compiled extension, rather than Ruby that makes methods.
    pub(crate) fn is_compiled(&self) -> bool {
        self.by.starts_with(COMPILED)
    }

    /// A `method_missing` that hands a name it lacks to another object,
    /// rather than Ruby that makes methods (DEC-261).
    pub(crate) fn forwards(&self) -> bool {
        self.by == FORWARDER
    }

    /// As stored: the bare maker, or `by|side|shape[|via[|&]]` when it says
    /// more.
    pub(crate) fn encode(&self) -> String {
        if self.singleton.is_none() && self.shape.is_none() && self.via.is_none() {
            return self.by.clone();
        }
        let side = match self.singleton {
            Some(true) => "singleton",
            Some(false) => "instance",
            None => "",
        };
        let shape = self.shape.as_deref().unwrap_or("");
        let block = if self.block { "|&" } else { "" };
        match &self.via {
            Some(via) => format!("{}|{side}|{shape}|{via}{block}", self.by),
            None => format!("{}|{side}|{shape}", self.by),
        }
    }

    pub(crate) fn parse(target: &str) -> Maker {
        let mut parts = target.splitn(5, '|');
        let by = parts.next().unwrap_or_default().to_string();
        let singleton = match parts.next() {
            Some("singleton") => Some(true),
            Some("instance") => Some(false),
            _ => None,
        };
        let shape = parts.next().filter(|s| !s.is_empty()).map(str::to_string);
        let via = parts.next().filter(|s| !s.is_empty()).map(str::to_string);
        let block = parts.next() == Some("&");
        Maker {
            by,
            singleton,
            shape,
            via,
            block,
        }
    }

    /// Could this maker have made `name`, on this side? A macro's `{0}`,
    /// the name it is handed, is any name until a caller says (DEC-162).
    pub(crate) fn may_make(&self, name: &str, singleton: bool) -> bool {
        self.singleton.is_none_or(|side| side == singleton)
            && self.shape.as_deref().is_none_or(|shape| {
                let any = handed(shape, &[None]).pop().unwrap_or_default();
                shape_matches(&any, name)
            })
    }
}

/// A macro's shape with the names a call hands it (DEC-162): `{k}` is the
/// call's `k`th argument, `{k*}` each argument from the `k`th on (a splat
/// the macro iterates), and one that is not a literal name is `*`.
pub(crate) fn handed(shape: &str, args: &[Option<String>]) -> Vec<String> {
    let Some(open) = shape.find('{') else {
        return vec![shape.to_string()];
    };
    let Some(close) = shape[open..].find('}').map(|at| open + at) else {
        return vec![shape.to_string()];
    };
    let token = &shape[open + 1..close];
    let (index, splat) = match token.strip_suffix('*') {
        Some(index) => (index.parse::<usize>().ok(), true),
        None => (token.parse::<usize>().ok(), false),
    };
    let values: Vec<String> = match index {
        Some(k) if splat => args
            .get(k..)
            .unwrap_or_default()
            .iter()
            .map(|arg| arg.clone().unwrap_or_else(|| "*".to_string()))
            .collect(),
        Some(k) => vec![
            args.get(k)
                .cloned()
                .flatten()
                .unwrap_or_else(|| "*".to_string()),
        ],
        None => vec!["*".to_string()],
    };
    let (before, after) = (&shape[..open], &shape[close + 1..]);
    values
        .iter()
        .flat_map(|value| {
            handed(after, args)
                .into_iter()
                .map(move |rest| format!("{before}{value}{rest}"))
        })
        .collect()
}

/// `*` is any run of characters, everything else itself.
pub(crate) fn shape_matches(shape: &str, name: &str) -> bool {
    let mut pieces = shape.split('*');
    let first = pieces.next().unwrap_or_default();
    let Some(mut rest) = name.strip_prefix(first) else {
        return false;
    };
    let pieces: Vec<&str> = pieces.collect();
    let Some((last, middle)) = pieces.split_last() else {
        return rest.is_empty();
    };
    for piece in middle {
        match rest.find(piece) {
            Some(at) => rest = &rest[at + piece.len()..],
            None => return false,
        }
    }
    rest.ends_with(last)
}

/// A shared group written inside an example group (DEC-490).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LocalShared {
    /// Its body's nesting: a group segment of the file's.
    pub(crate) body: Vec<String>,
    /// The module its name would make, which an include names.
    pub(crate) module: String,
    /// Where the `shared_examples` call is written.
    pub(crate) pos: Pos,
}

/// An `ActiveSupport.on_load(:name) do … end` block (DEC-214).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HookBlock {
    /// The hook, as `run_load_hooks` names it.
    pub(crate) name: String,
    /// What a call directly in the block has as its `block_owner`.
    pub(crate) owner: Pos,
    /// The block's first and last lines.
    pub(crate) lines: (u32, u32),
}

/// A string of code as read, with where its bytes are in the file (DEC-167).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct StringCode {
    pub(crate) src: Vec<u8>,
    /// Per byte of `src`, its offset in the file, or `None` for a byte of a
    /// substituted value.
    pub(crate) origin: Vec<Option<usize>>,
}

impl StringCode {
    /// A span of the rendered text as a span of the file, when every byte
    /// of it is the file's own and they are contiguous there.
    pub(crate) fn place(&self, span: std::ops::Range<usize>) -> Option<std::ops::Range<usize>> {
        if span.is_empty() {
            let at = (*self.origin.get(span.start)?)?;
            return Some(at..at);
        }
        let first = (*self.origin.get(span.start)?)?;
        for (i, at) in self.origin.get(span.clone())?.iter().enumerate() {
            if *at != Some(first + i) {
                return None;
            }
        }
        Some(first..first + span.len())
    }
}

/// `add_helper :color` in a class body: a call on the class itself, with the
/// literal names it is handed, positionally (DEC-162).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct BodyCall {
    pub(crate) name: String,
    pub(crate) nesting: Vec<String>,
    pub(crate) args: Vec<Option<String>>,
    pub(crate) line: u32,
}

/// A constant mentioned, with the lexical nesting that will resolve it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct ConstRef {
    /// As written: `Foo`, `A::B`, `::Foo`.
    pub(crate) name: String,
    pub(crate) nesting: Vec<String>,
    pub(crate) pos: Pos,
}

/// `x = <something>` — the something, in the shapes worth inferring a type
/// from. rwr measured which ones pay (D61): `X.new` and the identity methods
/// carry real signal; `then` and `presence` do not and are excluded.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ValueShape {
    /// `X.new`
    New(String),
    /// `X` — a bare constant, so the variable holds the class itself.
    Const(String),
    /// `y`, or `y.freeze` / `.dup` / `.clone` / `.itself` / `.tap` — whatever
    /// `y` is.
    Same(String),
    /// `helper` — an implicit-self call, whose `sig` may name the type.
    SelfCall(String),
    /// `X.build` — a call on a constant, whose `sig` may name the type.
    /// `at` is where the call's name is, to find it again among the file's
    /// calls, when the assignment recorded it.
    ConstCall {
        recv: String,
        name: String,
        at: Option<Pos>,
    },
    /// `y.build` — a call on another local. One step from a typed `y` and no
    /// further: rwr's D61 found 70 % of returns end in another call, so the
    /// recursive version drowns while the single sig-backed step pays.
    LocalCall {
        recv: String,
        name: String,
        at: Option<Pos>,
    },
    /// `x.where(…).order(:id)` — a call on another call: the one whose name
    /// is at this position, found again among the file's calls, which a
    /// chain types step by step (DEC-444).
    Chain(Pos),
    /// `[]`, `{}`, `"x"`, `1` — a literal, whose class core now knows.
    Literal(&'static str),
    /// `rescue X => e` — an instance of the class rescued; `StandardError`
    /// for a bare `rescue => e`.
    Rescued(String),
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Assign {
    /// `x` for a local, `@x` for an instance variable.
    pub(crate) target: String,
    pub(crate) value: ValueShape,
    pub(crate) nesting: Vec<String>,
    /// Written where `self` is the class: a class-level instance variable,
    /// not its instances'.
    pub(crate) singleton: bool,
    pub(crate) pos: Pos,
}

/// A method call site. The receiver **shape** is the fact Rubydex does not
/// carry (PLAN §8) and the reason this engine exists: 53–66% of call sites are
/// implicit self and need no inference at all, and the rest sort into a ladder.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct Call {
    pub(crate) name: String,
    pub(crate) recv: RecvShape,
    /// Source text of the receiver, when it is a name worth resolving: the
    /// constant path, the local's name, the ivar's name. `None` otherwise.
    pub(crate) recv_text: Option<String>,
    pub(crate) nesting: Vec<String>,
    /// Written inside a singleton method (`def self.x`, or `class << self`).
    /// An implicit receiver means the class itself here and an instance of it
    /// otherwise — the same source text, two different lookups.
    pub(crate) singleton: bool,
    /// Where a local receiver is read — the key to which writes it sees.
    #[serde(skip)]
    pub(crate) recv_pos: Option<Pos>,
    /// A receiver that is a value rather than a name: another call, whose
    /// return types this one, or a literal.
    #[serde(skip)]
    pub(crate) recv_value: Option<RecvValue>,
    /// The call whose block this one is written in, innermost, when that
    /// block runs on something the source does not say — found again among
    /// the file's calls by its position. `None` directly in a method, class
    /// or RSpec example body.
    #[serde(skip)]
    pub(crate) block_owner: Option<Pos>,
    /// Written in an example's own block (`it`), which runs in its group
    /// alone — not a hook's or a `let`'s, which nested groups run too, with
    /// their own `let`s (DEC-096).
    #[serde(skip)]
    pub(crate) in_example: bool,
    /// Written directly in an example group's body, as a macro (`it`, `let`,
    /// `include_examples`) — run on the group's class as the file loads,
    /// never on an example (DEC-490).
    #[serde(skip)]
    pub(crate) group_body: bool,
    /// Written in a `scope`'s body, which ActiveRecord runs on the model's
    /// relation (DEC-116).
    #[serde(skip)]
    pub(crate) in_scope: bool,
    /// The call this name stands for, sent to the receiver it really has:
    /// RSpec's predicate matcher (`be_empty`) is `empty?` on the
    /// expectation's subject (DEC-090), and a symbol naming a method
    /// (`send(:x)`, `before_action :x`) is `x` on the object that will call
    /// it (DEC-093). Untyped when the source does not say what that is.
    #[serde(skip)]
    pub(crate) stands_for: Option<Box<Call>>,
    /// Positional argument count, or `None` when a splat makes it unknowable.
    pub(crate) argc: Option<u32>,
    pub(crate) block: bool,
    pub(crate) pos: Pos,
}

/// A template named in code: `render "posts/form"`, `render partial: "row"`,
/// `render @post`, RABL's `extends "posts/base"` (DEC-524).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TemplateRef {
    /// Where the name, or the value standing for it, is written.
    pub(crate) pos: Pos,
    /// Its length on the line.
    pub(crate) len: u32,
    pub(crate) names: Named,
    /// The keywords handed to the partial as its locals (`post: @post`): the
    /// name, where the key is written, and the value when it is a variable
    /// (`@post`, `post`), with where it is read.
    pub(crate) locals: Vec<Local>,
}

/// A local a `render` hands a partial.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Local {
    pub(crate) name: String,
    pub(crate) pos: Pos,
    pub(crate) value: Option<(String, Pos)>,
}

/// How a template is named.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Named {
    /// `render "x"`: a partial in a view, a template in a controller.
    Render(String),
    /// `partial: "x"` (and RABL's `partial "x"`).
    Partial(String),
    /// `template: "x"`, `layout: "x"`, RABL's `extends "x"`: a template by
    /// its path, with no underscore.
    Template(String),
    /// `render @post`, `render @posts`, `collection: @posts`: the partial
    /// the value's class names, by an instance or ivar as written.
    Object { value: String, collection: bool },
}

/// What an `Other` receiver is, when that is worth knowing.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RecvValue {
    /// `x.gsub(a, b).downcase` — the call at this position (`gsub`), found
    /// again among the file's calls.
    Call(Pos),
    /// `"x".downcase`, `[].push` — a literal of this core class.
    Literal(&'static str),
    /// The example's `subject`, which `is_expected` and a bare `should`
    /// expect without writing it (DEC-096).
    Subject,
    /// `main`, for a bare `describe` at the top of a spec: RSpec's
    /// `expose_dsl_globally` gives `main` its group methods, each sending to
    /// `RSpec` (DEC-115).
    Main,
}

/// The receiver ladder's rungs, in the order they are worth trying.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum RecvShape {
    /// No receiver: `foo` — the enclosing class is the receiver.
    Implicit,
    /// Literal `self.foo`.
    #[serde(rename = "self")]
    SelfRecv,
    /// A constant: `Foo.bar`, `A::B.bar`.
    Const,
    /// A local variable or method parameter: `x.bar`.
    Local,
    /// An instance or class variable: `@x.bar`, `@@x.bar`.
    Ivar,
    /// Anything else — a chain, a literal, a block param.
    Other,
    /// `super`, recorded under the name of the method it is written in: that
    /// is the name Ruby looks up, starting *after* the method's own owner in
    /// the receiver's ancestors.
    Super,
    /// Not written as a call at all: a symbol handed to one, as
    /// `after_create :ensure_thing` or `attributes :name`. The method is
    /// invoked by name at runtime and the receiver is unknowable here, so this
    /// can only ever be a *possible* reference — but leaving it unrecorded is
    /// what makes a callback-registered method look unused (DEC-037).
    Symbol,
}

impl RecvShape {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            RecvShape::Implicit => "implicit",
            RecvShape::SelfRecv => "self",
            RecvShape::Const => "const",
            RecvShape::Local => "local",
            RecvShape::Ivar => "ivar",
            RecvShape::Other => "other",
            RecvShape::Symbol => "symbol",
            RecvShape::Super => "super",
        }
    }
}

impl Call {
    /// How many bytes of source the call's name covers at `pos`. A `super`
    /// site is named after its method but written as the keyword.
    pub(crate) fn written_len(&self) -> usize {
        match self.recv {
            RecvShape::Super => "super".len(),
            _ => self.name.len(),
        }
    }

    /// What is written at `pos`, for a reader choosing between names.
    pub(crate) fn written_name(&self) -> &str {
        match self.recv {
            RecvShape::Super => "super",
            _ => &self.name,
        }
    }
}

/// A nesting stack round-trips through one TEXT column: scope paths joined by
/// `;`, innermost first, empty string at top level. Ruby constant paths are
/// `[A-Za-z0-9_:]` only, so the separator can never appear inside one.
pub(crate) fn join_nesting(nesting: &[String]) -> String {
    nesting.join(";")
}

pub(crate) fn split_nesting(s: &str) -> Vec<String> {
    if s.is_empty() {
        Vec::new()
    } else {
        s.split(';').map(str::to_string).collect()
    }
}

/// RSpec's example groups (DEC-084).
///
/// `describe` builds an anonymous subclass of `RSpec::Core::ExampleGroup` and
/// runs its block as that class's body; `it`, `before` and `let` run theirs
/// on an instance of it. A group is pushed onto a nesting as a segment of its
/// own, so a call inside one knows what `self` is. The segment is not a
/// constant scope — a constant or class written inside a group is the file's,
/// as Ruby has it — so the tree reads past it, and a method a group defines
/// (`let`, `subject`, a `def`) is visible only from that group and the ones
/// nested in it, so it never leaves its file.
pub(crate) mod rspec {
    /// What every example group subclasses.
    pub(crate) const EXAMPLE_GROUP: &str = "RSpec::Core::ExampleGroup";

    /// Where `RSpec.describe` and its kin live, and what a bare top-level
    /// `describe` on `main` sends to (DEC-115).
    pub(crate) const RSPEC: &str = "RSpec";

    /// Where the matchers live, and the `method_missing` that makes the
    /// dynamic ones (DEC-090).
    pub(crate) const MATCHERS: &str = "RSpec::Matchers";

    const OPEN: &str = "(group ";

    /// A top-level shared group's segment (DEC-092).
    const SHARED: &str = "(shared ";

    /// Where a top-level shared group's module is named: RSpec keys it by
    /// name alone, so the name is the module's.
    const SHARED_GROUPS: &str = "RSpec::SharedExampleGroups";

    /// A group's segment, named as RSpec names the class (`AccordTypesDecimal`).
    pub(crate) fn segment(name: &str) -> String {
        format!("{OPEN}{name})")
    }

    /// The body of `shared_context "raw http server"` at the top of a file.
    pub(crate) fn shared_segment(name: &str) -> String {
        format!("{SHARED}{name})")
    }

    pub(crate) fn is_group(segment: &str) -> bool {
        segment.starts_with(OPEN) || segment.starts_with(SHARED)
    }

    /// The module a top-level shared group is, from its body's segment.
    pub(crate) fn shared_module_of(segment: &str) -> Option<String> {
        let name = segment.strip_prefix(SHARED)?.strip_suffix(')')?;
        Some(shared_module(name))
    }

    /// A method a top-level shared group's body defines, which is its
    /// module's rather than a group's (DEC-092).
    pub(crate) fn is_shared_member(def: &super::Def) -> bool {
        def.nesting.first().is_some_and(|scope| {
            scope
                .strip_prefix("::")
                .and_then(|scope| scope.strip_prefix(SHARED_GROUPS))
                .is_some_and(|name| name.starts_with("::"))
        })
    }

    /// `RSpec::SharedExampleGroups::RawHttpServer`, for a shared group's name
    /// as `base_name` writes it.
    pub(crate) fn shared_module(name: &str) -> String {
        format!("{SHARED_GROUPS}::{name}")
    }

    /// Is this the module a shared group's name makes?
    pub(crate) fn is_shared_module(fqn: &str) -> bool {
        fqn.strip_prefix(SHARED_GROUPS)
            .is_some_and(|name| name.starts_with("::"))
    }

    /// Is the innermost scope here an example group?
    pub(crate) fn in_group(nesting: &[String]) -> bool {
        nesting.first().is_some_and(|s| is_group(s))
    }

    /// `RSpec::ExampleGroups::AccordTypesDecimal::WhenValid`, the name RSpec
    /// gives the innermost group's class at runtime.
    /// A shared group's body runs in whichever group includes it, so a group
    /// nested in one is named from the shared module.
    pub(crate) fn class_name(nesting: &[String]) -> String {
        let mut root = "RSpec::ExampleGroups".to_string();
        let mut names: Vec<&str> = Vec::new();
        for segment in nesting.iter().rev() {
            if let Some(module) = shared_module_of(segment) {
                root = module;
                names.clear();
            } else if let Some(name) = segment.strip_prefix(OPEN).and_then(|s| s.strip_suffix(')'))
            {
                names.push(name);
            }
        }
        std::iter::once(root.as_str())
            .chain(names)
            .collect::<Vec<_>>()
            .join("::")
    }

    /// The predicate a dynamic matcher calls on its subject, as
    /// `RSpec::Matchers#method_missing` reads the name: `be_empty` and
    /// `be_an_empty` → `empty?` (BePredicate), `have_key` → `has_key?` (Has).
    pub(crate) fn predicate(matcher: &str) -> Option<String> {
        if matcher.ends_with(['?', '!', '=']) {
            return None;
        }
        if let Some(root) = matcher.strip_prefix("have_") {
            return (!root.is_empty()).then(|| format!("has_{root}?"));
        }
        let rest = matcher.strip_prefix("be_")?;
        let root = rest
            .strip_prefix("an_")
            .or_else(|| rest.strip_prefix("a_"))
            .unwrap_or(rest);
        (!root.is_empty()).then(|| format!("{root}?"))
    }

    /// BePredicate's fallback when the subject has no `exist?`: `exists?`.
    pub(crate) fn present_tense(predicate: &str) -> Option<String> {
        predicate.strip_suffix('?').map(|root| format!("{root}s?"))
    }

    /// RSpec's `base_name_for`: a description as a constant name.
    pub(crate) fn base_name(description: &str) -> String {
        let mut name = String::new();
        let mut upcase = false;
        for c in description.chars() {
            if c.is_ascii_alphanumeric() {
                if upcase {
                    name.push(c.to_ascii_uppercase());
                } else {
                    name.push(c);
                }
                upcase = false;
            } else {
                upcase = true;
            }
        }
        if let Some(first) = name.chars().next() {
            name.replace_range(..1, &first.to_ascii_uppercase().to_string());
        }
        match name.chars().next() {
            None => "Anonymous".to_string(),
            Some(c) if !c.is_ascii_uppercase() => format!("Nested{name}"),
            Some(_) => name,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_description_is_named_as_rspec_names_its_class() {
            assert_eq!(base_name("Accord::Types::Decimal"), "AccordTypesDecimal");
            assert_eq!(base_name("when it is valid"), "WhenItIsValid");
            assert_eq!(base_name("#to_s"), "ToS");
            assert_eq!(base_name("2 widgets"), "Nested2Widgets");
            assert_eq!(base_name(""), "Anonymous");
        }

        #[test]
        fn a_dynamic_matcher_names_the_predicate_it_calls() {
            assert_eq!(predicate("be_empty").as_deref(), Some("empty?"));
            assert_eq!(predicate("be_an_admin").as_deref(), Some("admin?"));
            assert_eq!(predicate("be_a_uuid").as_deref(), Some("uuid?"));
            assert_eq!(predicate("have_key").as_deref(), Some("has_key?"));
            assert_eq!(predicate("be_"), None);
            assert_eq!(predicate("eq"), None);
            assert_eq!(present_tense("exist?").as_deref(), Some("exists?"));
        }

        #[test]
        fn a_group_is_named_from_the_outside_in() {
            let nesting = [segment("WhenIdle"), segment("Widget"), "Shop".to_string()];
            assert!(in_group(&nesting));
            assert!(!is_group(&nesting[2]));
            assert_eq!(
                class_name(&nesting),
                "RSpec::ExampleGroups::Widget::WhenIdle"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nesting_round_trips_through_one_column() {
        for stack in [vec![], vec!["A::B".into()], vec!["A::B".into(), "A".into()]] {
            assert_eq!(split_nesting(&join_nesting(&stack)), stack);
        }
    }
}

#[cfg(test)]
mod surface_tests {
    use crate::extract::extract;

    /// A `let` is its file's alone (DEC-084): adding one rebuilds nothing.
    #[test]
    fn an_example_groups_own_method_is_not_on_the_surface() {
        let base = extract(b"describe Widget do\n  let(:a) { 1 }\nend\n");
        let more = extract(b"describe Widget do\n  let(:a) { 1 }\n  let(:b) { 2 }\nend\n");
        assert_eq!(base.surface(), more.surface());
    }

    /// The property the edit-churn defence rests on: a body-only edit leaves
    /// the surface alone, and anything the tree reads moves it.
    #[test]
    fn a_body_edit_leaves_the_surface_alone_and_a_structural_one_moves_it() {
        let base = extract(b"class Widget\n  include Trackable\n  def save\n    1\n  end\nend\n");
        let same_shape =
            extract(b"class Widget\n  include Trackable\n  def save\n    2 + 2\n  end\nend\n");
        assert_eq!(
            base.surface(),
            same_shape.surface(),
            "only the body changed"
        );

        for (label, source) in [
            (
                "a new method",
                b"class Widget\n  include Trackable\n  def save\n    1\n  end\n  def load\n  end\nend\n"
                    .as_slice(),
            ),
            (
                "a dropped mixin",
                b"class Widget\n  def save\n    1\n  end\nend\n".as_slice(),
            ),
            (
                "a changed arity",
                b"class Widget\n  include Trackable\n  def save(force)\n    1\n  end\nend\n"
                    .as_slice(),
            ),
            (
                "a definition that moved",
                b"class Widget\n  include Trackable\n\n  def save\n    1\n  end\nend\n".as_slice(),
            ),
        ] {
            assert_ne!(
                base.surface(),
                extract(source).surface(),
                "{label} must move the surface"
            );
        }
    }

    /// The snapshot holds no method, so a method edit — a new one, a moved
    /// one, a changed arity — leaves the namespace alone, and a declaration
    /// or an edge moves it (DEC-194).
    #[test]
    fn a_method_edit_leaves_the_namespace_alone() {
        let base = extract(
            b"class Widget\n  include Trackable\n  LIMIT = 3\n  def save\n    1\n  end\nend\n",
        );
        for (label, source) in [
            (
                "a changed arity and a new method",
                b"class Widget\n  include Trackable\n  LIMIT = 3\n  def save(force)\n    1\n  end\n  def load\n  end\nend\n"
                    .as_slice(),
            ),
            (
                "a method that moved",
                b"class Widget\n  include Trackable\n  LIMIT = 3\n\n  def save\n    1\n  end\nend\n"
                    .as_slice(),
            ),
        ] {
            let edited = extract(source);
            assert_ne!(base.surface(), edited.surface(), "{label}");
            assert_eq!(base.namespace(), edited.namespace(), "{label}");
        }
        for (label, source) in [
            (
                "a new class",
                b"class Widget\n  include Trackable\n  LIMIT = 3\n  def save\n    1\n  end\nend\nclass Gadget\nend\n"
                    .as_slice(),
            ),
            (
                "a dropped mixin",
                b"class Widget\n  LIMIT = 3\n  def save\n    1\n  end\nend\n".as_slice(),
            ),
            (
                "a constant that moved",
                b"class Widget\n  include Trackable\n\n  LIMIT = 3\n  def save\n    1\n  end\nend\n"
                    .as_slice(),
            ),
        ] {
            assert_ne!(
                base.namespace(),
                extract(source).namespace(),
                "{label} must move the namespace"
            );
        }
    }

    /// Calls and constant references are resolve-time facts, read from their
    /// own tables — putting them in the surface would rebuild the tree for
    /// every edit and defeat the point.
    #[test]
    fn a_call_only_edit_is_not_part_of_the_surface() {
        let before = extract(b"class Widget\n  def save\n    helper\n  end\nend\n");
        let after = extract(b"class Widget\n  def save\n    a(SOME_CONST); b; c\n  end\nend\n");
        assert!(
            after.calls.len() > before.calls.len() && !after.const_refs.is_empty(),
            "the calls and references really did change"
        );
        assert_eq!(before.surface(), after.surface());
    }

    #[test]
    fn a_shape_matches_only_the_names_it_spells() {
        use super::shape_matches;
        assert!(shape_matches("_render_with_*", "_render_with_json"));
        assert!(shape_matches("*_changed?", "name_changed?"));
        assert!(shape_matches("a_*_b_*", "a_x_b_y"));
        assert!(!shape_matches("_render_with_*", "zz_nope"));
        assert!(!shape_matches("*_x", "x_y"));
        assert!(!shape_matches("exact", "exactly"));
    }

    #[test]
    fn a_macro_shape_takes_the_names_its_caller_hands_it() {
        use super::handed;
        let args = [Some("a".to_string()), None, Some("c".to_string())];
        assert_eq!(handed("{0}_x", &args), ["a_x"]);
        assert_eq!(handed("{1}?", &args), ["*?"]);
        assert_eq!(handed("{1*}=", &args), ["*=", "c="]);
        assert!(handed("{3*}", &args).is_empty());
    }

    #[test]
    fn a_maker_round_trips_what_it_says() {
        use super::Maker;
        assert_eq!(
            Maker {
                by: "define_method".into(),
                ..Maker::default()
            }
            .encode(),
            "define_method"
        );
        let full = Maker {
            by: "define_method".into(),
            singleton: Some(true),
            shape: Some("x_*".into()),
            via: Some("make".into()),
            block: true,
        };
        assert_eq!(Maker::parse(&full.encode()), full);
        let unblocked = Maker {
            block: false,
            ..full.clone()
        };
        assert_eq!(Maker::parse(&unblocked.encode()), unblocked);
        assert!(full.may_make("x_a", true));
        assert!(!full.may_make("x_a", false));
    }
}
