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

#[derive(Clone, Debug, Serialize)]
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
    /// Where a read of an example group's own method was found: its group,
    /// a nested one, an included shared group's body, a helper (DEC-490).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) from: Option<&'static str>,
    /// The name the site writes, when it is not the queried one: `new`, for
    /// a construction that runs `initialize` (DEC-541).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) called_as: Option<&'static str>,
}

const SUPER_UNPLACED: &str = "`super` from a method whose owner the index cannot place";
const SUPER_UNINDEXED: &str = "`super` from a class whose ancestors are not fully indexed";
const BY_SYMBOL: &str = "named by a symbol handed to a macro — invoked by name, receiver unknown";

/// A possible site that names no class it would run in, so it would reach
/// any class's `initialize` as readily as this one's (DEC-541).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unplaced {
    /// `new` on an untyped receiver.
    New,
    /// A `super` whose landing the index cannot place.
    Super,
    /// A symbol handed to a macro.
    Symbol,
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

impl Reference {
    /// What kind of site this is, when it is possible only because nothing
    /// places it.
    pub(crate) fn unplaced(&self) -> Option<Unplaced> {
        if self.tier != Tier::Possible {
            return None;
        }
        match self.why {
            _ if self.called_as.is_some() && self.receiver_type.is_none() => Some(Unplaced::New),
            SUPER_UNPLACED | SUPER_UNINDEXED => Some(Unplaced::Super),
            BY_SYMBOL => Some(Unplaced::Symbol),
            _ => None,
        }
    }

