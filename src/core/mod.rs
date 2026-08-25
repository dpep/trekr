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
}

impl Facts {
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
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
    /// Parameter name → class, from the `params(...)` half of a `sig`.
    ///
    /// Not stored: a parameter can only be a receiver inside the method that
    /// declares it, and `--def` reparses that file anyway. Keeping it out of
    /// the schema is the same call as `Facts::assigns` (DEC-012).
    pub(crate) sig_params: Vec<(String, String)>,
    pub(crate) pos: Pos,
    pub(crate) end_line: u32,
}

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
    /// Positional argument count, or `None` when a splat makes it unknowable.
    pub(crate) argc: Option<u32>,
    pub(crate) block: bool,
    pub(crate) pos: Pos,
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
    /// Anything else — a chain, a literal, a block param, `super`.
    Other,
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
