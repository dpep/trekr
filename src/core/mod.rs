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
    /// Local and instance variable assignments. Extracted but **not stored**:
    /// what a local holds is a question about one file, and `--def` already
    /// reparses that file. Keeping it out of the schema keeps 2 M rows out of
    /// the database for a fact that never crosses a file boundary.
    pub(crate) assigns: Vec<Assign>,
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
        // FNV-1a: no dependency, and the only property needed is that an
        // unrelated edit is overwhelmingly unlikely to land on the same value.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let mut eat = |bytes: &[u8]| {
            for byte in bytes {
                hash ^= *byte as u64;
                hash = hash.wrapping_mul(0x100_0000_01b3);
            }
        };
        for def in &self.defs {
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
    pub(crate) pos: Pos,
    pub(crate) end_line: u32,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Relation {
    Superclass,
    Include,
    Prepend,
    Extend,
}

impl Relation {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Relation::Superclass => "superclass",
            Relation::Include => "include",
            Relation::Prepend => "prepend",
            Relation::Extend => "extend",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Relation> {
        Some(match s {
            "superclass" => Relation::Superclass,
            "include" => Relation::Include,
            "prepend" => Relation::Prepend,
            "extend" => Relation::Extend,
            _ => return None,
        })
    }
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
    ConstCall {
        recv: String,
        name: String,
    },
    /// `y.build` — a call on another local. One step from a typed `y` and no
    /// further: rwr's D61 found 70 % of returns end in another call, so the
    /// recursive version drowns while the single sig-backed step pays.
    LocalCall {
        recv: String,
        name: String,
    },
    /// `[]`, `{}`, `"x"`, `1` — a literal, whose class core now knows.
    Literal(&'static str),
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Assign {
    /// `x` for a local, `@x` for an instance variable.
    pub(crate) target: String,
    pub(crate) value: ValueShape,
    pub(crate) nesting: Vec<String>,
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
    /// Positional argument count, or `None` when a splat makes it unknowable.
    pub(crate) argc: Option<u32>,
    pub(crate) block: bool,
    pub(crate) pos: Pos,
}

/// What an `Other` receiver is, when that is worth knowing.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RecvValue {
    /// `x.gsub(a, b).downcase` — the call at this position (`gsub`), found
    /// again among the file's calls.
    Call(Pos),
    /// `"x".downcase`, `[].push` — a literal of this core class.
    Literal(&'static str),
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
}
