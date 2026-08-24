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
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Facts {
    pub(crate) defs: Vec<Def>,
    pub(crate) ancestry: Vec<Ancestry>,
    pub(crate) const_refs: Vec<ConstRef>,
    pub(crate) calls: Vec<Call>,
    /// Prism reported syntax errors; the facts above are what survived.
    pub(crate) parse_errors: usize,
    pub(crate) lines: usize,
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
