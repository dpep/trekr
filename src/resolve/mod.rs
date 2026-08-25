//! Layer 3: which method does this call site run?
//!
//! The ladder, in the order rwr measured to pay (PLAN §2):
//!
//! | rung | share of call sites | what it costs |
//! |---|---|---|
//! | implicit / explicit `self` | ~45 % | nothing — the enclosing scope *is* the receiver |
//! | constant receiver | ~11 % | one constant resolution |
//! | local from `X.new` or an identity method | ~14 % | an assignment scan of the file |
//! | instance variable | ~3 % | the same scan |
//! | inline Sorbet `sig` | — | a second method lookup |
//!
//! Everything below that is residue, and residue is not nothing: it comes back
//! as ordered candidates with the receiver shape as the reason.

pub(crate) mod refs;

use crate::core::{Assign, Call, Facts, RecvShape, ValueShape};
use crate::tree::{Site, Status, Tree};
use serde::Serialize;

/// How the receiver's type was established, and how strongly.
pub(super) struct Receiver {
    pub(super) fqn: String,
    /// A class-method lookup rather than an instance-method one.
    pub(super) singleton: bool,
    pub(super) via: &'static str,
    /// Assignments that agreed on this type, out of those considered. For the
    /// rungs that are a language rule rather than an inference, both are 1.
    pub(super) agreeing: usize,
    pub(super) total: usize,
    /// The rung picked a winner that other definitions could equally have
    /// been. Only a convention-based rung sets this; a language rule cannot be
    /// ambiguous about what the receiver is.
    pub(super) ambiguous: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct Candidate {
    pub(crate) owner: String,
    pub(crate) singleton: bool,
    /// Why this candidate is ranked where it is — a named tier, not a weight.
    pub(crate) why: &'static str,
    pub(crate) site: Site,
}

#[derive(Debug, Serialize)]
pub(crate) struct MethodAnswer {
    pub(crate) status: Status,
    /// 1 when the receiver's type is settled and Ruby's lookup finds the method
    /// in it. For the assignment rungs it is the share of assignments that
    /// agreed — a count, not a calibration (DEC-011).
    pub(crate) confidence: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) resolved_via: Option<String>,
    /// The receiver's syntactic shape, always — it is the reason a residue is a
    /// residue.
    pub(crate) receiver: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) receiver_type: Option<String>,
    /// Whether that type is a `class` or a `module`. It matters more than it
    /// looks: for an implicit receiver inside a **module**, the enclosing scope
    /// is not the real receiver — whatever includes the module is — so a miss
    /// there is expected rather than a failure of the lookup.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) receiver_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) owner: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) sites: Vec<Site>,
    /// Assignments that agreed / were considered, when a rung inferred a type.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) agreement: Option<String>,
    /// Ancestors of the receiver's type that we could not resolve. A "not
    /// found" is only as trustworthy as this list is short: a method defined in
    /// an unindexed gem ancestor looks exactly like a method that does not
    /// exist.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) unresolved_ancestors: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) candidates: Vec<Candidate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<String>,
}

/// How many candidates a residue answer offers. Enough to be useful, few
/// enough that an agent is not being handed Ruby LSP's "first ten" by another
/// name — these are ordered by named evidence, and the count is disclosed.
const MAX_CANDIDATES: usize = 8;

/// `path` is the call site's file, relative to the checkout — one of the tiers
/// residue candidates are ordered by.
pub(crate) fn method_at(tree: &Tree, facts: &Facts, call: &Call, path: &str) -> MethodAnswer {
    let shape = call.recv.as_str();
    match receiver_of(tree, facts, call) {
        Some(receiver) => {
            match tree.lookup(&receiver.fqn, receiver.singleton, &call.name) {
                Some(found) => {
                    // A method Tapioca generated has no source of its own. Send
                    // the caller to the class that generates it rather than to
                    // the .rbi, which is where Sorbet would have left them.
                    let generated = found.site.is_dsl_rbi();
                    let sites = if generated {
                        let real: Vec<Site> = tree
                            .sites(&found.owner)
                            .iter()
                            .filter(|site| !site.is_dsl_rbi())
                            .cloned()
                            .collect();
                        // If the class itself only exists in the RBI there is
                        // nowhere better to point, so keep what we have.
                        if real.is_empty() {
                            vec![found.site.clone()]
                        } else {
                            real
                        }
                    } else {
                        vec![found.site.clone()]
                    };
                    MethodAnswer {
                        status: if receiver.ambiguous {
                            Status::Ambiguous
                        } else {
                            Status::Resolved
                        },
                        confidence: share(receiver.agreeing, receiver.total),
                        resolved_via: Some(if generated {
                            "rbi_dsl".to_string()
                        } else {
                            receiver.via.to_string()
                        }),
                        receiver: shape,
                        receiver_kind: tree.kind_of(&receiver.fqn).map(str::to_string),
                        receiver_type: Some(receiver.fqn.clone()),
                        owner: Some(found.owner.clone()),
                        sites,
                        agreement: agreement(&receiver),
                        unresolved_ancestors: Vec::new(),
                        // An ambiguous answer is the one case where competitors
                        // are known to exist — that is what made it ambiguous —
                        // so listing them is not hedging, it is the disclosure.
                        // A resolved answer has none to list.
                        candidates: if receiver.ambiguous {
                            competitors(tree, &call.name, &found.owner)
                        } else {
                            Vec::new()
                        },
                        reason: None,
                    }
                }
                // A call written inside a module has no receiver of its own:
                // whatever includes the module is the receiver. When the index
                // knows which class that is, the call is determinate after all.
                None if tree.kind_of(&receiver.fqn) == Some("module") => {
                    match via_includers(tree, call, &receiver) {
                        Some(answer) => answer,
                        None => residue(
                            tree,
                            call,
                            path,
                            Some(receiver),
                            "the call is inside a module, and no class the index \
                             knows of mixes it in and defines this name",
                        ),
                    }
                }
                // The type is settled and Ruby would still not find the method
                // here. That is a different "no" from an unknown receiver, and
                // usually means a gem, a DSL, or `method_missing`.
                None => residue(
                    tree,
                    call,
                    path,
                    Some(receiver),
                    "the receiver's type is known but nothing in its ancestors \
                     defines this name — a gem, a DSL, or method_missing",
                ),
            }
        }
        None => residue(
            tree,
            call,
            path,
            None,
            "the receiver's type is not determined by this file",
        ),
    }
}