    /// A site ruled out against an index still being filled is not ruled
    /// out: the definition that would take its call may be in a file not read
    /// yet (DEC-320). It is listed as possible, last, with the ruling kept.
    pub(crate) fn unrule(&mut self) {
        if self.tier == Tier::Excluded {
            self.tier = Tier::Possible;
            self.why =
                "ruled out by a partial index, which may not hold the definition it lands on";
            self.proximity = u8::MAX;
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

    /// A site counted already, taken back: a later rule tiered it again.
    pub(crate) fn forget(&mut self, reference: &Reference) {
        let mut taken = Counts::default();
        taken.record(reference);
        self.confirmed -= taken.confirmed;
        self.possible -= taken.possible;
        self.excluded -= taken.excluded;
        self.excluded_different_owner -= taken.excluded_different_owner;
        self.excluded_no_such_method -= taken.excluded_no_such_method;
        self.excluded_arity -= taken.excluded_arity;
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

/// The other name a call of the queried method is written as: `X.new` runs
/// `Class#new`, which calls `initialize` on the instance it makes (DEC-541).
/// Only for an owned query: a bare `initialize` would claim every `new`.
pub(crate) fn constructor_of(query: &Query) -> Option<&'static str> {
    (query.owner.is_some() && !query.singleton && query.name == "initialize").then_some("new")
}

/// The names a call of the queried method is written as.
pub(crate) fn called_as(query: &Query) -> Vec<&str> {
    let mut names = vec![query.name.as_str()];
    names.extend(constructor_of(query));
    names
}

/// Is this call a site of the queried method: one of its name, or an `X.new`
/// that may run it. A `super` in a custom `new` is not one — the `X.new`
/// that runs that `new` already is (DEC-541).
pub(crate) fn names_it(call: &Call, query: &Query) -> bool {
    call.name == query.name
        || constructor_of(query) == Some(call.name.as_str())
            && call.recv != crate::core::RecvShape::Super
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
    tier_call_with(tree, facts, call, path, query, target, None)
}

/// `tier_call`, given what an `X.new` site constructs when the caller has
/// already worked it out (`--dead` asks about every `initialize`).
pub(crate) fn tier_call_with(
    tree: &Tree,
    facts: &Facts,
    call: &Call,
    path: &str,
    query: &Query,
    target: Option<&str>,
    made: Option<&Construct>,
) -> Reference {
    let mut reference = match made {
        _ if call.name == query.name => tier(tree, facts, call, path, query, target),
        Some(made) => tier_construct(tree, made, call, path, query, target),
        None => {
            let made = construct(tree, facts, call, path);
            tier_construct(tree, &made, call, path, query, target)
        }
    };
    // A split name's variant is reported as the name (DEC-072).
    let public = |name: &mut String| *name = crate::tree::public_name(name).to_string();
    reference.receiver_type.as_mut().map(public);
    reference.owner.as_mut().map(public);
    if call.name != query.name {
        reference.called_as = constructor_of(query);
    }
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
        from: None,
        called_as: None,
    };

    if call.recv == crate::core::RecvShape::Super {
        return tier_super(tree, call, path, query, target);
    }

    // A `def` at the top level is Object's, and private (DEC-311). trekr
    // places it on no class, so an implicit call whose receiver has no
    // method of the name is the one that may run it.
    if target == Some("") {
        let explicit = !matches!(
            call.recv,
            crate::core::RecvShape::Implicit | crate::core::RecvShape::SelfRecv
        );
        let receiver = super::receiver_of(tree, facts, call, path);
        let found = receiver
            .as_ref()
            .and_then(|receiver| super::lookup_on(tree, call, receiver));
        let receiver_type = receiver.map(|receiver| receiver.fqn);
        return match found {
            _ if explicit => here(
                Tier::Excluded,
                receiver_type,
                None,
                "a top-level method is private: an explicit receiver cannot call it",
                0,
                Some(Ruling::DifferentOwner),
            ),
            Some(found) => here(
                Tier::Excluded,
                receiver_type,
                Some(found.owner),
                "the receiver's type has a method of its own by this name",
                0,
                Some(Ruling::DifferentOwner),
            ),
            None => here(
                Tier::Possible,
                receiver_type,
                None,
                "a method defined at the top level is every object's",
                1,
                None,
            ),
        };
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
        && target.is_none_or(|target| {
            runs_asked(tree, query, target, (&rival.fqn, rival.singleton), &found)
        })
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
    // A module's own call that no includer answers runs on an Object (DEC-314).
    let found = super::lookup_on(tree, call, &receiver).or_else(|| {
        tree.includers_of(&receiver.fqn)
            .is_empty()
            .then(|| super::on_any_object(tree, &call.name, &receiver))
            .flatten()
    });
    let matches = found.as_ref().is_some_and(|found| {
        target.is_none_or(|target| {
            runs_asked(
                tree,
                query,
                target,
                (&receiver.fqn, receiver.singleton),
                found,
            )
        })
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
        let why = if target.is_some_and(|target| inherited(tree, query, target).is_some()) {
            "`self` may be the subclass that inherits this"
        } else {
            "`self` may be a subclass that overrides this"
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
    // A template that several controllers may render runs the method of
    // whichever does: each landing is a possibility, none a confirmation
    // (DEC-521).
    if matches!(receiver.via, "view" | "rabl")
        && receiver.ambiguous
        && let Some(target) = target
        && std::iter::once(receiver.fqn.as_str())
            .chain(receiver.rivals.iter().map(|(rival, _)| rival.as_str()))
            .any(|class| {
                tree.lookup(class, false, &call.name)
                    .is_some_and(|landed| runs_asked(tree, query, target, (class, false), &landed))
            })
    {
        return here(
            Tier::Possible,
            Some(receiver.fqn.clone()),
            found.map(|found| found.owner),
            "the template may be rendered by controllers whose methods differ",
            1,
            None,
        );
    }
    // A guess among classes that define the name, landing on a method it
    // only inherits, lands wherever its ancestors do — for `to_s`, Kernel's —
    // so it cannot confirm a call of the owner that inherits that method.
    if matches
        && receiver.ambiguous
        && let (Some(found), Some(target)) = (found.as_ref(), target)
        && !(found.owner == target && found.singleton == query.singleton)
    {
        return here(
            Tier::Possible,
            Some(receiver.fqn.clone()),
            Some(found.owner.clone()),
            "the receiver's type is a guess, and one that inherits this",
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
        let below = target.is_some_and(|target| tree.inherits(target, &receiver.fqn));
        let why = if below && target.is_some_and(|target| inherited(tree, query, target).is_some())
        {
            "the receiver is typed as an ancestor, and may be the subclass that inherits this"
        } else if below {
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
            tree.lookup(fqn, false, &query.name).is_some_and(|own| {
                crate::tree::public_name(&own.owner) == target
                    || at_or_below(tree, fqn, target) && is_inherited(tree, query, target, &own)
            })
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
                let why = if target.is_some_and(|target| is_inherited(tree, query, target, &found))
                {
                    "the receiver's type inherits the same method, but is no subclass of the owner"
                } else {
                    "the receiver's type resolves to a different owner"
                };
                here(
                    Tier::Excluded,
                    Some(receiver.fqn.clone()),
                    Some(found.owner.clone()),
                    why,
                    0,
                    Some(Ruling::DifferentOwner),
                )
            }
        }
        // Ruby finds nothing, and the class's `delegate_missing_to` hands the
        // name on: to the target when its type says so (DEC-112).
        None if let Some(answer) = super::handed_on(tree, facts, call, path, &receiver) => {
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
        // `self` is the written scope only if the block it is in runs as it
        // stands, which a method that is not Ruby's own does not promise.
        None if receiver.via == "self" && super::self_unsettled(tree, facts, call, path) => here(
            Tier::Possible,
            Some(receiver.fqn.clone()),
            None,
            "the call is in a block whose `self` the method it is handed to may change",
            1,
            None,
        ),
        // A relation hands a name it lacks to its model's class methods, and
        // the chain did not say which model, or that model's class methods
        // are not all indexed (DEC-444).
        None if !receiver.singleton && super::is_relation(tree, &receiver.fqn) => here(
            Tier::Possible,
            Some(receiver.fqn.clone()),
            None,
            "a relation hands a name it lacks to its model's class methods",
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

/// Does `X.new` on this class reach `Class#new`, and so `initialize`? Each
/// custom `new` on the class side is followed by what it returns (DEC-133):
/// `super` goes on up, another class's `new` makes that class instead.
#[derive(Clone)]
pub(crate) enum Construction {
    /// Every path ends at `Class#new` — or one does, beside others.
    Initializes,
    /// A custom `new` that returns nothing the extractor reads, which may
    /// or may not call `super` or `allocate` and `initialize`.
    Unread,
    /// Only other classes: this `initialize` never runs.
    Elsewhere(String),
}

fn construction(tree: &Tree, class: &str) -> Construction {
    let mut new = tree.lookup(class, true, "new");
    // A chain of `super`s ends at core; the bound only guards a cycle.
    for _ in 0..8 {
        let Some(found) = new.take() else {
            break;
        };
        if crate::tree::is_core(&found.site.path) {
            break;
        }
        let Some(written) = found.returns_for(None, false) else {
            return Construction::Unread;
        };
        if !written.split('|').any(|part| part == "super") {
            return Construction::Elsewhere(found.owner.clone());
        }
        new = tree.after_on_class_side(class, &found, "new");
    }
    // A module has no `Class#new` to reach.
    match tree.kind_of(class) {
        Some("module") => Construction::Elsewhere(class.to_string()),
        _ => Construction::Initializes,
    }
}

/// What an `X.new` site constructs, which no query changes: worked out once
/// per site and tiered against each `initialize` asked about (DEC-541).
#[derive(Clone)]
pub(crate) enum Construct {
    /// A receiver the index cannot type: any class's `new`.
    Untyped,
    /// Not a construction of a class: a macro's `:new` (an action, an
    /// option), or `new` sent to an instance.
    Not {
        receiver_type: Option<String>,
        why: &'static str,
    },
    /// `new` on a class the index knows.
    Class {
        class: String,
        /// `self` in a class method, or a class a `sig` bounds, or a guess:
        /// it may be a subclass.
        may_be_below: bool,
        guess: bool,
        made: Construction,
        /// The `initialize` an instance of `class` finds.
        found: Option<Box<crate::tree::MethodDef>>,
        /// Its ancestors the index could not resolve, as written.
        unresolved: Vec<String>,
    },
}

pub(crate) fn construct(tree: &Tree, facts: &Facts, call: &Call, path: &str) -> Construct {
    // `X.public_send(:new, …)` constructs as `X.new(…)` does.
    if call.recv == crate::core::RecvShape::Symbol {
        return match call.stands_for.as_deref() {
            Some(sent) if super::receiver_of(tree, facts, sent, path).is_some() => {
                construct(tree, facts, sent, path)
            }
            Some(_) => Construct::Untyped,
            None => Construct::Not {
                receiver_type: None,
                why: "a symbol handed to a macro names an action or an option, not a class",
            },
        };
    }
    let Some(receiver) = super::receiver_of(tree, facts, call, path) else {
        return Construct::Untyped;
    };
    if !receiver.singleton {
        // An instance of `Class` is a class nothing says which.
        if matches!(receiver.fqn.as_str(), "Class" | "Module") {
            return Construct::Untyped;
        }
        return Construct::Not {
            receiver_type: Some(receiver.fqn),
            why: "the receiver is an instance, so its `new` constructs nothing",
        };
    }
    let class = receiver.fqn;
    Construct::Class {
        may_be_below: receiver.via == "self" || receiver.bound || receiver.ambiguous,
        guess: receiver.ambiguous,
        made: construction(tree, &class),
        found: tree.lookup(&class, false, "initialize").map(Box::new),
        unresolved: tree.ancestors(&class).unresolved.clone(),
        class,
    }
}

/// Can a construction be a typed site of `own`, an `initialize` asked
/// about? Not an untyped one, nor what is no construction, nor a definite
/// class whose `new` runs another `initialize`.
pub(crate) fn may_run(made: &Construct, own: &crate::tree::MethodDef) -> bool {
    match made {
        Construct::Untyped | Construct::Not { .. } => false,
        Construct::Class {
            found: Some(found),
            may_be_below: false,
            made: Construction::Initializes | Construction::Unread,
            ..
        } => found.site.path == own.site.path && found.site.line == own.site.line,
        Construct::Class { .. } => true,
    }
}

/// An `X.new` site, tiered against an `initialize` (DEC-541): a call of the
/// `initialize` an instance of the class it makes finds, as Ruby's
/// `Class#new` calls it.
pub(crate) fn tier_construct(
    tree: &Tree,
    construct: &Construct,
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
        from: None,
        called_as: None,
    };
    let (class, may_be_below, guess, made, found, unresolved) = match construct {
        Construct::Untyped => return possible(tree, call, path, query, target, shape),
        Construct::Not { receiver_type, why } => {
            return here(
                Tier::Excluded,
                receiver_type.clone(),
                None,
                why,
                0,
                Some(Ruling::DifferentOwner),
            );
        }
        Construct::Class {
            class,
            may_be_below,
            guess,
            made,
            found,
            unresolved,
        } => (class, *may_be_below, *guess, made, found, unresolved),
    };
    if let Construction::Elsewhere(owner) = made {
        return here(
            Tier::Excluded,
            Some(class.clone()),
            Some(owner.clone()),
            "the class's own `new` makes another class",
            0,
            Some(Ruling::DifferentOwner),
        );
    }
    let owner = found.as_ref().map(|found| found.owner.clone());
    let matches = found.as_ref().is_some_and(|found| {
        target.is_none_or(|target| runs_asked(tree, query, target, (class, false), found))
    });
    if matches {
        let (tier, why, proximity) = match made {
            _ if guess => (
                Tier::Possible,
                "the receiver's type is a guess, and it constructs one that runs this",
                1,
            ),
            Construction::Unread => (
                Tier::Possible,
                "the class's own `new` runs first, and whether it calls `initialize` is not read",
                1,
            ),
            _ => (
                Tier::Confirmed,
                "`new` on the receiver's class runs this `initialize`",
                0,
            ),
        };
        return here(tier, Some(class.clone()), owner, why, proximity, None);
    }
    if may_be_below && target.is_some_and(|target| below_reaches(tree, class, target, query)) {
        return here(
            Tier::Possible,
            Some(class.clone()),
            owner,
            "the receiver may be a subclass, which this `initialize` is",
            1,
            None,
        );
    }
    match found {
        Some(_) => here(
            Tier::Excluded,
            Some(class.clone()),
            owner,
            "the class it constructs runs a different `initialize`",
            0,
            Some(Ruling::DifferentOwner),
        ),
        // An ancestor the index could not place may be the owner asked
        // about only if it is written with the owner's name: an app's class
        // is no gem's ancestor.
        None if target.is_none_or(|target| {
            let last = target.rsplit("::").next().unwrap_or(target);
            unresolved
                .iter()
                .any(|name| name.rsplit("::").next() == Some(last))
        }) =>
        {
            here(
                Tier::Possible,
                Some(class.clone()),
                None,
                "the receiver's ancestors are not fully indexed",
                1,
                None,
            )
        }
        None => here(
            Tier::Excluded,
            Some(class.clone()),
            None,
            "nothing indexed defines `initialize` on the class it constructs",
            0,
            Some(Ruling::NoSuchMethod),
        ),
    }
}

/// May an object of a class below `fqn` run `target`'s own `query` method:
/// a subclass that defines it (DEC-140), or a subclass that mixes in the
/// module `target` that does, ahead of whatever else defines it (DEC-213)?
fn below_reaches(tree: &Tree, fqn: &str, target: &str, query: &Query) -> bool {
    let lands = |class: &str| {
        tree.lookup(class, query.singleton, &query.name)
            .is_some_and(|own| {
                crate::tree::public_name(&own.owner) == target
                    || is_inherited(tree, query, target, &own)
            })
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

/// Does a receiver of type `on` (a class, and whether its class side) whose
/// lookup found `method` run the queried one? The owner's own method from
/// anywhere, or the method it inherits from the owner or a class below it:
/// another class inheriting the same method is not calling the owner's
/// (DEC-280).
fn runs_asked(
    tree: &Tree,
    query: &Query,
    target: &str,
    (on, singleton): (&str, bool),
    method: &crate::tree::MethodDef,
) -> bool {
    if method.owner == target && method.singleton == query.singleton {
        return true;
    }
    singleton == query.singleton
        && at_or_below(tree, on, target)
        && is_inherited(tree, query, target, method)
}

/// The method the queried owner inherits, when it does not define its own.
fn inherited(tree: &Tree, query: &Query, target: &str) -> Option<crate::tree::MethodDef> {
    tree.lookup_owned(target, query.singleton, &query.name)
        .filter(|asked| crate::tree::public_name(&asked.owner) != target)
}

/// Is `method` the one the queried owner inherits rather than defines?
fn is_inherited(tree: &Tree, query: &Query, target: &str, method: &crate::tree::MethodDef) -> bool {
    inherited(tree, query, target).is_some_and(|asked| {
        asked.owner == method.owner
            && asked.singleton == method.singleton
            && asked.site.path == method.site.path
            && asked.site.line == method.site.line
    })
}

fn at_or_below(tree: &Tree, fqn: &str, target: &str) -> bool {
    crate::tree::public_name(fqn) == target || tree.inherits(fqn, target)
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
        from: None,
        called_as: None,
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
        return here(Tier::Possible, scope.clone(), None, SUPER_UNPLACED, 3, None);
    };
    let is_target = |class: &str, method: &crate::tree::MethodDef| {
        target.is_none_or(|target| runs_asked(tree, query, target, (class, call.singleton), method))
    };
    let found: Vec<&crate::tree::MethodDef> = landings
        .per_class
        .iter()
        .filter_map(|(_, landing)| landing.as_ref())
        .collect();
    let hits = landings
        .per_class
        .iter()
        .filter(|(class, landing)| landing.as_ref().is_some_and(|m| is_target(class, m)))
        .count();
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
        return here(Tier::Possible, owner, None, SUPER_UNINDEXED, 1, None);
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
            from: None,
            called_as: None,
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
            why: BY_SYMBOL,
            ruling: None,
            proximity: 4,
            from: None,
            called_as: None,
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
        from: None,
        called_as: None,
    }
}

/// An `X.new` on an untyped receiver against an `initialize` `own`, unranked:
/// possible unless its arguments do not fit (DEC-541). `possible` ranks it
/// too, which only a listing needs.
pub(crate) fn untyped_construction(
    call: &Call,
    path: &str,
    own: &crate::tree::MethodDef,
) -> Reference {
    let fits = own.accepts(call.argc);
    Reference {
        path: path.to_string(),
        line: call.pos.line,
        col: call.pos.col,
        tier: if fits { Tier::Possible } else { Tier::Excluded },
        receiver: call.recv.as_str(),
        receiver_type: None,
        owner: None,
        why: if fits {
            "untyped receiver, nothing rules it out"
        } else {
            "the argument count does not fit this method"
        },
        ruling: (!fits).then_some(Ruling::Arity),
        proximity: 3,
        from: None,
        called_as: Some("new"),
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

/// Who calls a protocol hook by its name, which no call site writes: Ruby
/// core's conversions and hooks, the stdlib's serializers, and Rails'
/// helpers (DEC-315). `None` for the side it is not a hook on.
pub(crate) fn protocol_hook(name: &str, singleton: bool) -> Option<&'static str> {
    const INSTANCE: &[(&str, &str)] = &[
        ("marshal_dump", "Marshal"),
        ("marshal_load", "Marshal"),
        ("_dump", "Marshal"),
        ("init_with", "YAML (Psych)"),
        ("encode_with", "YAML (Psych)"),
        ("to_s", "string interpolation"),
        ("inspect", "`p` and `inspect`"),
        (
            "instance_variables_to_inspect",
            "`Kernel#inspect` (Ruby 3.4+)",
        ),
        ("pretty_print", "`pp`"),
        ("pretty_print_cycle", "`pp`"),
        ("pretty_print_instance_variables", "`pp`"),
        ("hash", "Hash, for a key"),
        ("eql?", "Hash, for a key"),
        ("==", "`==` and `!=`"),
        ("<=>", "Comparable and `sort`"),
        ("===", "`case`"),
        ("=~", "`case` and `!~`"),
        ("deconstruct", "pattern matching (`in [a, b]`)"),
        ("deconstruct_keys", "pattern matching (`in {a:}`)"),
        ("each", "Enumerable"),
        ("succ", "a Range's iteration"),
        ("call", "`.()` and anything handed a callable"),
        ("to_proc", "`&`"),
        ("coerce", "Numeric arithmetic"),
        ("to_str", "an implicit String conversion"),
        ("to_ary", "an implicit Array conversion"),
        ("to_hash", "an implicit Hash conversion and `**`"),
        ("to_int", "an implicit Integer conversion"),
        ("to_io", "IO"),
        ("to_path", "File and Pathname"),
        ("to_regexp", "`Regexp.union`"),
        ("to_a", "a splat (`*`)"),
        ("respond_to_missing?", "`respond_to?`"),
        ("method_missing", "Ruby's dispatch"),
        (
            "singleton_method_added",
            "Ruby, when a singleton method is defined",
        ),
        (
            "singleton_method_removed",
            "`remove_method` on the singleton class",
        ),
        (
            "singleton_method_undefined",
            "`undef_method` on the singleton class",
        ),
        ("initialize_copy", "`dup` and `clone`"),
        ("initialize_dup", "`dup`"),
        ("initialize_clone", "`clone`"),
        ("to_param", "Rails' URL helpers"),
        ("to_partial_path", "`render`"),
        ("to_model", "Rails' form and URL helpers"),
        ("to_key", "Rails' `dom_id`"),
        ("model_name", "Rails' form, URL and i18n helpers"),
        ("persisted?", "Rails' form and URL helpers"),
        ("to_attachable_partial_path", "Action Text"),
        ("as_json", "`to_json` and `render json:`"),
        ("to_json", "`render json:`"),
        ("serializable_hash", "ActiveModel serialization"),
        ("cache_key", "Rails' cache helpers"),
        ("cache_key_with_version", "Rails' cache helpers"),
        ("cache_version", "Rails' cache helpers"),
        (
            "read_attribute_for_serialization",
            "ActiveModel serializers",
        ),
        ("read_attribute_for_validation", "ActiveModel validations"),
        ("perform", "a job runner (`perform_later`, Sidekiq)"),
    ];
    const SINGLETON: &[(&str, &str)] = &[
        ("_load", "Marshal"),
        ("json_create", "`JSON.parse` with `create_additions`"),
        ("inherited", "Ruby, when it is subclassed"),
        ("included", "`include`"),
        ("extended", "`extend`"),
        ("prepended", "`prepend`"),
        ("append_features", "`include`"),
        ("extend_object", "`extend`"),
        ("prepend_features", "`prepend`"),
        ("method_added", "Ruby, when a method is defined"),
        ("method_removed", "`remove_method`"),
        ("method_undefined", "`undef_method`"),
        (
            "singleton_method_added",
            "Ruby, when a singleton method is defined",
        ),
        (
            "singleton_method_removed",
            "`remove_method` on the singleton class",
        ),
        (
            "singleton_method_undefined",
            "`undef_method` on the singleton class",
        ),
        ("method_missing", "Ruby's dispatch"),
        ("respond_to_missing?", "`respond_to?`"),
        ("const_missing", "constant lookup"),
        (
            "const_added",
            "Ruby, when a constant is defined (Ruby 3.2+)",
        ),
        ("model_name", "Rails' form, URL and i18n helpers"),
        (
            "table_name_prefix",
            "Active Record, for a namespace's models' tables",
        ),
        (
            "table_name_suffix",
            "Active Record, for a namespace's models' tables",
        ),
        (
            "use_relative_model_naming?",
            "Active Model naming, for a namespace's models",
        ),
    ];
    let table = if singleton { SINGLETON } else { INSTANCE };
    table
        .iter()
        .find(|(hook, _)| *hook == name)
        .map(|(_, caller)| *caller)
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

/// The method a query about `owner` lands on, in Ruby's notation, and
/// whether the owner inherits it rather than defining it (DEC-280).
pub(crate) fn resolves_to(tree: &Tree, owner: &str, query: &Query) -> Option<(String, bool)> {
    let method = tree.lookup_owned(owner, query.singleton, &query.name)?;
    let defined_by = crate::tree::public_name(&method.owner);
    let mark = if method.singleton { '.' } else { '#' };
    Some((
        format!("{defined_by}{mark}{}", query.name),
        defined_by != crate::tree::public_name(owner),
    ))
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
