//! References narrowed by receiver — the thing no Ruby tool does.
//!
//! `rg -w save` finds every `save` in the repo. Ruby LSP finds method
//! references only by bare name, and Rubydex does not attribute method calls at
//! all. What makes an answer useful is knowing which of those call sites could
//! actually reach *this* method, and the receiver ladder already knows.
//!
//! Three tiers, and the third is the product:
//!
//! * **confirmed** — the receiver's type resolves and Ruby's lookup from it
//!   lands on the queried method.
//! * **possible** — the receiver is untyped, and nothing rules the site out.
//!   Ranked by proximity, never dropped.
//! * **excluded** — the receiver resolves somewhere *else*, or the arity does
//!   not fit. Not listed, but **counted**: that count is the difference
//!   between this and a grep, so it is reported rather than quietly enjoyed.

use crate::core::{Call, Facts};
use crate::tree::{Site, Tree};
use serde::Serialize;

/// `Widget#save`, `Widget.build`, or a bare `save`.
#[derive(Debug, PartialEq)]
pub(crate) struct Query {
    /// The owner as written. `None` for a bare name, which narrows nothing.
    pub(crate) owner: Option<String>,
    /// `Widget.build` asks about a class method.
    pub(crate) singleton: bool,
    pub(crate) name: String,
}