/// Resolve a call written inside a module by asking the classes that mix it in.
///
/// `ActiveRecord::Transactions#destroyed?` is not defined in `Transactions`; it
/// is defined in `Persistence`, and the two only meet because
/// `ActiveRecord::Base` includes both. Lexical resolution cannot see that, but
/// the ancestor index can.
///
/// Confidence is the share of mixing-in classes that agree on one definition —
/// a count, as ever. One includer that defines the name is certain *within the
/// index*; three includers of which one defines it is `1/3`, and says so.
fn via_includers(tree: &Tree, call: &Call, receiver: &Receiver) -> Option<MethodAnswer> {
    let includers = tree.includers_of(&receiver.fqn);
    if includers.is_empty() {
        return None;
    }
    let mut found: Vec<&crate::tree::MethodDef> = Vec::new();
    for class in &includers {
        if let Some(method) = tree.lookup(class, call.singleton, &call.name) {
            found.push(method);
        }
    }
    let winner = found.first()?;
    let agreeing = found
        .iter()
        .filter(|method| {
            method.site.line == winner.site.line && method.site.path == winner.site.path
        })
        .count();
    Some(MethodAnswer {
        status: Status::Resolved,
        confidence: share(agreeing, includers.len()),
        resolved_via: Some("includer".to_string()),
        receiver: call.recv.as_str(),
        receiver_kind: Some("module".to_string()),
        receiver_type: Some(receiver.fqn.clone()),
        owner: Some(winner.owner.clone()),
        sites: vec![winner.site.clone()],
        // The fraction counts classes that mix the module in, not assignments —
        // `resolved_via` is what says which.
        agreement: Some(format!("{agreeing}/{} includers", includers.len())),
        unresolved_ancestors: Vec::new(),
        candidates: Vec::new(),
        reason: None,
    })
}

fn agreement(receiver: &Receiver) -> Option<String> {
    (receiver.total > 1 || receiver.agreeing != receiver.total)
        .then(|| format!("{}/{}", receiver.agreeing, receiver.total))
}

/// Climb the ladder until a rung names a type.
pub(super) fn receiver_of(tree: &Tree, facts: &Facts, call: &Call) -> Option<Receiver> {
    match call.recv {
        // The enclosing scope is the receiver by language rule. No inference
        // happens, which is why this rung is both the largest and the cheapest.
        RecvShape::Implicit | RecvShape::SelfRecv => {
            let fqn = tree.scope_fqn(&call.nesting)?;
            tree.is_known(&fqn).then_some(Receiver {
                fqn,
                singleton: call.singleton,
                via: "self",
                agreeing: 1,
                total: 1,
                ambiguous: false,
            })
        }
        RecvShape::Const => {
            let name = call.recv_text.as_ref()?;
            let fqn = tree.resolve(name, &call.nesting).fqn?;
            Some(Receiver {
                fqn,
                // `Foo.bar` runs a class method.
                singleton: true,
                via: "const",
                agreeing: 1,
                total: 1,
                ambiguous: false,
            })
        }
        // An assignment first, because it is the more specific evidence; a
        // parameter's declared type is the fallback when there is none.
        RecvShape::Local | RecvShape::Ivar => from_assignments(tree, facts, call)
            .or_else(|| from_sig_params(tree, facts, call))
            // Last, because it is the only rung resting on a naming habit
            // rather than on something the code states.
            .or_else(|| from_receiver_name(tree, call)),
        RecvShape::Other => None,
    }
}

/// What the receiver is *called*, when nothing else typed it.
///
/// `@widget.supplier_region` — no assignment to chase, no signature, and the
/// answer sitting in the name. Session 14 shipped this as a ranking signal;
/// this promotes it to a typing rung, but only with all three corroborations,
/// because a naming habit is weaker evidence than anything else on the ladder:
///
/// 1. the name resolves to a constant the tree actually knows;
/// 2. that constant defines or inherits the method being called;
/// 3. **nothing in the enclosing scope's own chain defines it too** — if it
///    does, there is a competing reading and the name is not decisive.
///
/// A visible-but-untypeable assignment is deliberately *not* disqualifying:
/// this rung is only reached because assignment typing already failed, and
/// `@widget = widget` — a constructor parameter that shares the name — is the
/// exact case it exists for. Guarding on it removed the whole population.
///
/// Confidence is graded by the ambiguity it had to resolve, never flat: one
/// hypothesis against "something else", plus one for every other class that
/// defines the same method. A unique match is 0.5; four competitors is 0.17.
fn from_receiver_name(tree: &Tree, call: &Call) -> Option<Receiver> {
    let named = receiver_names_a_class(call.recv_text.as_deref()?)?;
    let fqn = tree.resolve(&named, &call.nesting).fqn?;
    // (2) it has to actually answer the call.
    tree.lookup(&fqn, false, &call.name)?;
    // (3) a competing reading in the enclosing scope disqualifies the guess.
    if let Some(scope) = tree.scope_fqn(&call.nesting)
        && scope != fqn
        && tree.lookup(&scope, false, &call.name).is_some()
    {
        return None;
    }
    let others = tree
        .named(&call.name)
        .into_iter()
        .filter(|method| method.owner != fqn)
        .map(|method| method.owner.clone())
        .collect::<std::collections::HashSet<_>>()
        .len();
    Some(Receiver {
        fqn,
        singleton: false,
        via: "receiver_name",
        agreeing: 1,
        // The name is one hypothesis; "something else entirely" is always the
        // other; every other class defining this name is one more.
        total: 2 + others,
        // A unique name match is the whole story. Competitors mean the name
        // picked among equals, which is what `ambiguous` is for — no
        // threshold, just whether anything else could have been the answer.
        ambiguous: others > 0,
    })
}

/// A method parameter's type, from the `params(...)` half of its `sig`.
///
/// Measured on graph_weaver: half of all untyped local receivers are
/// parameters. They have no assignment to chase, so every rung that looks for
/// one misses them — and a signature has already said what they are.
fn from_sig_params(tree: &Tree, facts: &Facts, call: &Call) -> Option<Receiver> {
    let target = call.recv_text.as_ref()?;
    let enclosing = enclosing_method(facts, call.pos.line)?;
    let class = enclosing
        .sig_params
        .iter()
        .find(|(name, _)| name == target)
        .map(|(_, class)| class)?;
    Some(Receiver {
        fqn: tree.resolve(class, &call.nesting).fqn?,
        singleton: false,
        via: "sig:param",
        agreeing: 1,
        total: 1,
        ambiguous: false,
    })
}

/// The innermost method definition containing a line.
///
/// Cheap because `--def` has already reparsed the file: the enclosing method of
/// a call is always in it, which is why parameter types never needed storing.
fn enclosing_method(facts: &Facts, line: u32) -> Option<&crate::core::Def> {
    facts
        .defs
        .iter()
        .filter(|def| {
            def.kind == crate::core::Kind::Method && def.pos.line <= line && line <= def.end_line
        })
        .min_by_key(|def| def.end_line - def.pos.line)
}

