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
//! * **possible** — the receiver is untyped, or typed as an ancestor of the
//!   queried owner that may be it, and nothing rules the site out. Ranked by
//!   proximity, never dropped.
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

    /// Another run's counts, added to these.
    pub(crate) fn add(&mut self, other: &Counts) {
        self.confirmed += other.confirmed;
        self.possible += other.possible;
        self.excluded += other.excluded;
        self.excluded_different_owner += other.excluded_different_owner;
        self.excluded_no_such_method += other.excluded_no_such_method;
        self.excluded_arity += other.excluded_arity;
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
    let mut reference = tier(tree, facts, call, path, query, target);
    // A split name's variant is reported as the name (DEC-072).
    let public = |name: &mut String| *name = crate::tree::public_name(name).to_string();
    reference.receiver_type.as_mut().map(public);
    reference.owner.as_mut().map(public);
    reference
}

fn tier(
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

    if call.recv == crate::core::RecvShape::Super {
        return tier_super(tree, call, path, query, target);
    }

    // A group's own method, found through the group as `--def` finds it:
    // a `let`, a `def` in the group, a shared group's (DEC-113).
    if let Some(member) = super::example_member(tree, facts, call, path) {
        let owner = member.owner();
        let receiver_type = Some(crate::core::rspec::class_name(&call.nesting));
        return if target.is_none_or(|target| owner == target) {
            here(
                Tier::Confirmed,
                receiver_type,
                Some(owner),
                "the example group defines it",
                0,
                None,
            )
        } else {
            here(
                Tier::Excluded,
                receiver_type,
                Some(owner),
                "the example group defines its own",
                0,
                Some(Ruling::DifferentOwner),
            )
        };
    }
    let Some(receiver) = super::receiver_of(tree, facts, call, path) else {
        return possible(tree, call, path, query, target, shape);
    };

    // The type most writes give lacks the name, and another write's has
    // it: the call may run that one (DEC-165).
    let rival = super::rival_with(tree, call, receiver.clone());
    if rival.fqn != receiver.fqn
        && let Some(found) = super::lookup_on(tree, call, &rival)
        && target.is_none_or(|target| found.owner == target && found.singleton == query.singleton)
    {
        return here(
            Tier::Possible,
            Some(rival.fqn.clone()),
            Some(found.owner),
            "another write the receiver's read can see gives it a type that has this",
            1,
            None,
        );
    }
    let found = super::lookup_on(tree, call, &receiver);
    let matches = found.as_ref().is_some_and(|found| {
        target.is_none_or(|target| found.owner == target && found.singleton == query.singleton)
    });
    // `self` is typed as the class the call is written in, but runs as any
    // subclass: Base#run calling `setup` reaches Child#setup. The template
    // method pattern, and a hook the base never defines is the same shape.
    if receiver.via == "self"
        && !matches
        && target.is_some_and(|target| {
            receiver.singleton == query.singleton && tree.inherits(target, &receiver.fqn)
        })
    {
        return here(
            Tier::Possible,
            Some(receiver.fqn.clone()),
            found.map(|found| found.owner),
            "`self` may be a subclass that overrides this",
            1,
            None,
        );
    }
    // In a module, `self` is whatever includes it, and a method another of
    // the includer's modules defines is one it can reach: a module calling
    // what it expects its includer to provide.
    if receiver.via == "self"
        && !receiver.singleton
        && !query.singleton
        && !matches
        && tree.kind_of(&receiver.fqn) == Some("module")
        && target.is_some_and(|target| {
            let landing = found.as_ref().map(|found| found.owner.as_str());
            tree.includer_reaches(&receiver.fqn, target, landing)
        })
    {
        return here(
            Tier::Possible,
            Some(receiver.fqn.clone()),
            found.map(|found| found.owner),
            "`self` is what includes this module, and one that does has this",
            1,
            None,
        );
    }
    // A type guessed from what a name's definitions return can confirm a
    // site, but ruling one out on it would rule out what the definitions that
    // declare nothing might have returned.
    if receiver.via == "chain:name" && receiver.ambiguous && !matches {
        return here(
            Tier::Possible,
            Some(receiver.fqn.clone()),
            found.map(|found| found.owner),
            "the receiver's type is a guess from what the previous call can return",
            1,
            None,
        );
    }
    // A declared type is an upper bound: a receiver a `sig` says is a `Base`
    // may be the `Child` whose own method is the one asked about, and then
    // that is what runs. `X.new` is exactly an `X`, so it never is (DEC-140).
    if receiver.bound
        && !matches
        && target.is_some_and(|target| {
            receiver.singleton == query.singleton
                && below_reaches(tree, &receiver.fqn, target, query)
        })
    {
        let why = if target.is_some_and(|target| tree.inherits(target, &receiver.fqn)) {
            "the receiver is typed as an ancestor, and may be the subclass that defines this"
        } else {
            "the receiver is typed as an ancestor, and may be a subclass that mixes this in"
        };
        return here(
            Tier::Possible,
            Some(receiver.fqn.clone()),
            found.map(|found| found.owner),
            why,
            1,
            None,
        );
    }
    // A delegate sends the name to its `to:` target: the site counts for
    // the method that target's type runs (DEC-166).
    if !matches
        && let (Some(landed), Some(target)) = (found.as_ref(), target)
        && let Some(sent) = delegated(tree, &receiver, landed)
    {
        let owns = |fqn: &str| {
            tree.lookup(fqn, false, &query.name)
                .is_some_and(|own| crate::tree::public_name(&own.owner) == target)
        };
        let answer = match sent {
            Delegated::To { fqn, .. } if !query.singleton && owns(&fqn) => Some((
                Tier::Confirmed,
                "the receiver's class delegates this to a value whose type runs it",
                0,
            )),
            Delegated::To { fqn, bound: true }
                if !query.singleton && below_reaches(tree, &fqn, target, query) =>
            {
                Some((
                    Tier::Possible,
                    "the receiver's class delegates this to a value that may be the subclass defining it",
                    1,
                ))
            }
            Delegated::To { .. } => None,
            Delegated::Untyped => Some((
                Tier::Possible,
                "the receiver's class delegates this to a value of no known type",
                1,
            )),
        };
        if let Some((tier, why, proximity)) = answer {
            return here(
                tier,
                Some(receiver.fqn.clone()),
                Some(landed.owner.clone()),
                why,
                proximity,
                None,
            );
        }
    }
    match found {
        Some(found) => {
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
        // Ruby finds nothing, and the class's `delegate_missing_to` hands the
        // name on: to the target when its type says so (DEC-112).
        None if let Some(answer) = super::handed_on(tree, call, path, &receiver) => {
            match answer.owner {
                Some(owner) if answer.status != super::Status::Residue => {
                    let (tier, why, ruling) = if target.is_none_or(|target| owner == target) {
                        (
                            Tier::Confirmed,
                            "the receiver hands the name on to here",
                            None,
                        )
                    } else {
                        (
                            Tier::Excluded,
                            "the receiver hands the name on to a different owner",
                            Some(Ruling::DifferentOwner),
                        )
                    };
                    here(tier, answer.receiver_type, Some(owner), why, 0, ruling)
                }
                _ => here(
                    Tier::Possible,
                    Some(receiver.fqn.clone()),
                    None,
                    "the receiver's class delegates missing names to a value of no known type",
                    1,
                    None,
                ),
            }
        }
        // Nothing indexed defines it, and the receiver's `method_missing`
        // sends it on to an object of any class (DEC-261).
        None if tree
            .forwarder_in_chain(&receiver.fqn, receiver.singleton)
            .is_some() =>
        {
            here(
                Tier::Possible,
                Some(receiver.fqn.clone()),
                None,
                "the receiver's class sends a name it lacks on to another object",
                1,
                None,
            )
        }
        // Nothing indexed defines it, but the queried owner defines methods
        // its source does not name, and this receiver is one of it (DEC-130).
        None if unnamed_reach(tree, &receiver, query, target) => here(
            Tier::Possible,
            Some(receiver.fqn.clone()),
            None,
            "the receiver's class defines methods its source does not name",
            1,
            None,
        ),
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

/// May an object of a class below `fqn` run `target`'s own `query` method:
/// a subclass that defines it (DEC-140), or a subclass that mixes in the
/// module `target` that does, ahead of whatever else defines it (DEC-213)?
fn below_reaches(tree: &Tree, fqn: &str, target: &str, query: &Query) -> bool {
    let lands = |class: &str| {
        tree.lookup(class, query.singleton, &query.name)
            .is_some_and(|own| crate::tree::public_name(&own.owner) == target)
    };
    if tree.inherits(target, fqn) {
        return lands(target);
    }
    !query.singleton
        && tree.kind_of(target) == Some("module")
        && tree
            .mixers_of(target)
            .iter()
            .any(|class| tree.inherits(class, fqn) && lands(class))
}

/// Where a delegate sends its name (DEC-166).
pub(super) enum Delegated {
    /// A value of this type; `bound` when it may be a subclass.
    To {
        fqn: String,
        bound: bool,
    },
    Untyped,
}

/// What a `delegate … to: :x` the call landed on sends the name to: the type
/// `x`'s reader declares, or for `all`/`unscoped` on a model's class side,
/// its relation. `None` for a method that is no delegate.
pub(super) fn delegated(
    tree: &Tree,
    receiver: &super::Receiver,
    landed: &crate::tree::MethodDef,
) -> Option<Delegated> {
    if landed.via.as_deref() != Some("delegate") {
        return None;
    }
    let to = landed.forwards_to.as_deref()?;
    let reader = tree.lookup(&receiver.fqn, receiver.singleton, to);
    if let Some(reader) = &reader
        && let Some(returns) = reader.returns_for(Some(0), false)
        && let Some(fqn) = tree.returned_class(reader, returns)
    {
        return Some(Delegated::To { fqn, bound: true });
    }
    const BASE: &str = "ActiveRecord::Base";
    let model = receiver.singleton
        && (crate::tree::public_name(&receiver.fqn) == BASE || tree.inherits(&receiver.fqn, BASE));
    if model && matches!(to, "all" | "unscoped") && tree.is_known(super::RELATION) {
        return Some(Delegated::To {
            fqn: super::RELATION.to_string(),
            bound: true,
        });
    }
    Some(Delegated::Untyped)
}

/// Could a receiver whose lookup found nothing still reach the queried
/// method, because its owner defines methods by a name no literal spells?
/// Only when the receiver is the owner or inherits from it: a marker on some
/// other class defines that class's methods, not the queried one.
fn unnamed_reach(
    tree: &Tree,
    receiver: &super::Receiver,
    query: &Query,
    target: Option<&str>,
) -> bool {
    let Some(target) = target else {
        return tree
            .dynamic_in_chain(&receiver.fqn, receiver.singleton, &query.name)
            .is_some();
    };
    tree.dynamic_in_chain(target, query.singleton, &query.name)
        .is_some()
        && tree
            .lookup_chain(&receiver.fqn, receiver.singleton)
            .iter()
            .any(|(owner, _)| crate::tree::public_name(owner) == target)
}

/// A `super` site, tiered by where it lands from each class that can run it.
///
/// Confirmed only when every such class lands on the queried method; some of
/// them is `possible`, because which one runs depends on the object.
fn tier_super(
    tree: &Tree,
    call: &Call,
    path: &str,
    query: &Query,
    target: Option<&str>,
) -> Reference {
    let here = |tier, receiver_type, owner, why, proximity, ruling| Reference {
        path: path.to_string(),
        line: call.pos.line,
        col: call.pos.col,
        tier,
        receiver: call.recv.as_str(),
        receiver_type,
        owner,
        why,
        ruling,
        proximity,
    };
    let scope = tree.scope_fqn(&call.nesting).filter(|s| tree.is_known(s));
    // `super` looks after its own method's owner, so the method it is written
    // in is the one method it cannot reach — short of a chain that holds the
    // owner twice, which the lookup below sees exactly.
    let own = scope.as_deref().zip(target).is_some_and(|(scope, target)| {
        crate::tree::public_name(scope) == target
            && call.name == query.name
            && call.singleton == query.singleton
    });
    let never_itself = || {
        here(
            Tier::Excluded,
            scope.clone(),
            None,
            "a method's `super` never lands on the method itself",
            0,
            Some(Ruling::DifferentOwner),
        )
    };
    let Ok(landings) = super::super_landings(tree, call, path) else {
        if own {
            return never_itself();
        }
        return here(
            Tier::Possible,
            scope.clone(),
            None,
            "`super` from a method whose owner the index cannot place",
            3,
            None,
        );
    };
    let is_target = |method: &crate::tree::MethodDef| {
        target.is_none_or(|target| method.owner == target && method.singleton == query.singleton)
    };
    let found: Vec<&crate::tree::MethodDef> = landings
        .per_class
        .iter()
        .filter_map(|(_, landing)| landing.as_ref())
        .collect();
    let hits = found.iter().filter(|m| is_target(m)).count();
    let owner = Some(landings.owner.clone());
    let landed = found.first().map(|m| m.owner.clone());
    if hits > 0 && hits == landings.per_class.len() {
        return here(
            Tier::Confirmed,
            owner,
            landed,
            "`super` from an override lands here",
            0,
            None,
        );
    }
    if hits > 0 {
        return here(
            Tier::Possible,
            owner,
            None,
            "`super` lands here from some of the classes that mix its module in",
            0,
            None,
        );
    }
    if own {
        return never_itself();
    }
    if !super::unresolved_behind(tree, &landings).is_empty() && could_hide(tree, &landings, target)
    {
        return here(
            Tier::Possible,
            owner,
            None,
            "`super` from a class whose ancestors are not fully indexed",
            1,
            None,
        );
    }
    match landed {
        Some(elsewhere) => here(
            Tier::Excluded,
            owner,
            Some(elsewhere),
            "`super` lands on a different owner",
            0,
            Some(Ruling::DifferentOwner),
        ),
        None => here(
            Tier::Excluded,
            owner,
            None,
            "nothing indexed after the method's owner defines this name",
            0,
            Some(Ruling::NoSuchMethod),
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

    // A symbol handed to a macro invokes by name and says nothing about the
    // receiver, so it is never confirmable — but it *is* a reference, and the
    // reason has to say which kind so a caller can weigh it.
    if call.recv == crate::core::RecvShape::Symbol {
        return Reference {
            path: path.to_string(),
            line: call.pos.line,
            col: call.pos.col,
            tier: Tier::Possible,
            receiver: shape,
            receiver_type: None,
            owner: None,
            why: "named by a symbol handed to a macro — invoked by name, receiver unknown",
            ruling: None,
            proximity: 4,
        };
    }

    let scope = tree
        .scope_fqn(&call.nesting)
        .map(|scope| tree.variant_at(&scope, path));
    let inherits = |scope: &str, target: &str| {
        tree.ancestors(scope)
            .chain
            .iter()
            .any(|a| crate::tree::public_name(a) == target)
    };
    let (proximity, why) = match (&scope, target) {
        (Some(scope), Some(target)) if inherits(scope, target) => (
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

/// What `--dead` makes of one method's references (DEC-038).
/// Could the queried owner be one of the ancestors the index could not see?
///
/// An unseen module may include any module, so a module target always could.
/// A class enters a chain only as a superclass, so it hides only behind a
/// superclass line that stops short of `BasicObject`, never behind a mixin.
fn could_hide(tree: &Tree, landings: &super::SuperLandings, target: Option<&str>) -> bool {
    let Some(target) = target else {
        return true;
    };
    if tree.kind_of(target) != Some("class") {
        return true;
    }
    landings.per_class.iter().any(|(class, _)| {
        !tree
            .ancestors(class)
            .chain
            .iter()
            .any(|a| a == "BasicObject")
    })
}

#[derive(Debug, PartialEq)]
pub(crate) struct Liveness {
    /// `None` when the method is plainly referenced and not a candidate.
    pub(crate) tier: Option<&'static str>,
    pub(crate) by_symbol: usize,
    pub(crate) by_super: usize,
    /// The owners of the overrides whose `super` reaches it.
    pub(crate) super_from: Vec<String>,
}

/// Tier a method by the references that survived narrowing.
///
/// Two kinds are split out of the written calls because they mean something
/// else. A symbol handed to a macro invokes by name — real use, and the shape
/// most likely to be coincidence. A `super` from an override makes the method
/// live exactly when that override is: neither unused nor something to inline
/// into its one caller, so it gets its own tier.
/// A reference that is an ordinary written call: not a symbol handed to a
/// macro, and not a `super` from an override the index can name.
pub(crate) fn is_written_call(reference: &Reference) -> bool {
    reference.tier != Tier::Excluded
        && reference.receiver != "symbol"
        && !(reference.receiver == "super" && reference.receiver_type.is_some())
}

pub(crate) fn liveness(found: &[Reference], counts: &Counts) -> Liveness {
    let by_symbol = found.iter().filter(|r| r.receiver == "symbol").count();
    // A `super` whose method has no owner the index knows comes from nowhere
    // that can be named, so it counts as an ordinary call of unknown origin.
    let supers: Vec<&Reference> = found
        .iter()
        .filter(|r| r.receiver == "super" && r.receiver_type.is_some())
        .collect();
    let written = (counts.confirmed + counts.possible).saturating_sub(by_symbol + supers.len());
    let tier = match (written, supers.len(), by_symbol) {
        (0, 0, 0) => Some("unreferenced"),
        (0, 0, _) => Some("convention-only"),
        (0, _, _) => Some("super-only"),
        (1, _, _) => Some("single-caller"),
        _ => None,
    };
    let mut super_from: Vec<String> = Vec::new();
    for owner in supers.iter().filter_map(|r| r.receiver_type.as_ref()) {
        if !super_from.contains(owner) {
            super_from.push(owner.clone());
        }
    }
    Liveness {
        tier,
        by_symbol,
        by_super: supers.len(),
        super_from,
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
    // A name two programs declare differently has a definition in each
    // (DEC-072); asked about by name, it is every one of them.
    let variants = tree.variants_of(&owner);
    let classes = if variants.is_empty() {
        vec![owner.clone()]
    } else {
        variants
    };
    let mut sites: Vec<Site> = Vec::new();
    for class in &classes {
        if let Some(method) = tree.lookup(class, query.singleton, &query.name)
            && !sites
                .iter()
                .any(|s| s.path == method.site.path && s.line == method.site.line)
        {
            sites.push(method.site.clone());
        }
    }
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