impl Query {
    /// `#` and `.` are Ruby's own notation for the two kinds of method, and
    /// neither can appear in a method name — so the last one is the separator.
    pub(crate) fn parse(text: &str) -> Query {
        if let Some((owner, name)) = text.rsplit_once('#') {
            return Query {
                owner: Some(owner.to_string()),
                singleton: false,
                name: name.to_string(),
            };
        }
        if let Some((owner, name)) = text.rsplit_once('.') {
            return Query {
                owner: Some(owner.to_string()),
                singleton: true,
                name: name.to_string(),
            };
        }
        Query {
            owner: None,
            singleton: false,
            name: text.to_string(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Tier {
    Confirmed,
    Possible,
    Excluded,
}

#[derive(Debug, Serialize)]
pub(crate) struct Reference {
    pub(crate) path: String,
    pub(crate) line: u32,
    pub(crate) col: u32,
    pub(crate) tier: Tier,
    /// The receiver's syntactic shape — always, because it is the reason.
    pub(crate) receiver: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) receiver_type: Option<String>,
    /// Where Ruby's lookup from that receiver actually lands.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) owner: Option<String>,
    pub(crate) why: &'static str,
    /// Which of the three exclusion reasons applied, when one did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ruling: Option<Ruling>,
    /// Ranking tier within `possible`; lower is nearer. Not a score — the
    /// `why` string names it, and no weights are invented (DEC-011).
    #[serde(skip)]
    pub(crate) proximity: u8,
}

impl Tier {
    /// Confirmed before possible; excluded is never listed.
    pub(crate) fn rank(self) -> u8 {
        match self {
            Tier::Confirmed => 0,
            Tier::Possible => 1,
            Tier::Excluded => 2,
        }
    }
}

impl Counts {
    pub(crate) fn record(&mut self, reference: &Reference) {
        match reference.tier {
            Tier::Confirmed => self.confirmed += 1,
            Tier::Possible => self.possible += 1,
            Tier::Excluded => {
                self.excluded += 1;
                match reference.ruling {
                    Some(Ruling::DifferentOwner) => self.excluded_different_owner += 1,
                    Some(Ruling::NoSuchMethod) => self.excluded_no_such_method += 1,
                    Some(Ruling::Arity) | None => self.excluded_arity += 1,
                }
            }
        }
    }
}

/// Why a call site was ruled out. The three reasons are **not** equally
/// strong, and blending them into one number would overclaim the weakest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Ruling {
    /// The receiver's type resolves and Ruby's lookup lands on a *different*
    /// method. Positive evidence; this call provably is not the queried one.
    DifferentOwner,
    /// The receiver's type resolves and nothing indexed defines this name on
    /// it. Weaker: Rails writes `delegate :where, to: :all`, and a method
    /// defined by a DSL is absent from the index without being absent from the
    /// program. Right when the target is some other class — which is the usual
    /// case — and wrong if the queried owner is itself dynamic.
    NoSuchMethod,
    /// The argument count does not fit the definition we have. Sound against
    /// *that* definition, which is all a syntactic check can claim.
    Arity,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct Counts {
    pub(crate) confirmed: usize,
    pub(crate) possible: usize,
    /// Same-name call sites the receiver ruled out — the number a grep cannot
    /// produce. Broken down, because the reasons differ in strength.
    pub(crate) excluded: usize,
    pub(crate) excluded_different_owner: usize,
    pub(crate) excluded_no_such_method: usize,
    pub(crate) excluded_arity: usize,
}

/// One call site, tiered against the query.
///
/// `target` is the queried method's owner, already resolved — `None` for a bare
/// name, which can confirm nothing because there is nothing to confirm against.
pub(crate) fn tier_call(
    tree: &Tree,
    facts: &Facts,
    call: &Call,
    path: &str,
    query: &Query,
    target: Option<&str>,
) -> Reference {
    let shape = call.recv.as_str();
    let here = |tier, receiver_type, owner, why, proximity, ruling| Reference {
        path: path.to_string(),
        line: call.pos.line,
        col: call.pos.col,
        tier,
        receiver: shape,
        receiver_type,
        owner,
        why,
        ruling,
        proximity,
    };

    let Some(receiver) = super::receiver_of(tree, facts, call) else {
        return possible(tree, call, path, query, target, shape);
    };

    match tree.lookup(&receiver.fqn, receiver.singleton, &call.name) {
        Some(found) => {
            let matches = target
                .is_none_or(|target| found.owner == target && found.singleton == query.singleton);
            if matches {
                here(
                    Tier::Confirmed,
                    Some(receiver.fqn.clone()),
                    Some(found.owner.clone()),
                    "the receiver's type resolves here",
                    0,
                    None,
                )
            } else {
                here(
                    Tier::Excluded,
                    Some(receiver.fqn.clone()),
                    Some(found.owner.clone()),
                    "the receiver's type resolves to a different owner",
                    0,
                    Some(Ruling::DifferentOwner),
                )
            }
        }
        // The type is settled and Ruby finds nothing — unless the chain was cut
        // short, in which case the missing ancestor could be the target.
        None if tree.ancestors(&receiver.fqn).unresolved.is_empty() => here(
            Tier::Excluded,
            Some(receiver.fqn.clone()),
            None,
            "nothing indexed defines this name on the receiver's type",
            0,
            Some(Ruling::NoSuchMethod),
        ),
        None => here(
            Tier::Possible,
            Some(receiver.fqn.clone()),
            None,
            "the receiver's ancestors are not fully indexed",
            1,
            None,
        ),
    }
}

/// An untyped receiver: rank it rather than drop it.
fn possible(
    tree: &Tree,
    call: &Call,
    path: &str,
    query: &Query,
    target: Option<&str>,
    shape: &'static str,
) -> Reference {
    let definition = target.and_then(|target| tree.lookup(target, query.singleton, &query.name));

    // Arity is the one thing a syntactic check can rule out outright, and it is
    // already stored.
    if let Some(definition) = &definition
        && !definition.accepts(call.argc)
    {
        return Reference {
            path: path.to_string(),
            line: call.pos.line,
            col: call.pos.col,
            tier: Tier::Excluded,
            receiver: shape,
            receiver_type: None,
            owner: None,
            why: "the argument count does not fit this method",
            ruling: Some(Ruling::Arity),
            proximity: 0,
        };
    }

    let scope = tree.scope_fqn(&call.nesting);
    let (proximity, why) = match (&scope, target) {
        (Some(scope), Some(target)) if tree.ancestors(scope).chain.iter().any(|a| a == target) => (
            0,
            "untyped receiver, but the enclosing class inherits from the owner",
        ),
        _ if definition.as_ref().is_some_and(|d| d.site.path == path) => {
            (1, "untyped receiver, same file as the definition")
        }
        (Some(scope), Some(target)) if shares_namespace(scope, target) => (
            2,
            "untyped receiver, enclosing class shares a namespace with the owner",
        ),
        _ => (3, "untyped receiver, nothing rules it out"),
    };
    Reference {
        path: path.to_string(),
        line: call.pos.line,
        col: call.pos.col,
        tier: Tier::Possible,
        receiver: shape,
        receiver_type: None,
        owner: None,
        why,
        ruling: None,
        proximity,
    }
}

fn shares_namespace(one: &str, other: &str) -> bool {
    match (one.rsplit_once("::"), other.rsplit_once("::")) {
        (Some((a, _)), Some((b, _))) => a == b,
        _ => false,
    }
}

/// Sort key: tier, then proximity within it, then source order.
pub(crate) fn order(reference: &Reference) -> (u8, u8, String, u32) {
    (
        reference.tier.rank(),
        reference.proximity,
        reference.path.clone(),
        reference.line,
    )
}

/// Where the queried method is defined, if the owner resolves.
pub(crate) fn definition_of(tree: &Tree, query: &Query) -> (Option<String>, Vec<Site>) {
    let Some(written) = &query.owner else {
        return (None, Vec::new());
    };
    let Some(owner) = tree.resolve(written, &[]).fqn else {
        return (None, Vec::new());
    };
    let sites = tree
        .lookup(&owner, query.singleton, &query.name)
        .map(|method| vec![method.site.clone()])
        .unwrap_or_default();
    (Some(owner), sites)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_both_of_rubys_method_notations() {
        assert_eq!(
            Query::parse("Widget#save"),
            Query {
                owner: Some("Widget".into()),
                singleton: false,
                name: "save".into()
            }
        );
        assert_eq!(
            Query::parse("Shop::Widget.build"),
            Query {
                owner: Some("Shop::Widget".into()),
                singleton: true,
                name: "build".into()
            }
        );
        assert_eq!(
            Query::parse("save!"),
            Query {
                owner: None,
                singleton: false,
                name: "save!".into()
            },
            "a bare name narrows nothing, and `!` is part of it"
        );
    }
}