/// What a local or instance variable holds, judged from every assignment to it
/// in this file.
///
/// The scan is file-wide rather than flow-sensitive. That over-counts — an
/// assignment in an unrelated method still votes — but it errs toward *lower*
/// confidence, which is the safe direction for a number a caller may trust.
fn from_assignments(tree: &Tree, facts: &Facts, call: &Call) -> Option<Receiver> {
    let target = call.recv_text.as_ref()?;
    let scope = call.nesting.first();
    let relevant: Vec<&Assign> = facts
        .assigns
        .iter()
        .filter(|a| &a.target == target && a.nesting.first() == scope)
        .collect();
    if relevant.is_empty() {
        return None;
    }

    let mut votes: Vec<(String, bool, &'static str)> = Vec::new();
    for assign in &relevant {
        if let Some(vote) = type_of(tree, facts, &assign.value, &assign.nesting, 0, 0) {
            votes.push(vote);
        }
    }
    let (fqn, singleton, via) = votes.first().cloned()?;
    let agreeing = votes.iter().filter(|(f, _, _)| *f == fqn).count();
    Some(Receiver {
        fqn,
        singleton,
        via,
        agreeing,
        total: relevant.len(),
        ambiguous: false,
    })
}

/// The class a value expression produces, if syntax or a `sig` names one.
fn type_of(
    tree: &Tree,
    facts: &Facts,
    value: &ValueShape,
    nesting: &[String],
    depth: usize,
    steps: usize,
) -> Option<(String, bool, &'static str)> {
    // `x = y; y = x` is legal Ruby and would otherwise spin.
    if depth > 4 {
        return None;
    }
    match value {
        ValueShape::New(name) => Some((tree.resolve(name, nesting).fqn?, false, "local:new")),
        // `x = Foo` holds the class itself, so `x.bar` is a class method.
        ValueShape::Const(name) => Some((tree.resolve(name, nesting).fqn?, true, "local:const")),
        ValueShape::Same(other) => {
            let next = facts.assigns.iter().find(|a| &a.target == other)?;
            type_of(tree, facts, &next.value, &next.nesting, depth + 1, steps)
        }
        // Core knows what an Array is now, so `out = []` types `out`.
        ValueShape::Literal(class) => Some((tree.resolve(class, &[]).fqn?, false, "literal")),
        // One step, and only one: type the receiver from its own assignment,
        // then read the `sig` of the method called on it. Chaining further is
        // what rwr measured drowning.
        ValueShape::LocalCall { recv, name } => {
            if steps > 0 {
                return None;
            }
            let assign = facts.assigns.iter().find(|a| &a.target == recv)?;
            let (owner, singleton, _) = type_of(
                tree,
                facts,
                &assign.value,
                &assign.nesting,
                depth + 1,
                steps + 1,
            )?;
            let returns = tree.lookup(&owner, singleton, name)?.sig_returns.clone()?;
            Some((tree.resolve(&returns, nesting).fqn?, false, "sig:step"))
        }
        // A `sig` names a usable class for 64 % of signatures against 3.9 %
        // from syntax alone (PLAN §2) — the highest-yield rung on the ladder.
        ValueShape::SelfCall(name) => {
            let scope = tree.scope_fqn(nesting)?;
            let returns = tree.lookup(&scope, false, name)?.sig_returns.clone()?;
            Some((tree.resolve(&returns, nesting).fqn?, false, "sig"))
        }
        ValueShape::ConstCall { recv, name } => {
            let owner = tree.resolve(recv, nesting).fqn?;
            // A declared return type is better evidence than a convention, so
            // it is tried first.
            if let Some(returns) = tree
                .lookup(&owner, true, name)
                .and_then(|m| m.sig_returns.clone())
            {
                return Some((tree.resolve(&returns, nesting).fqn?, false, "sig"));
            }
            // ActiveRecord's finders return an instance of the class they are
            // called on. This is a convention, not a signature — `find` given
            // an array returns an array — so it is the last rung tried and it
            // says `finder`, not `sig`.
            is_finder(name).then_some((owner, false, "finder"))
        }
        ValueShape::Other => None,
    }
}

/// A count over a count, rounded to the precision two counts actually carry.
///
/// `1/31` is 0.032, not 0.03225806451612903: printing the tail claims evidence
/// that is not there, and it claims it hardest in JSON, where a reader cannot
/// see that the human output was more careful.
fn share(agreeing: usize, total: usize) -> f64 {
    if total == 0 {
        return 0.0;
    }
    let raw = agreeing as f64 / total as f64;
    (raw * 100.0).round() / 100.0
}

/// The ActiveRecord class methods that answer with one instance of the class.
///
/// Deliberately not `where`, `all` or `order`, which answer with a relation,
/// and not `find_each`, which yields. Measured across rails, discourse and
/// mastodon: 4,333 assignments of this shape would newly type **12,005 call
/// sites** — about half the reach of the `.new` rung already shipped, which is
/// what earned this one.
fn is_finder(name: &str) -> bool {
    matches!(
        name,
        "find"
            | "find_by"
            | "find_by!"
            | "first"
            | "first!"
            | "last"
            | "last!"
            | "create"
            | "create!"
            | "find_or_create_by"
            | "find_or_create_by!"
            | "find_or_initialize_by"
    )
}

/// The class a receiver expression is *named* after, if it looks like one.
///
/// `@widget` → `Widget`, `order_item` → `OrderItem`. Sigils are stripped;
/// anything that is not a plain snake_case identifier answers `None`, because
/// this is meant to be a strong hint or nothing at all. Never a resolution —
/// a name is a convention, so it ranks candidates and never promotes one.
fn receiver_names_a_class(text: &str) -> Option<String> {
    let bare = text.trim_start_matches(['@', '$']);
    if bare.is_empty() || !bare.starts_with(|c: char| c.is_ascii_lowercase()) {
        return None;
    }
    if !bare.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some(crate::extract::camelize(bare))
}

fn last_segment(fqn: &str) -> &str {
    fqn.rsplit("::").next().unwrap_or(fqn)
}

/// The other definitions the winner beat, for an answer that only just won.
///
/// Ordered by the same rule the residue ranker uses for its last tier — this
/// checkout's own code before a dependency's — so the list reads consistently
/// whichever surface produced it.
fn competitors(tree: &Tree, name: &str, winner: &str) -> Vec<Candidate> {
    let mut ranked: Vec<(bool, Candidate)> = tree
        .named(name)
        .into_iter()
        .filter(|method| method.owner != winner)
        .map(|method| {
            (
                !tree.in_checkout(&method.site.path),
                Candidate {
                    owner: method.owner.clone(),
                    singleton: method.singleton,
                    why: "defines the same name; the receiver's name chose between them",
                    site: method.site.clone(),
                },
            )
        })
        .collect();
    ranked.sort_by_key(|(from_gem, _)| *from_gem);
    ranked
        .into_iter()
        .take(MAX_CANDIDATES)
        .map(|(_, candidate)| candidate)
        .collect()
}

/// An honest no, with the candidates ordered by evidence a reader can check.
fn residue(
    tree: &Tree,
    call: &Call,
    path: &str,
    receiver: Option<Receiver>,
    reason: &str,
) -> MethodAnswer {
    let here = call
        .nesting
        .first()
        .and_then(|_| tree.scope_fqn(&call.nesting));
    let ancestors: Vec<String> = here
        .as_ref()
        .map(|fqn| tree.ancestors(fqn).chain.clone())
        .unwrap_or_default();
    // Whichever type we did settle on — the receiver's, else the enclosing
    // scope's — say what we could not see of its ancestry.
    let truncated = receiver
        .as_ref()
        .map(|r| r.fqn.clone())
        .or_else(|| here.clone())
        .map(|fqn| tree.ancestors(&fqn).unresolved.clone())
        .unwrap_or_default();

    // What the receiver is *called*. `@widget` names `Widget` far more often
    // than not, and when the receiver's type cannot be inferred that name is
    // the strongest evidence left in the expression.
    let named_type = call.recv_text.as_deref().and_then(receiver_names_a_class);

    let mut ranked: Vec<(u8, bool, Candidate)> = tree
        .named(&call.name)
        .into_iter()
        .map(|method| {
            let fits = method.accepts(call.argc);
            let names_owner = named_type
                .as_deref()
                .is_some_and(|named| last_segment(&method.owner) == named);
            // Named tiers, not weights: every one of these is a fact a reader
            // can check, and none of them is a constant somebody invented.
            let (tier, why) = match (fits, &here) {
                (true, _) if ancestors.contains(&method.owner) => (
                    0,
                    "arity fits, and the enclosing class inherits from its owner",
                ),
                (true, _) if names_owner => {
                    (1, "arity fits, and the receiver is named after its owner")
                }
                (true, Some(scope)) if shares_namespace(scope, &method.owner) => (
                    2,
                    "arity fits, and its owner shares a namespace with the call",
                ),
                (true, _) if crate::core::paths::names_file(&method.site.path, path) => {
                    (3, "arity fits, same file")
                }
                (true, _) => (4, "arity fits"),
                (false, _) => (5, "defined elsewhere; arity does not fit"),
            };
            (
                tier,
                // Within a tier, this checkout's own code before a dependency's.
                !tree.in_checkout(&method.site.path),
                Candidate {
                    owner: method.owner.clone(),
                    singleton: method.singleton,
                    why,
                    site: method.site.clone(),
                },
            )
        })
        .collect();
    ranked.sort_by_key(|(tier, from_gem, _)| (*tier, *from_gem));

    let total = ranked.len();
    let candidates: Vec<Candidate> = ranked
        .into_iter()
        .take(MAX_CANDIDATES)
        .map(|(_, _, c)| c)
        .collect();
    let reason = if total > candidates.len() {
        format!(
            "{reason}; showing {} of {total} definitions",
            candidates.len()
        )
    } else {
        reason.to_string()
    };

    MethodAnswer {
        status: Status::Residue,
        confidence: 0.0,
        resolved_via: None,
        receiver: call.recv.as_str(),
        receiver_kind: receiver
            .as_ref()
            .and_then(|r| tree.kind_of(&r.fqn))
            .map(str::to_string),
        receiver_type: receiver.map(|r| r.fqn),
        owner: None,
        sites: Vec::new(),
        agreement: None,
        unresolved_ancestors: truncated,
        candidates,
        reason: Some(reason),
    }
}

/// Do two names share an outer namespace? `A::B::C` and `A::B::D` do.
fn shares_namespace(one: &str, other: &str) -> bool {
    match (one.rsplit_once("::"), other.rsplit_once("::")) {
        (Some((a, _)), Some((b, _))) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Resolve the first call to `name` in this source.
    pub(super) fn answer(source: &str, name: &str) -> MethodAnswer {
        let tree = crate::tree::for_test(&[("a.rb", source)]);
        let facts = crate::extract::extract(source.as_bytes());
        let call = facts
            .calls
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("no call to {name}"))
            .clone();
        method_at(&tree, &facts, &call, "a.rb")
    }

    fn owner(source: &str, name: &str) -> Option<String> {
        answer(source, name).owner
    }

    #[test]
    fn an_implicit_receiver_needs_no_inference_at_all() {
        let source = "class W\n  def helper\n  end\n  def go\n    helper\n  end\nend\n";
        let found = answer(source, "helper");
        assert_eq!(found.status, Status::Resolved);
        assert_eq!(found.owner.as_deref(), Some("W"));
        assert_eq!(found.resolved_via.as_deref(), Some("self"));
        assert_eq!(
            found.confidence, 1.0,
            "the enclosing class is the receiver by language rule"
        );
    }

    #[test]
    fn an_implicit_receiver_inside_a_singleton_method_means_the_class() {
        // The same source line means two different lookups depending on which
        // kind of method encloses it.
        let source = "class W\n  def self.made\n  end\n  def made\n  end\n  \
                      def self.go\n    made\n  end\nend\n";
        let found = answer(source, "made");
        assert_eq!(found.receiver_type.as_deref(), Some("W"));
        assert_eq!(
            found.sites[0].line, 2,
            "inside `def self.go`, `made` is the class method on line 2"
        );
    }

    #[test]
    fn a_bare_call_in_a_class_body_dispatches_on_the_class() {
        // `self` in a class body is the class, so `setup` runs the singleton
        // method — even though a `def` written in the same place would not be
        // one. Conflating those two questions is what made every class-level
        // DSL call (`validates`, `prepend`, `class_attribute`) unresolvable.
        let source = "class W\n  def self.setup\n  end\n  setup\n  def setup\n  end\nend\n";
        let found = answer(source, "setup");
        assert_eq!(found.status, Status::Resolved);
        assert_eq!(
            found.sites[0].line, 2,
            "the class method on line 2, not the instance method on line 5"
        );
    }

    #[test]
    fn kernel_methods_resolve_from_an_ordinary_class() {
        // The largest single bucket in session 3's diagnosis: `puts` and
        // `raise` were "defined nowhere in the index" because core was not in
        // it. They reach an ordinary class through its implicit `< Object`.
        let source = "class W\n  def go\n    puts 1\n    raise \"x\"\n  end\nend\n";
        for name in ["puts", "raise"] {
            let found = answer(source, name);
            assert_eq!(found.status, Status::Resolved, "{name}");
            assert_eq!(found.owner.as_deref(), Some("Kernel"), "{name}");
            assert_eq!(found.sites[0].path, crate::tree::CORE_PATH);
        }
    }

    #[test]
    fn class_body_macros_resolve_to_module() {
        // The other half of that bucket: `prepend` and friends are Module
        // methods, reached because a class body dispatches on the class and a
        // class's singleton chain runs through Class and Module.
        let source = "module M\nend\nclass W\n  prepend M\nend\n";
        let found = answer(source, "prepend");
        assert_eq!(found.owner.as_deref(), Some("Module"));
    }

    #[test]
    fn new_on_a_constant_receiver_resolves_to_class() {
        let source = "class Box\nend\nclass W\n  def go\n    Box.new\n  end\nend\n";
        let found = answer(source, "new");
        assert_eq!(found.owner.as_deref(), Some("Class"));
        assert_eq!(found.resolved_via.as_deref(), Some("const"));
    }

    #[test]
    fn a_method_on_a_core_typed_local_resolves() {
        let source = "class W\n  def go\n    s = String.new\n    s.upcase\n  end\nend\n";
        assert_eq!(owner(source, "upcase").as_deref(), Some("String"));
    }

    #[test]
    fn a_top_level_call_has_no_class_to_dispatch_on() {
        // `self` at the top level is `main`, an ordinary Object instance —
        // which is not indexed, so this is residue rather than a wrong answer.
        let source = "def helper\nend\nhelper\n";
        assert_eq!(answer(source, "helper").status, Status::Residue);
    }

    #[test]
    fn an_explicit_self_resolves_the_same_way() {
        let source = "class W\n  def size=(v)\n  end\n  def go\n    self.size = 1\n  end\nend\n";
        assert_eq!(owner(source, "size=").as_deref(), Some("W"));
    }

    #[test]
    fn a_constant_receiver_looks_up_a_class_method() {
        let source = "class Reg\n  def self.lookup\n  end\n  def lookup\n  end\nend\n\
                      class W\n  def go\n    Reg.lookup\n  end\nend\n";
        let found = answer(source, "lookup");
        assert_eq!(found.resolved_via.as_deref(), Some("const"));
        assert_eq!(
            found.sites[0].line, 2,
            "the singleton one, not the instance one"
        );
    }

    #[test]
    fn a_local_assigned_from_new_carries_that_class() {
        let source = "class Box\n  def open\n  end\nend\n\
                      class W\n  def go\n    b = Box.new\n    b.open\n  end\nend\n";
        let found = answer(source, "open");
        assert_eq!(found.owner.as_deref(), Some("Box"));
        assert_eq!(found.resolved_via.as_deref(), Some("local:new"));
    }

    #[test]
    fn an_identity_method_does_not_lose_the_type() {
        let source = "class Box\n  def open\n  end\nend\n\
                      class W\n  def go\n    b = Box.new.freeze\n    b.open\n  end\nend\n";
        assert_eq!(owner(source, "open").as_deref(), Some("Box"));
    }

    #[test]
    fn a_local_holding_a_constant_gets_that_classs_class_methods() {
        let source = "class Box\n  def self.build\n  end\nend\n\
                      class W\n  def go\n    k = Box\n    k.build\n  end\nend\n";
        assert_eq!(owner(source, "build").as_deref(), Some("Box"));
    }

    #[test]
    fn an_instance_variable_is_typed_from_its_assignment() {
        let source = "class Box\n  def open\n  end\nend\n\
                      class W\n  def initialize\n    @box = Box.new\n  end\n  \
                      def go\n    @box.open\n  end\nend\n";
        assert_eq!(owner(source, "open").as_deref(), Some("Box"));
    }

    #[test]
    fn a_sorbet_signature_types_what_syntax_cannot() {
        let source = "class Box\n  def open\n  end\nend\n\
                      class W\n  sig { returns(Box) }\n  def fetch\n  end\n  \
                      def go\n    b = fetch\n    b.open\n  end\nend\n";
        let found = answer(source, "open");
        assert_eq!(found.owner.as_deref(), Some("Box"));
        assert_eq!(found.resolved_via.as_deref(), Some("sig"));
    }

    #[test]
    fn a_sig_types_a_method_parameter_that_has_no_assignment() {
        // Half of all untyped local receivers are parameters. They have no
        // assignment to chase, and the signature has already said what they
        // are.
        let source = "class Box\n  def open\n  end\nend\n\
                      class W\n  sig { params(box: Box).returns(Integer) }\n  \
                      def go(box)\n    box.open\n  end\nend\n";
        let found = answer(source, "open");
        assert_eq!(found.status, Status::Resolved);
        assert_eq!(found.owner.as_deref(), Some("Box"));
        assert_eq!(found.resolved_via.as_deref(), Some("sig:param"));
    }

    #[test]
    fn an_assignment_outranks_a_parameter_of_the_same_name() {
        let source = "class Box\n  def open\n  end\nend\n\
                      class Other\n  def open\n  end\nend\n\
                      class W\n  sig { params(box: Other).returns(Integer) }\n  \
                      def go(box)\n    box = Box.new\n    box.open\n  end\nend\n";
        let found = answer(source, "open");
        assert_eq!(
            found.owner.as_deref(),
            Some("Box"),
            "the assignment is the more specific evidence"
        );
    }

    #[test]
    fn a_tapioca_generated_method_answers_with_the_model_not_the_rbi() {
        // Sorbet's own go-to-definition lands in the generated file. Landing at
        // the model is the point of doing this at all.
        let tree = crate::tree::for_test(&[
            ("app/models/widget.rb", "class Widget < Base\nend\n"),
            (
                "sorbet/rbi/dsl/widget.rbi",
                "class Widget\n  sig { returns(String) }\n  def name; end\nend\n",
            ),
            (
                "app/jobs/job.rb",
                "class Job\n  def go\n    w = Widget.new\n    w.name\n  end\nend\n",
            ),
        ]);
        let source = "class Job\n  def go\n    w = Widget.new\n    w.name\n  end\nend\n";
        let facts = crate::extract::extract(source.as_bytes());
        let call = facts
            .calls
            .iter()
            .find(|c| c.name == "name")
            .unwrap()
            .clone();
        let found = method_at(&tree, &facts, &call, "app/jobs/job.rb");

        assert_eq!(found.status, Status::Resolved);
        assert_eq!(found.owner.as_deref(), Some("Widget"));
        assert_eq!(found.resolved_via.as_deref(), Some("rbi_dsl"));
        assert_eq!(
            found.sites[0].path, "app/models/widget.rb",
            "the model declares it, even though only the .rbi describes it"
        );
    }

    #[test]
    fn a_delegated_method_is_a_method() {
        // The exact mechanism behind `Topic.where`: ActiveRecord::Querying
        // says `delegate :where, to: :all` and Base extends it.
        let source = "module Querying\n  delegate :where, :find_by, to: :all\nend\n\
                      class Base\n  extend Querying\nend\n\
                      class Job\n  def go\n    Base.where(1)\n  end\nend\n";
        let found = answer(source, "where");
        assert_eq!(found.status, Status::Resolved);
        assert_eq!(found.owner.as_deref(), Some("Querying"));
        assert_eq!(found.resolved_via.as_deref(), Some("const"));
    }

    #[test]
    fn a_delegate_splatting_a_constant_array_still_names_literal_methods() {
        // Rails' highest-yield delegation is
        // `delegate(*QUERYING_METHODS, to: :all)` — ~60 of the most called
        // class methods in any app, none written as a literal argument.
        let source = "module Querying\n  METHODS = [:where, :find_by]\n  \
                      delegate(*METHODS, to: :all)\nend\n\
                      class Base\n  extend Querying\nend\n\
                      class Job\n  def go\n    Base.where(1)\n  end\nend\n";
        let found = answer(source, "where");
        assert_eq!(found.status, Status::Resolved);
        assert_eq!(found.owner.as_deref(), Some("Querying"));
    }

    #[test]
    fn a_splat_of_something_unknown_still_refuses() {
        let source = "class W\n  delegate(*computed, to: :other)\n  \
                      def go\n    thing\n  end\nend\n";
        assert_eq!(answer(source, "thing").status, Status::Residue);
    }

    #[test]
    fn a_delegate_without_a_target_is_left_as_an_ordinary_call() {
        // `delegate` with no `to:` is not a delegation, and a `prefix:` renames
        // everything — both refuse rather than invent a method.
        for source in [
            "class W\n  delegate :thing\n  def go\n    thing\n  end\nend\n",
            "class W\n  delegate :thing, to: :other, prefix: true\n  \
             def go\n    thing\n  end\nend\n",
        ] {
            assert_eq!(
                answer(source, "thing").status,
                Status::Residue,
                "no method was invented: {source}"
            );
        }
    }

    #[test]
    fn a_belongs_to_gives_its_reader_a_type() {
        let source = "class User\n  def name\n  end\nend\n\
                      class Post\n  belongs_to :user\n  \
                      def go\n    u = user\n    u.name\n  end\nend\n";
        let found = answer(source, "name");
        assert_eq!(
            found.owner.as_deref(),
            Some("User"),
            "the association names the class, so the reader is a typed receiver"
        );
        assert_eq!(found.resolved_via.as_deref(), Some("sig"));
    }

    #[test]
    fn class_name_overrides_what_the_association_would_be_called() {
        let source = "class Person\n  def name\n  end\nend\n\
                      class Post\n  belongs_to :author, class_name: \"Person\"\n  \
                      def go\n    a = author\n    a.name\n  end\nend\n";
        assert_eq!(owner(source, "name").as_deref(), Some("Person"));
    }

    #[test]
    fn schema_columns_become_typed_attributes_on_the_model() {
        // ruby-lsp-rails' capability with no running app. The point is not
        // that `post.body` exists but that it is a String.
        let schema = "create_table \"posts\" do |t|\n  t.string \"title\"\n  \
                      t.text \"body\"\n  t.integer \"views\"\n  t.timestamps\nend\n";
        // Through a local: `p.body.upcase` is a chained receiver, which
        // DEC-020 deliberately does not attack.
        let user = "class Post\nend\n\
                    class Job\n  def go\n    p = Post.new\n    b = p.body\n    \
                    b.upcase\n  end\nend\n";
        let tree = crate::tree::for_test(&[("db/schema.rb", schema), ("app.rb", user)]);
        let facts = crate::extract::extract(user.as_bytes());
        let call = facts
            .calls
            .iter()
            .find(|c| c.name == "upcase")
            .unwrap()
            .clone();
        let found = method_at(&tree, &facts, &call, "app.rb");
        assert_eq!(
            found.owner.as_deref(),
            Some("String"),
            "the column's SQL type makes the attribute a typed receiver"
        );
        assert!(tree.lookup("Post", false, "title=").is_some());
        assert!(tree.lookup("Post", false, "views?").is_some());
        assert!(
            tree.lookup("Post", false, "created_at").is_some(),
            "t.timestamps is two columns spelled as one call"
        );
        assert!(
            tree.lookup("Post", false, "body_changed?").is_none(),
            "the dirty-tracking family is deliberately out"
        );
    }

    #[test]
    fn a_model_overriding_its_table_name_still_gets_that_tables_columns() {
        // The schema declares `legacy_posts`; the model is called Post. Nothing
        // links them except a literal in the class body, and the two live in
        // different blobs — so the join is a tree question.
        let schema = "create_table \"legacy_posts\" do |t|\n  t.string \"headline\"\nend\n";
        let model = "class Post\n  self.table_name = \"legacy_posts\"\nend\n";
        let tree = crate::tree::for_test(&[("db/schema.rb", schema), ("post.rb", model)]);
        assert!(
            tree.lookup("Post", false, "headline").is_some(),
            "the column reaches the model that actually uses the table"
        );
        assert!(tree.lookup("Post", false, "headline=").is_some());
    }

    #[test]
    fn an_enum_defines_a_predicate_a_bang_and_a_scope_per_member() {
        for source in [
            // Rails 6 spelling and Rails 7 spelling.
            "class Post\n  enum status: { draft: 0, live: 1 }\nend\n",
            "class Post\n  enum :status, { draft: 0, live: 1 }\nend\n",
        ] {
            let tree = crate::tree::for_test(&[("a.rb", source)]);
            assert!(tree.lookup("Post", false, "draft?").is_some(), "{source}");
            assert!(tree.lookup("Post", false, "live!").is_some(), "{source}");
            assert!(
                tree.lookup("Post", true, "draft").is_some(),
                "the scope is a class method: {source}"
            );
        }
    }

    #[test]
    fn an_enum_with_a_prefix_refuses_rather_than_guess_names() {
        let tree = crate::tree::for_test(&[(
            "a.rb",
            "class Post\n  enum status: { draft: 0 }, _prefix: true\nend\n",
        )]);
        let _ = tree.lookup("Post", false, "draft?");
        let tree = crate::tree::for_test(&[(
            "a.rb",
            "class Post\n  enum status: { draft: 0 }, prefix: true\nend\n",
        )]);
        assert!(tree.lookup("Post", false, "draft?").is_none());
    }

    #[test]
    fn a_scope_is_callable_on_the_class() {
        let source = "class Widget\n  scope :active, -> { where(1) }\nend\n\
                      class Job\n  def go\n    Widget.active\n  end\nend\n";
        assert_eq!(owner(source, "active").as_deref(), Some("Widget"));
    }

    #[test]
    fn a_has_many_brings_the_ids_accessor() {
        let source = "class Post\n  has_many :comments\nend\n\
                      class Job\n  def go\n    p = Post.new\n    p.comment_ids\n  end\nend\n";
        assert_eq!(owner(source, "comment_ids").as_deref(), Some("Post"));
    }

    #[test]
    fn a_literal_is_typed_now_that_core_knows_what_it_is() {
        let source = "class W\n  def go\n    out = []\n    out.push 1\n  end\nend\n";
        let found = answer(source, "push");
        assert_eq!(found.owner.as_deref(), Some("Array"));
        assert_eq!(found.resolved_via.as_deref(), Some("literal"));
    }

    #[test]
    fn a_call_on_a_typed_local_is_followed_exactly_one_step() {
        let source = "class Leaf\n  def touch\n  end\nend\n\
                      class Box\n  sig { returns(Leaf) }\n  def leaf\n  end\nend\n\
                      class W\n  def go\n    b = Box.new\n    l = b.leaf\n    \
                      l.touch\n  end\nend\n";
        let found = answer(source, "touch");
        assert_eq!(found.owner.as_deref(), Some("Leaf"));
        assert_eq!(found.resolved_via.as_deref(), Some("sig:step"));
    }

    #[test]
    fn a_second_step_is_refused_rather_than_chased() {
        // rwr measured 70% of returns ending in another call; chaining drowns.
        let source = "class Deep\n  def touch\n  end\nend\n\
                      class Leaf\n  sig { returns(Deep) }\n  def deep\n  end\nend\n\
                      class Box\n  sig { returns(Leaf) }\n  def leaf\n  end\nend\n\
                      class W\n  def go\n    b = Box.new\n    l = b.leaf\n    \
                      d = l.deep\n    d.touch\n  end\nend\n";
        assert_eq!(answer(source, "touch").status, Status::Residue);
    }

    #[test]
    fn assignments_that_disagree_lower_the_confidence_they_produced() {
        let source = "class A\n  def go\n  end\nend\nclass B\n  def go\n  end\nend\n\
                      class W\n  def one\n    x = A.new\n    x.go\n  end\n  \
                      def two\n    x = B.new\n  end\nend\n";
        let found = answer(source, "go");
        assert_eq!(found.status, Status::Resolved);
        assert_eq!(
            found.confidence, 0.5,
            "two assignments were seen and only one agreed — a count, not a guess"
        );
        assert_eq!(found.agreement.as_deref(), Some("1/2"));
    }

    #[test]
    fn a_call_inside_a_module_is_resolved_through_the_class_that_mixes_it_in() {
        // The Rails concern shape: two modules that know nothing of each other,
        // meeting only in the class that includes both.
        let source = "module Persistence\n  def destroyed?\n  end\nend\n\
                      module Transactions\n  def rollback\n    destroyed?\n  end\nend\n\
                      class Base\n  include Persistence\n  include Transactions\nend\n";
        let found = answer(source, "destroyed?");
        assert_eq!(found.status, Status::Resolved);
        assert_eq!(found.resolved_via.as_deref(), Some("includer"));
        assert_eq!(found.owner.as_deref(), Some("Persistence"));
        assert_eq!(
            found.confidence, 1.0,
            "exactly one class mixes it in, so the receiver is determinate"
        );
        assert_eq!(found.agreement.as_deref(), Some("1/1 includers"));
    }

    #[test]
    fn includers_that_disagree_lower_the_confidence_and_disclose_the_count() {
        // Two classes mix the module in; only one of them has the method.
        let source = "module Helper\n  def run\n    missing_here\n  end\nend\n\
                      class A\n  include Helper\n  def missing_here\n  end\nend\n\
                      class B\n  include Helper\nend\n";
        let found = answer(source, "missing_here");
        assert_eq!(found.status, Status::Resolved);
        assert_eq!(found.owner.as_deref(), Some("A"));
        assert_eq!(found.confidence, 0.5);
        assert_eq!(found.agreement.as_deref(), Some("1/2 includers"));
    }

    #[test]
    fn a_module_nobody_mixes_in_says_that_rather_than_guessing() {
        let source = "module Lonely\n  def run\n    nowhere\n  end\nend\n";
        let found = answer(source, "nowhere");
        assert_eq!(found.status, Status::Residue);
        assert!(found.reason.unwrap().contains("mixes it in"));
    }

    #[test]
    fn the_includer_rung_reaches_through_a_module_that_includes_a_module() {
        let source = "module Deep\n  def deep\n  end\nend\n\
                      module Middle\n  def run\n    deep\n  end\nend\n\
                      class C\n  include Deep\n  include Middle\nend\n";
        assert_eq!(owner(source, "deep").as_deref(), Some("Deep"));
    }

    #[test]
    fn an_undetermined_receiver_returns_ordered_candidates_not_nothing() {
        let source = "class Near\n  def save\n  end\nend\n\
                      class Far\n  def save(a, b)\n  end\nend\n\
                      class W < Near\n  def go\n    thing.save\n  end\nend\n";
        let found = answer(source, "save");
        assert_eq!(found.status, Status::Residue);
        assert_eq!(found.confidence, 0.0);
        assert_eq!(found.receiver, "other", "the shape is the reason");
        assert_eq!(
            found.candidates[0].owner, "Near",
            "the enclosing class inherits from Near, so its save ranks first"
        );
        assert!(found.candidates[0].why.contains("inherits"));
        assert_eq!(
            found.candidates.last().unwrap().owner,
            "Far",
            "arity rules Far out, so it sinks rather than disappearing"
        );
    }

    #[test]
    fn a_known_receiver_with_no_such_method_says_so_differently() {
        let source =
            "class Box\nend\nclass W\n  def go\n    b = Box.new\n    b.missing\n  end\nend\n";
        let found = answer(source, "missing");
        assert_eq!(found.status, Status::Residue);
        assert_eq!(
            found.receiver_type.as_deref(),
            Some("Box"),
            "the type was settled; it is the method that is absent"
        );
        assert!(found.reason.unwrap().contains("method_missing"));
    }

    #[test]
    fn an_assignment_cycle_stops_instead_of_spinning() {
        let source = "class W\n  def go\n    x = y\n    y = x\n    x.anything\n  end\nend\n";
        assert_eq!(answer(source, "anything").status, Status::Residue);
    }
}

#[cfg(test)]
mod finder_rung_tests {
    use super::*;

    /// A local assigned from an ActiveRecord finder holds an instance of the
    /// class the finder was called on, so calls on it resolve.
    #[test]
    fn a_finder_types_the_local_it_is_assigned_to() {
        let answer = super::tests::answer(
            "class Widget\n  def ship\n  end\nend\n\
             class Job\n  def run\n    w = Widget.find(1)\n    w.ship\n  end\nend\n",
            "ship",
        );
        assert_eq!(answer.status, Status::Resolved);
        assert_eq!(answer.owner.as_deref(), Some("Widget"));
        assert_eq!(
            answer.resolved_via.as_deref(),
            Some("finder"),
            "and it names the rung, because a convention is weaker than a sig"
        );
    }

    /// `where` and `all` answer with a relation, not an instance. Typing them
    /// as the model would be confidently wrong on every chained call.
    #[test]
    fn a_relation_returning_class_method_is_not_a_finder() {
        for method in ["where", "all", "order", "find_each"] {
            assert!(!is_finder(method), "{method} does not answer with one row");
        }
        for method in ["find", "find_by", "first", "create!"] {
            assert!(is_finder(method), "{method} does");
        }
    }
}

#[cfg(test)]
mod receiver_name_ranking_tests {
    use super::*;

    #[test]
    fn reads_a_class_name_out_of_a_receiver_expression() {
        assert_eq!(receiver_names_a_class("@widget").as_deref(), Some("Widget"));
        assert_eq!(
            receiver_names_a_class("order_item").as_deref(),
            Some("OrderItem")
        );
        // Anything that is not a plain snake_case identifier is not a hint.
        // Guessing here would rank a wrong answer first, which is worse than
        // declining to rank at all.
        for text in ["Widget", "foo.bar", "widget(1)", "@", "widget[0]"] {
            assert_eq!(receiver_names_a_class(text), None, "{text}");
        }
    }

    /// The signal that motivated this, now a typing rung: an untypeable ivar
    /// whose *name* says what it holds, with nothing competing.
    #[test]
    fn a_receiver_named_after_its_class_is_typed_by_that_name() {
        let answer = super::tests::answer(
            "class Alpha\n  def ship\n  end\nend\n\
             class Widget\n  def ship\n  end\nend\n\
             class Zeta\n  def ship\n  end\nend\n\
             class Job\n  def run\n    @widget.ship\n  end\nend\n",
            "ship",
        );
        // Alpha and Zeta define `ship` too, so the name picked among equals:
        // an answer with competitors, which is what `ambiguous` is for.
        assert_eq!(answer.status, Status::Ambiguous);
        assert_eq!(answer.owner.as_deref(), Some("Widget"));
        assert_eq!(answer.resolved_via.as_deref(), Some("receiver_name"));
        // Graded by the ambiguity it resolved — one hypothesis against
        // "something else", plus Alpha and Zeta. Never the 1.0 of a rung that
        // read the answer out of the code.
        assert!(
            answer.confidence < 0.3,
            "a naming habit is weak evidence: {}",
            answer.confidence
        );
    }

    /// An ambiguous answer knows its competitors exist — that is what made it
    /// ambiguous — so it has to show them. Only residue used to carry a list.
    #[test]
    fn an_ambiguous_answer_lists_what_it_beat() {
        let answer = super::tests::answer(
            "class Alpha\n  def ship\n  end\nend\n\
             class Widget\n  def ship\n  end\nend\n\
             class Zeta\n  def ship\n  end\nend\n\
             class Job\n  def run\n    @widget.ship\n  end\nend\n",
            "ship",
        );
        assert_eq!(answer.status, Status::Ambiguous);
        let owners: Vec<&str> = answer.candidates.iter().map(|c| c.owner.as_str()).collect();
        assert_eq!(owners, ["Alpha", "Zeta"], "the winner is not its own rival");
        assert!(answer.candidates[0].why.contains("same name"));
    }

    /// With nothing else defining the name, the receiver's name is the whole
    /// story and the answer is not ambiguous at all.
    #[test]
    fn a_unique_name_match_resolves_rather_than_hedging() {
        let answer = super::tests::answer(
            "class Widget\n  def ship_it\n  end\nend\n\
             class Job\n  def run\n    @widget.ship_it\n  end\nend\n",
            "ship_it",
        );
        assert_eq!(answer.status, Status::Resolved);
        assert_eq!(answer.resolved_via.as_deref(), Some("receiver_name"));
        assert!(
            (answer.confidence - 0.5).abs() < 1e-9,
            "{}",
            answer.confidence
        );
    }

    /// A competing definition in the enclosing scope's own chain means the
    /// name is not decisive, so the rung declines and only ranking applies.
    #[test]
    fn a_competing_definition_in_scope_blocks_the_promotion() {
        let answer = super::tests::answer(
            "class Widget\n  def ship\n  end\nend\n\
             class Job\n  def ship\n  end\n  def run\n    @widget.ship\n  end\nend\n",
            "ship",
        );
        assert_eq!(
            answer.status,
            Status::Residue,
            "Job defines ship too, so the name settles nothing"
        );
        // And the enclosing class's own definition outranks the name match,
        // which is the right order: a definition in scope is stronger evidence
        // than a naming habit.
        let owners: Vec<&str> = answer.candidates.iter().map(|c| c.owner.as_str()).collect();
        assert_eq!(owners.first(), Some(&"Job"));
        assert!(owners.contains(&"Widget"), "still offered: {owners:?}");
    }

    /// An assignment the ladder could not type does **not** disqualify: this
    /// rung is only reached because that typing already failed, and
    /// `@widget = widget` — a constructor parameter sharing the name — is the
    /// population it exists to serve. An earlier version guarded on this and
    /// promoted almost nothing.
    #[test]
    fn an_untypeable_assignment_does_not_block_the_promotion() {
        let answer = super::tests::answer(
            "class Widget\n  def ship\n  end\nend\n\
             class Job\n  def initialize(widget)\n    @widget = widget\n  end\n\
             \x20 def run\n    @widget.ship\n  end\nend\n",
            "ship",
        );
        assert_eq!(answer.status, Status::Resolved);
        assert_eq!(answer.resolved_via.as_deref(), Some("receiver_name"));
    }
}
