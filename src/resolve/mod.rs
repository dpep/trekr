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

use crate::core::{Assign, Call, Def, Facts, Pos, RecvShape, RecvValue, ValueShape, rspec};
use crate::tree::{Kind, Site, Status, Tree};
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
    /// been. A language rule cannot be ambiguous about what the receiver is;
    /// a naming convention, or writes that disagree, can.
    pub(super) ambiguous: bool,
    /// The other types the writes gave it, when they disagreed.
    pub(super) rivals: Vec<(String, bool)>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Candidate {
    pub(crate) owner: String,
    pub(crate) singleton: bool,
    /// Why this candidate is ranked where it is — a named tier, not a weight.
    pub(crate) why: &'static str,
    /// Whether `site` is the body or the macro line that made the name.
    pub(crate) kind: Kind,
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
    /// Whether `sites` is the code that runs or the line that declared the
    /// name. Absent when there are no sites to describe. Not to be confused
    /// with `sites[].kind`, which is class/module/method/constant — this one
    /// is about the *location's* nature, that one about the symbol's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) kind: Option<Kind>,
    /// The macro that declared it: `belongs_to`, `enum`, `schema`, `delegate`.
    /// Present only for a declaration, and the reason a caller can act on one
    /// rather than merely being warned about it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) defined_via: Option<String>,
    /// `definition` on the wire, as in every answer that names where a thing
    /// is defined; always present, empty for a residue (DEC-080).
    #[serde(rename = "definition")]
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
    let answer = if call.recv == RecvShape::Super {
        super_at(tree, call, path)
    } else {
        call_at(tree, facts, call, path)
    };
    answer.published()
}

fn call_at(tree: &Tree, facts: &Facts, call: &Call, path: &str) -> MethodAnswer {
    if let Some(member) =
        group_member(facts, call).filter(|_| on_the_example(tree, facts, call, path))
    {
        return member_answer(tree, call, member, path);
    }
    let shape = call.recv.as_str();
    match receiver_of(tree, facts, call, path) {
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
                    let overrides = match receiver.via {
                        "self" => self_overrides(tree, &receiver, &call.name, &found),
                        _ => Vec::new(),
                    };
                    MethodAnswer {
                        status: if receiver.ambiguous || !overrides.is_empty() {
                            Status::Ambiguous
                        } else {
                            Status::Resolved
                        },
                        confidence: match overrides.len() {
                            0 => share(receiver.agreeing, receiver.total),
                            n => share(1, n + 1),
                        },
                        resolved_via: Some(if generated {
                            "rbi_dsl".to_string()
                        } else {
                            receiver.via.to_string()
                        }),
                        receiver: shape,
                        receiver_kind: tree.kind_of(&receiver.fqn).map(str::to_string),
                        receiver_type: Some(receiver.fqn.clone()),
                        owner: Some(found.owner.clone()),
                        kind: Some(found.kind()),
                        defined_via: found.declared_via(),
                        sites,
                        agreement: agreement(&receiver),
                        unresolved_ancestors: Vec::new(),
                        // An ambiguous answer is the one case where competitors
                        // are known to exist — that is what made it ambiguous —
                        // so listing them is not hedging, it is the disclosure.
                        // A resolved answer has none to list.
                        candidates: if !overrides.is_empty() {
                            overrides.into_iter().take(MAX_CANDIDATES).collect()
                        } else if !receiver.rivals.is_empty() {
                            rival_landings(tree, &receiver, &call.name)
                        } else if receiver.ambiguous {
                            competitors(tree, &call.name, &found.owner, receiver.via)
                        } else {
                            Vec::new()
                        },
                        reason: None,
                    }
                }
                // A name declared with conflicting superclasses, asked about
                // from a file equally near two of them (DEC-072).
                None if !tree.variants_of(&receiver.fqn).is_empty() => {
                    split_receiver(tree, call, path, receiver)
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
                // A spec whose bundle's rspec-core is not indexed: the
                // receiver is known, and nothing about it is.
                None if receiver.via == "example_group"
                    && tree.kind_of(&receiver.fqn).is_none() =>
                {
                    residue(
                        tree,
                        call,
                        path,
                        Some(receiver),
                        "the call runs on an RSpec example group, and rspec-core is \
                         not indexed",
                    )
                }
                // The type is settled and Ruby would still not find the method
                // in what is indexed. Say what was checked, never why: the
                // cause is exactly what was not seen.
                None => residue(
                    tree,
                    call,
                    path,
                    Some(receiver),
                    "the receiver's type is known, and nothing indexed in its \
                     ancestors defines this name",
                ),
            }
        }
        None if rspec::in_group(&call.nesting) && call.recv == RecvShape::Implicit => residue(
            tree,
            call,
            path,
            None,
            "the call is in a block handed to a method that may run it on another object",
        ),
        None => residue(
            tree,
            call,
            path,
            None,
            "the receiver's type is not determined by this file",
        ),
    }
}

/// Does a call in a spec still run on the example or group it is written in?
///
/// A block keeps its caller's `self` unless the method it is handed to
/// evaluates it somewhere else, and only a method whose behaviour is known
/// can be vouched for: RSpec's own (found on the example group), Ruby core's,
/// or one sent to a value — an iterator. A block handed to a helper, or to a
/// constant's method in the checkout or a gem, may be a DSL's body, so a
/// call in it is not answered as the example's (DEC-084). So is one handed to
/// the methods whose purpose is to change `self`.
fn on_the_example(tree: &Tree, facts: &Facts, call: &Call, path: &str) -> bool {
    let mut current = call;
    // Each step goes to an enclosing block, which is earlier in the file.
    for _ in 0..facts.calls.len() {
        let Some(at) = current.block_owner else {
            return true;
        };
        let Some(owner) = facts
            .calls
            .iter()
            .find(|c| c.pos == at && c.recv != RecvShape::Symbol)
        else {
            return false;
        };
        if !yields_to_its_caller(tree, facts, owner, path) {
            return false;
        }
        current = owner;
    }
    false
}

/// Ruby's own ways to run a block as another object: `instance_eval` and its
/// kin, and a class or module built with a body.
fn evaluates_its_block(call: &Call) -> bool {
    let constant = call
        .recv_text
        .as_deref()
        .map(|r| r.trim_start_matches("::"));
    matches!(
        call.name.as_str(),
        "instance_eval"
            | "instance_exec"
            | "class_eval"
            | "class_exec"
            | "module_eval"
            | "module_exec"
    ) || matches!(
        (constant, call.name.as_str()),
        (Some("Class" | "Module" | "Struct"), "new") | (Some("Data"), "define")
    )
}

/// Is the method this block is handed to one known to call it as it stands?
fn yields_to_its_caller(tree: &Tree, facts: &Facts, owner: &Call, path: &str) -> bool {
    if evaluates_its_block(owner) {
        return false;
    }
    match owner.recv {
        RecvShape::Local | RecvShape::Ivar | RecvShape::Other => true,
        RecvShape::Implicit | RecvShape::SelfRecv => {
            rspec::in_group(&owner.nesting)
                && group_member(facts, owner).is_none()
                && tree
                    .lookup(rspec::EXAMPLE_GROUP, owner.singleton, &owner.name)
                    .is_some()
        }
        // Ruby's own: a class core declares, sending a method core defines or
        // no one indexed does (`Dir.mktmpdir` is the standard library's).
        RecvShape::Const => {
            let Some(class) = owner
                .recv_text
                .as_deref()
                .and_then(|name| tree.resolve_at(name, &owner.nesting, path).fqn)
                .and_then(|fqn| tree.namespace_named(&fqn))
            else {
                return false;
            };
            let core = |site: &crate::tree::Site| crate::tree::is_core(&site.path);
            tree.sites(&class).iter().any(core)
                && tree
                    .lookup(&class, true, &owner.name)
                    .is_none_or(|method| core(&method.site))
        }
        RecvShape::Symbol | RecvShape::Super => false,
    }
}

/// A method an enclosing example group defines — a `let`, a `subject`, a
/// `def` in its body — which only this file can see (DEC-084).
///
/// The innermost group that defines the name wins, as the subclass does, and
/// within one group the last definition, as a redefined method does. A symbol
/// is the definition itself only where it is written: `let(:name)`.
fn group_member<'f>(facts: &'f Facts, call: &Call) -> Option<&'f Def> {
    let named = |def: &&Def| def.is_group_member() && def.name == call.name;
    if call.recv == RecvShape::Symbol {
        return facts
            .defs
            .iter()
            .filter(named)
            .find(|def| def.pos == call.pos);
    }
    if !matches!(call.recv, RecvShape::Implicit | RecvShape::SelfRecv)
        || !rspec::in_group(&call.nesting)
    {
        return None;
    }
    facts
        .defs
        .iter()
        .filter(named)
        .filter(|def| def.singleton == call.singleton && call.nesting.ends_with(&def.nesting))
        .max_by_key(|def| (def.nesting.len(), def.pos))
}

fn member_answer(tree: &Tree, call: &Call, member: &Def, path: &str) -> MethodAnswer {
    let kind = Kind::of(member.via.as_deref());
    MethodAnswer {
        status: Status::Resolved,
        confidence: 1.0,
        resolved_via: Some("example_group".to_string()),
        receiver: call.recv.as_str(),
        receiver_type: (call.recv != RecvShape::Symbol).then(|| rspec::class_name(&call.nesting)),
        receiver_kind: (call.recv != RecvShape::Symbol).then(|| "class".to_string()),
        owner: Some(rspec::class_name(&member.nesting)),
        kind: Some(kind),
        defined_via: (kind == Kind::Declaration)
            .then(|| member.via.clone())
            .flatten(),
        sites: vec![Site {
            path: tree.site_path(path),
            line: member.pos.line,
            col: member.pos.col,
            kind: "method".to_string(),
        }],
        agreement: None,
        unresolved_ancestors: Vec::new(),
        candidates: Vec::new(),
        reason: None,
    }
}

/// The methods a call on `self` runs instead of `found` when `self` is one of
/// the subclasses that override it — the template-method pattern, which
/// DEC-081 made a possible reference and this makes a named competitor of the
/// answer. `self` runs as any class that inherits the method's caller.
fn self_overrides(
    tree: &Tree,
    receiver: &Receiver,
    name: &str,
    found: &crate::tree::MethodDef,
) -> Vec<Candidate> {
    // Every definition of the name, not every subclass: the name's list is
    // short, and a class with thousands of descendants is not.
    tree.named(name)
        .into_iter()
        .filter(|method| {
            method.singleton == receiver.singleton
                && method.owner != found.owner
                && overrides_for_self(tree, receiver, method)
        })
        .map(|method| Candidate {
            owner: method.owner.clone(),
            singleton: method.singleton,
            why: "a subclass overrides it, and `self` may be one",
            kind: method.kind(),
            site: method.site.clone(),
        })
        .collect()
}

/// Does `self` reach `method` in some class it may be: a subclass that
/// defines it, or a class inheriting the receiver that mixes in the module
/// defining it ahead of the method found (`ActiveRecord::Callbacks`'
/// `create_or_update` in front of `Persistence`'s, on every model).
fn overrides_for_self(tree: &Tree, receiver: &Receiver, method: &crate::tree::MethodDef) -> bool {
    if tree.kind_of(&method.owner) != Some("module") {
        return tree.inherits(&method.owner, &receiver.fqn);
    }
    tree.mixers_of(&method.owner).iter().any(|class| {
        (class == &receiver.fqn || tree.inherits(class, &receiver.fqn))
            && tree
                .lookup(class, receiver.singleton, &method.name)
                .is_some_and(|landed| {
                    landed.site.path == method.site.path && landed.site.line == method.site.line
                })
    })
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
    let mut found: Vec<crate::tree::MethodDef> = Vec::new();
    for class in &includers {
        if let Some(method) = tree.lookup(class, call.singleton, &call.name) {
            found.push(method);
        }
    }
    let winner = found.first()?;
    let same_place = |method: &crate::tree::MethodDef| {
        method.site.line == winner.site.line && method.site.path == winner.site.path
    };
    let agreeing = found.iter().filter(|m| same_place(m)).count();
    // DEC-027: a pick among competitors is `ambiguous`, not `resolved`. The
    // includers disagreeing about where the name is defined *is* a competitor,
    // and this rung reported `resolved` at confidence 0.2 regardless — the one
    // regression the `class_methods` change produced, because widening a
    // module's includer set is exactly what it does.
    let beaten: Vec<&crate::tree::MethodDef> = found
        .iter()
        .filter(|m| !same_place(m))
        .take(MAX_CANDIDATES)
        .collect();
    Some(MethodAnswer {
        status: if beaten.is_empty() {
            Status::Resolved
        } else {
            Status::Ambiguous
        },
        confidence: share(agreeing, includers.len()),
        resolved_via: Some("includer".to_string()),
        receiver: call.recv.as_str(),
        receiver_kind: Some("module".to_string()),
        receiver_type: Some(receiver.fqn.clone()),
        owner: Some(winner.owner.clone()),
        kind: Some(winner.kind()),
        defined_via: winner.declared_via(),
        sites: vec![winner.site.clone()],
        // The fraction counts classes that mix the module in, not assignments —
        // `resolved_via` is what says which.
        agreement: Some(format!("{agreeing}/{} includers", includers.len())),
        unresolved_ancestors: Vec::new(),
        // What the pick beat — the definitions the other includers offered, not
        // every method in the tree that shares the name.
        candidates: beaten
            .into_iter()
            .map(|method| Candidate {
                owner: method.owner.clone(),
                singleton: method.singleton,
                why: "another class mixing this module in defines the same name",
                kind: method.kind(),
                site: method.site.clone(),
            })
            .collect(),
        reason: None,
    })
}

/// Where one `super` goes, for every class that can run the method it is in.
pub(super) struct SuperLandings {
    /// The method's own owner — what `super` starts looking *after*.
    pub(super) owner: String,
    /// Each class that can run the method, and what `super` finds from it:
    /// one entry for a class's own method, one per includer for a module's.
    pub(super) per_class: Vec<(String, Option<crate::tree::MethodDef>)>,
    /// The classes are a module's includers rather than the owner itself.
    pub(super) via_includers: bool,
}

/// Ruby's rule for `super`: the next definition of the same name after the
/// method's owner, in the ancestors of the object running it.
///
/// A class's method runs on instances of it and its subclasses, and a subclass
/// can only add ancestors *before* the class, so the class's own chain is the
/// answer for all of them. A module's method runs wherever it is mixed in, so
/// each includer is asked — the same move as the `includer` rung.
pub(super) fn super_landings(
    tree: &Tree,
    call: &Call,
    path: &str,
) -> Result<SuperLandings, &'static str> {
    let owner = tree
        .scope_fqn(&call.nesting)
        .filter(|owner| tree.is_known(owner))
        .map(|owner| tree.variant_at(&owner, path))
        .ok_or("the method `super` is in has no owner the index knows")?;
    let via_includers = tree.kind_of(&owner) == Some("module") && !call.singleton;
    let classes = if via_includers {
        let mixers = tree.mixers_of(&owner);
        if mixers.is_empty() {
            return Err("`super` is in a module, and no class the index knows mixes it in");
        }
        mixers
    } else {
        vec![owner.clone()]
    };
    let per_class: Vec<(String, Option<crate::tree::MethodDef>)> = classes
        .into_iter()
        .filter_map(|class| {
            let landing = tree.after_in_chain(&class, call.singleton, &owner, &call.name)?;
            Some((class, landing))
        })
        .collect();
    if per_class.is_empty() {
        return Err("the method's owner is not in the ancestor chain `super` would walk");
    }
    Ok(SuperLandings {
        owner,
        per_class,
        via_includers,
    })
}

/// Ancestors of the classes a `super` was asked from that the index could not
/// resolve — where an unseen definition could be hiding.
pub(super) fn unresolved_behind(tree: &Tree, landings: &SuperLandings) -> Vec<String> {
    let mut unseen: Vec<String> = Vec::new();
    for (class, _) in &landings.per_class {
        for name in &tree.ancestors(class).unresolved {
            if !unseen.contains(name) {
                unseen.push(name.clone());
            }
        }
    }
    unseen
}

/// `--def` on a `super`: the method it runs.
fn super_at(tree: &Tree, call: &Call, path: &str) -> MethodAnswer {
    let landings = match super_landings(tree, call, path) {
        Ok(landings) => landings,
        Err(reason) => return residue(tree, call, path, None, reason),
    };
    let unseen = unresolved_behind(tree, &landings);
    let found: Vec<&crate::tree::MethodDef> = landings
        .per_class
        .iter()
        .filter_map(|(_, landing)| landing.as_ref())
        .collect();
    let Some(winner) = found.first().copied() else {
        let reason = if unseen.is_empty() {
            "nothing after the method's owner in its indexed ancestors defines this name"
        } else {
            "nothing indexed after the method's owner defines this name, and some of \
             its ancestors are not indexed"
        };
        let mut answer = residue(tree, call, path, None, reason);
        answer.receiver_type = Some(landings.owner.clone());
        answer.unresolved_ancestors = unseen;
        return answer;
    };
    let same_place = |method: &crate::tree::MethodDef| {
        method.site.line == winner.site.line && method.site.path == winner.site.path
    };
    let agreeing = found.iter().filter(|m| same_place(m)).count();
    let total = landings.per_class.len();
    // DEC-027: classes that disagree about where `super` lands are competitors.
    let beaten: Vec<Candidate> = found
        .iter()
        .filter(|m| !same_place(m))
        .take(MAX_CANDIDATES)
        .map(|method| Candidate {
            owner: method.owner.clone(),
            singleton: method.singleton,
            why: "`super` lands here from another class that mixes the module in",
            kind: method.kind(),
            site: method.site.clone(),
        })
        .collect();
    MethodAnswer {
        status: if agreeing == total {
            Status::Resolved
        } else {
            Status::Ambiguous
        },
        confidence: share(agreeing, total),
        resolved_via: Some("super".to_string()),
        receiver: call.recv.as_str(),
        receiver_kind: tree.kind_of(&landings.owner).map(str::to_string),
        receiver_type: Some(landings.owner.clone()),
        owner: Some(winner.owner.clone()),
        kind: Some(winner.kind()),
        defined_via: winner.declared_via(),
        sites: vec![winner.site.clone()],
        agreement: landings
            .via_includers
            .then(|| format!("{agreeing}/{total} includers")),
        unresolved_ancestors: unseen,
        candidates: beaten,
        reason: None,
    }
}

fn agreement(receiver: &Receiver) -> Option<String> {
    (receiver.total > 1 || receiver.agreeing != receiver.total)
        .then(|| format!("{}/{}", receiver.agreeing, receiver.total))
}

/// A receiver's type without a method lookup.
pub(crate) struct ReceiverType {
    pub(crate) fqn: String,
    /// The class itself: a singleton lookup.
    pub(crate) singleton: bool,
    /// Another type was a known possibility, so what `fqn` has is not all
    /// the receiver might.
    pub(crate) ambiguous: bool,
}

/// The ladder's answer without a method lookup, for a caller that lists what
/// the receiver has rather than finding one method on it (LSP completion,
/// DEC-040).
pub(crate) fn receiver_type(
    tree: &Tree,
    facts: &Facts,
    call: &Call,
    path: &str,
) -> Option<ReceiverType> {
    receiver_of(tree, facts, call, path)
        .or_else(|| unanswered_guess(tree, facts, call, path))
        .map(|receiver| ReceiverType {
            fqn: receiver.fqn,
            singleton: receiver.singleton,
            ambiguous: receiver.ambiguous,
        })
}

/// Completion asks before the method's name is written, so a guess by
/// return type has no call to answer yet and is taken as it stands — still
/// `ambiguous` when a definition declared nothing, which the caller must say.
fn unanswered_guess(tree: &Tree, facts: &Facts, call: &Call, path: &str) -> Option<Receiver> {
    let RecvValue::Call(at) = call.recv_value.as_ref()? else {
        return None;
    };
    let previous = facts
        .calls
        .iter()
        .find(|c| c.pos == *at && c.recv != RecvShape::Symbol)?;
    if typed_at(tree, facts, previous, path, 1).is_some() {
        return None;
    }
    let guess = by_return_types(tree, previous)?;
    Some(Receiver {
        fqn: tree.variant_at(&guess.fqn, path),
        ..guess
    })
}

/// Climb the ladder until a rung names a type — and when that type is a name
/// two programs declare differently, the one this file belongs to (DEC-072).
pub(super) fn receiver_of(tree: &Tree, facts: &Facts, call: &Call, path: &str) -> Option<Receiver> {
    let mut receiver = typed(tree, facts, call, path)?;
    receiver.fqn = tree.variant_at(&receiver.fqn, path);
    for (rival, _) in &mut receiver.rivals {
        *rival = tree.variant_at(rival, path);
    }
    Some(receiver)
}

/// The ladder itself.
///
/// `path` is the call's file: a constant written in a class declared twice
/// is looked up through the declaration nearest it (DEC-072).
fn typed(tree: &Tree, facts: &Facts, call: &Call, path: &str) -> Option<Receiver> {
    typed_at(tree, facts, call, path, 0)
}

/// How many calls back a chain is followed. Each step needs a declared
/// return type to continue, so the bound is a guard, not a tuning knob.
const MAX_CHAIN: usize = 4;

/// The ladder, `depth` calls into a chain.
fn typed_at(tree: &Tree, facts: &Facts, call: &Call, path: &str, depth: usize) -> Option<Receiver> {
    match call.recv {
        // The enclosing scope is the receiver by language rule. No inference
        // happens, which is why this rung is both the largest and the cheapest.
        RecvShape::Implicit | RecvShape::SelfRecv => {
            // A block RSpec runs: a group's body is a subclass of
            // ExampleGroup, and an example's an instance of one (DEC-084).
            if rspec::in_group(&call.nesting) {
                if !on_the_example(tree, facts, call, path) {
                    return None;
                }
                return Some(Receiver {
                    fqn: rspec::EXAMPLE_GROUP.to_string(),
                    singleton: call.singleton,
                    via: "example_group",
                    agreeing: 1,
                    total: 1,
                    ambiguous: false,
                    rivals: Vec::new(),
                });
            }
            let fqn = tree.scope_fqn(&call.nesting)?;
            tree.is_known(&fqn).then_some(Receiver {
                fqn,
                singleton: call.singleton,
                via: "self",
                agreeing: 1,
                total: 1,
                ambiguous: false,
                rivals: Vec::new(),
            })
        }
        RecvShape::Const => {
            let name = call.recv_text.as_ref()?;
            let written = tree.resolve_at(name, &call.nesting, path).fqn?;
            // `NAMES.join` calls the value NAMES holds, whose class was not
            // recorded; `Short.go` calls the module `Short = Router` names.
            let fqn = tree.namespace_named(&written)?;
            Some(Receiver {
                fqn,
                // `Foo.bar` runs a class method.
                singleton: true,
                via: "const",
                agreeing: 1,
                total: 1,
                ambiguous: false,
                rivals: Vec::new(),
            })
        }
        // An assignment first, because it is the more specific evidence; a
        // parameter's declared type is the fallback when there is none.
        RecvShape::Local | RecvShape::Ivar => from_assignments(tree, facts, call)
            .or_else(|| from_sig_params(tree, facts, call, path))
            // Last, because it is the only rung resting on a naming habit
            // rather than on something the code states.
            .or_else(|| from_receiver_name(tree, call, path)),
        RecvShape::Other => chained(tree, facts, call, path, depth),
        // A symbol names the method, never the receiver, so there is nothing
        // here to type. `super` is typed by its own rule, `super_landings`.
        RecvShape::Symbol | RecvShape::Super => None,
    }
}

/// A receiver that is a value: a literal is its class, and a call returns
/// what its `sig` says — `x.gsub(a, b).downcase` is a String (DEC-077).
fn chained(tree: &Tree, facts: &Facts, call: &Call, path: &str, depth: usize) -> Option<Receiver> {
    match call.recv_value.as_ref()? {
        RecvValue::Literal(class) => Some(Receiver {
            fqn: tree.resolve(class, &[]).fqn?,
            singleton: false,
            via: "literal",
            agreeing: 1,
            total: 1,
            ambiguous: false,
            rivals: Vec::new(),
        }),
        RecvValue::Call(at) => {
            if depth >= MAX_CHAIN {
                return None;
            }
            let previous = facts
                .calls
                .iter()
                .find(|c| c.pos == *at && c.recv != RecvShape::Symbol)?;
            returned_by(tree, facts, previous, path, depth + 1, call)
        }
    }
}

/// What a call returns, as a receiver for the next one in its chain.
///
/// When the call's own receiver has a type, its method is found and its
/// `sig` read, and the evidence for the receiver carries over. When it has
/// none, every definition of the name is asked instead.
fn returned_by(
    tree: &Tree,
    facts: &Facts,
    previous: &Call,
    path: &str,
    depth: usize,
    next: &Call,
) -> Option<Receiver> {
    let Some(receiver) = typed_at(tree, facts, previous, path, depth) else {
        // A guess among competitors has to answer the call it was made for,
        // as a name has to for `receiver_name`: one that does not is evidence
        // a competitor was the receiver.
        return by_return_types(tree, previous)
            .filter(|guess| tree.lookup(&guess.fqn, false, &next.name).is_some());
    };
    if crate::core::IDENTITY.contains(&previous.name.as_str()) {
        return Some(Receiver {
            via: "chain",
            ..receiver
        });
    }
    // `Foo.new.bar`, as `x = Foo.new` types `x`.
    if previous.name == "new" && receiver.singleton {
        return Some(Receiver {
            singleton: false,
            via: "chain",
            ..receiver
        });
    }
    let method = tree.lookup(&receiver.fqn, receiver.singleton, &previous.name)?;
    let (declarer, returns) = match method.returns_for(previous.argc, previous.block) {
        Some(returns) => (method.clone(), returns.to_string()),
        None => tree.declared_returns(&method, previous.argc, previous.block)?,
    };
    Some(Receiver {
        fqn: tree.returned_class(&declarer, &returns)?,
        singleton: false,
        via: "chain",
        rivals: Vec::new(),
        ..receiver
    })
}

/// A call whose receiver has no type returns what every definition of its
/// name that says so agrees on.
///
/// `something.gsub(/x/, "")` could be any `gsub`, and the index holds only
/// String's, which returns a String. A definition that declares no return
/// type is a competitor: it counts against the answer and makes it
/// `ambiguous`. Two that declare different ones leave the call untyped.
fn by_return_types(tree: &Tree, previous: &Call) -> Option<Receiver> {
    // Returns its receiver, which is exactly what is unknown here.
    if crate::core::IDENTITY.contains(&previous.name.as_str()) {
        return None;
    }
    let returned =
        |method: &crate::tree::MethodDef| match method.returns_for(previous.argc, previous.block) {
            Some(returns) => tree.returned_class(method, returns),
            None => tree
                .declared_returns(method, previous.argc, previous.block)
                .and_then(|(declarer, returns)| tree.returned_class(&declarer, &returns)),
        };
    // An untyped receiver is taken to be an instance, since a class mostly
    // arrives as a constant and is typed: `Dir.[]` alone says nothing of
    // `h[:a]`. But `self.class.build` and `factory.build` are classes that
    // arrived another way, so a class method declaring something else
    // objects to the instance methods' answer, though it never makes one.
    let (singletons, instances): (Vec<_>, Vec<_>) = tree
        .named(&previous.name)
        .into_iter()
        .partition(|m| m.singleton);
    let mut owners: Vec<String> = Vec::new();
    let mut votes: Vec<Option<String>> = Vec::new();
    for method in instances {
        if owners.contains(&method.owner) {
            continue;
        }
        owners.push(method.owner.clone());
        votes.push(returned(&method));
    }
    let mut declared = votes.iter().flatten();
    let fqn = declared.next()?.clone();
    if declared.any(|other| *other != fqn) {
        return None;
    }
    if singletons
        .iter()
        .filter_map(returned)
        .any(|other| other != fqn)
    {
        return None;
    }
    let agreeing = votes.iter().flatten().count();
    Some(Receiver {
        fqn,
        singleton: false,
        via: "chain:name",
        agreeing,
        total: votes.len(),
        ambiguous: agreeing < votes.len(),
        rivals: Vec::new(),
    })
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
fn from_receiver_name(tree: &Tree, call: &Call, path: &str) -> Option<Receiver> {
    let named = receiver_names_a_class(call.recv_text.as_deref()?)?;
    let fqn = tree.resolve_at(&named, &call.nesting, path).fqn?;
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
        rivals: Vec::new(),
    })
}

/// A method parameter's type, from the `params(...)` half of its `sig`.
///
/// Measured on graph_weaver: half of all untyped local receivers are
/// parameters. They have no assignment to chase, so every rung that looks for
/// one misses them — and a signature has already said what they are.
fn from_sig_params(tree: &Tree, facts: &Facts, call: &Call, path: &str) -> Option<Receiver> {
    let target = call.recv_text.as_ref()?;
    let enclosing = enclosing_method(facts, call.pos.line)?;
    let class = enclosing
        .sig_params
        .iter()
        .find(|(name, _)| name == target)
        .map(|(_, class)| class)?;
    Some(Receiver {
        fqn: tree.resolve_at(class, &call.nesting, path).fqn?,
        singleton: false,
        via: "sig:param",
        agreeing: 1,
        total: 1,
        ambiguous: false,
        rivals: Vec::new(),
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

/// What a local or instance variable holds, judged from the assignments that
/// can have set it.
///
/// A local's are the writes its read can see — the same flow analysis the LSP
/// answers a local with (DEC-064): an assignment in another method, or one a
/// later write replaced, has no vote. An instance variable spans methods, so
/// every assignment to it in the file votes, which errs toward lower
/// confidence. Writes that cannot be typed count against the answer, and
/// writes that type differently make it `ambiguous` (DEC-071).
fn from_assignments(tree: &Tree, facts: &Facts, call: &Call) -> Option<Receiver> {
    let target = call.recv_text.as_ref()?;
    let scope = call.nesting.first();
    let seen = call
        .recv_pos
        .and_then(|read| facts.reaching(read, reaching_writes));
    let (relevant, total): (Vec<&Assign>, usize) = match seen {
        Some(writes) => (
            facts
                .assigns
                .iter()
                .filter(|a| &a.target == target && writes.contains(&a.pos))
                .collect(),
            writes.len(),
        ),
        None => {
            let all: Vec<&Assign> = facts
                .assigns
                .iter()
                .filter(|a| &a.target == target && a.nesting.first() == scope)
                .collect();
            let total = all.len();
            (all, total)
        }
    };
    if relevant.is_empty() {
        return None;
    }

    let mut votes: Vec<(String, bool, &'static str)> = Vec::new();
    for assign in &relevant {
        if let Some(vote) = type_of(
            tree,
            facts,
            &assign.value,
            &assign.nesting,
            assign.pos,
            0,
            0,
        ) {
            votes.push(vote);
        }
    }
    // The type most writes agree on; among equals, the one written last.
    let (fqn, singleton, via) = votes
        .iter()
        .rev()
        .max_by_key(|(f, _, _)| votes.iter().filter(|(g, _, _)| g == f).count())
        .cloned()?;
    let agreeing = votes.iter().filter(|(f, _, _)| *f == fqn).count();
    // `rescue A, B => e` is one write with two types.
    let total = total.max(votes.len());
    let mut rivals: Vec<(String, bool)> = Vec::new();
    for (other, side, _) in &votes {
        if *other != fqn && !rivals.iter().any(|(r, _)| r == other) {
            rivals.push((other.clone(), *side));
        }
    }
    Some(Receiver {
        fqn,
        singleton,
        via,
        agreeing,
        total,
        ambiguous: !rivals.is_empty(),
        rivals,
    })
}

/// Every local read in a source → the writes that may have set it.
fn reaching_writes(source: &[u8]) -> std::collections::HashMap<Pos, Vec<Pos>> {
    let vars = crate::serve::vars::analyze(source);
    let lines = crate::extract::LineIndex::new(source);
    let at = |occurrence: &crate::serve::vars::Occurrence| lines.pos(occurrence.span.start);
    vars.occurrences
        .iter()
        .filter(|o| o.sigil == crate::serve::vars::Sigil::Local && o.read)
        .map(|read| {
            let writes = read
                .reaches
                .iter()
                .map(|&i| at(&vars.occurrences[i as usize]))
                .collect();
            (at(read), writes)
        })
        .collect()
}

/// The class a value expression produces, if syntax or a `sig` names one.
fn type_of(
    tree: &Tree,
    facts: &Facts,
    value: &ValueShape,
    nesting: &[String],
    // Where the value is written: a local it names was last set before here.
    at: Pos,
    depth: usize,
    steps: usize,
) -> Option<(String, bool, &'static str)> {
    // `x = y; y = x` is legal Ruby and would otherwise spin.
    if depth > 4 {
        return None;
    }
    match value {
        ValueShape::New(name) => Some((tree.resolve(name, nesting).fqn?, false, "local:new")),
        ValueShape::Rescued(name) => {
            Some((tree.resolve(name, nesting).fqn?, false, "local:rescue"))
        }
        // `x = Foo` holds the class itself, so `x.bar` is a class method.
        ValueShape::Const(name) => Some((tree.resolve(name, nesting).fqn?, true, "local:const")),
        ValueShape::Same(other) => {
            let next = last_write_before(facts, other, at)?;
            type_of(
                tree,
                facts,
                &next.value,
                &next.nesting,
                next.pos,
                depth + 1,
                steps,
            )
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
            let assign = last_write_before(facts, recv, at)?;
            let (owner, singleton, _) = type_of(
                tree,
                facts,
                &assign.value,
                &assign.nesting,
                assign.pos,
                depth + 1,
                steps + 1,
            )?;
            let method = tree.lookup(&owner, singleton, name)?;
            let returns = method.sig_returns.as_deref()?;
            Some((tree.returned_class(&method, returns)?, false, "sig:step"))
        }
        // A `sig` names a usable class for 64 % of signatures against 3.9 %
        // from syntax alone (PLAN §2) — the highest-yield rung on the ladder.
        ValueShape::SelfCall(name) => {
            let scope = tree.scope_fqn(nesting)?;
            let method = tree.lookup(&scope, false, name)?;
            let returns = method.sig_returns.as_deref()?;
            Some((tree.returned_class(&method, returns)?, false, "sig"))
        }
        ValueShape::ConstCall { recv, name } => {
            let owner = tree.resolve(recv, nesting).fqn?;
            // A declared return type is better evidence than a convention, so
            // it is tried first.
            if let Some(method) = tree.lookup(&owner, true, name)
                && let Some(returns) = method.sig_returns.as_deref()
            {
                return Some((tree.returned_class(&method, returns)?, false, "sig"));
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

/// The assignment to `name` nearest before `at` — the one a straight-line
/// read there would see. Not the flow analysis a receiver gets: a hop through
/// another local is one step, and a branch there is rarer than the ordering
/// mistake this avoids, which was taking the first write in the file.
fn last_write_before<'f>(facts: &'f Facts, name: &str, at: Pos) -> Option<&'f Assign> {
    facts
        .assigns
        .iter()
        .filter(|a| a.target == name && a.pos < at)
        .max_by_key(|a| a.pos)
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

/// How many trailing directories two paths share.
///
/// The call site's path is checkout-relative and a definition's is absolute, so
/// the comparison is made from the right: a shared *tail* of directories is
/// what "nearby in the tree" means when one side does not know where the
/// checkout begins.
fn shared_directories(site: &str, call_path: &str) -> usize {
    let dirs = |path: &str| -> Vec<String> {
        let mut parts: Vec<String> = path.split('/').map(str::to_string).collect();
        parts.pop();
        parts
    };
    let (a, b) = (dirs(site), dirs(call_path));
    a.iter()
        .rev()
        .zip(b.iter().rev())
        .take_while(|(x, y)| x == y)
        .count()
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
fn competitors(tree: &Tree, name: &str, winner: &str, via: &str) -> Vec<Candidate> {
    let why = match via {
        "chain:name" => {
            "defines the same name; the receiver's type is a guess from what the previous call returns"
        }
        "receiver_name" => "defines the same name; the receiver's name chose between them",
        _ => "defines the same name; the receiver's type is not certain",
    };
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
                    why,
                    kind: method.kind(),
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

/// Where the call lands for each other type the receiver's writes gave it.
/// A receiver whose class two programs declare with different superclasses,
/// in a file no nearer one than the other: whichever is loaded answers, so
/// each variant's landing is listed and none is promoted (DEC-072).
fn split_receiver(tree: &Tree, call: &Call, path: &str, receiver: Receiver) -> MethodAnswer {
    let variants = tree.variants_of(&receiver.fqn);
    let landings: Vec<crate::tree::MethodDef> = variants
        .iter()
        .filter_map(|variant| tree.lookup(variant, receiver.singleton, &call.name))
        .collect();
    let Some(first) = landings.first() else {
        return residue(
            tree,
            call,
            path,
            Some(receiver),
            "the receiver's class is declared with conflicting superclasses, and none of \
             them defines this name",
        );
    };
    MethodAnswer {
        status: Status::Ambiguous,
        confidence: share(1, variants.len()),
        resolved_via: Some(receiver.via.to_string()),
        receiver: call.recv.as_str(),
        receiver_kind: tree.kind_of(&receiver.fqn).map(str::to_string),
        receiver_type: Some(receiver.fqn.clone()),
        owner: Some(first.owner.clone()),
        kind: Some(first.kind()),
        defined_via: first.declared_via(),
        sites: vec![first.site.clone()],
        agreement: Some(format!("1/{} declarations", variants.len())),
        unresolved_ancestors: Vec::new(),
        candidates: landings[1..]
            .iter()
            .map(|method| Candidate {
                owner: method.owner.clone(),
                singleton: method.singleton,
                why: "another declaration of the receiver's class, a different class \
                      of the same name, defines it here",
                kind: method.kind(),
                site: method.site.clone(),
            })
            .collect(),
        reason: Some(format!(
            "{} is {} different classes, declared in separate files; which runs \
             depends on which is loaded",
            receiver.fqn,
            variants.len()
        )),
    }
}

impl MethodAnswer {
    /// Names as a person wrote them: a split name's variant is its name.
    fn published(mut self) -> MethodAnswer {
        let public = |name: &mut String| *name = crate::tree::public_name(name).to_string();
        self.receiver_type.as_mut().map(public);
        self.owner.as_mut().map(public);
        for candidate in &mut self.candidates {
            public(&mut candidate.owner);
        }
        self
    }
}

fn rival_landings(tree: &Tree, receiver: &Receiver, name: &str) -> Vec<Candidate> {
    receiver
        .rivals
        .iter()
        .filter_map(|(fqn, singleton)| tree.lookup(fqn, *singleton, name))
        .take(MAX_CANDIDATES)
        .map(|method| Candidate {
            owner: method.owner.clone(),
            singleton: method.singleton,
            why: "another write the receiver's read can see gives it this type",
            kind: method.kind(),
            site: method.site.clone(),
        })
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
    let here = if rspec::in_group(&call.nesting) {
        Some(rspec::EXAMPLE_GROUP.to_string())
    } else {
        call.nesting
            .first()
            .and_then(|_| tree.scope_fqn(&call.nesting))
            .map(|scope| tree.variant_at(&scope, path))
    };
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

    // `TREKR_RANK_OFF=affinity` switches the directory signal off, so it can
    // be re-sized against the gold set without a custom build. A ranking
    // feature that cannot be A/B'd is a constant somebody invented (DEC-028),
    // and that lesson cost two features and a session to learn.
    let use_affinity = !std::env::var("TREKR_RANK_OFF")
        .unwrap_or_default()
        .contains("affinity");

    // A `super` looks after its own method's owner: that method is the one
    // definition it can never reach.
    let own = |method: &crate::tree::MethodDef| {
        call.recv == RecvShape::Super
            && method.singleton == call.singleton
            && here
                .as_deref()
                .is_some_and(|here| crate::tree::public_name(here) == method.owner)
    };
    let mut ranked: Vec<(u8, bool, i32, Candidate)> = tree
        .named(&call.name)
        .into_iter()
        .filter(|method| !own(method))
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
            // A definition in the directory you are calling from is likelier
            // than one across the tree — the same intuition as "same file",
            // graded instead of binary. Negated so more shared directories
            // sorts earlier.
            let affinity = match use_affinity {
                true => -(shared_directories(&method.site.path, path) as i32),
                false => 0,
            };
            (
                tier,
                // Within a tier, this checkout's own code before a dependency's.
                !tree.in_checkout(&method.site.path),
                affinity,
                Candidate {
                    owner: method.owner.clone(),
                    singleton: method.singleton,
                    why,
                    kind: method.kind(),
                    site: method.site.clone(),
                },
            )
        })
        .collect();
    ranked.sort_by_key(|(tier, from_gem, affinity, _)| (*tier, *from_gem, *affinity));

    let total = ranked.len();
    let candidates: Vec<Candidate> = ranked
        .into_iter()
        .take(MAX_CANDIDATES)
        .map(|(_, _, _, c)| c)
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
        // A residue points at nothing, so there is no location to describe.
        // The candidates carry their own.
        kind: None,
        defined_via: None,
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
        // Not merely "the first call with this name": a symbol argument is a
        // call site too now (`delegate :where, to: :all` records `where`), and
        // it precedes the written call in source order. These tests are about
        // resolving a *receiver*, which a symbol never has, so skip them.
        let call = facts
            .calls
            .iter()
            .find(|c| c.name == name && c.recv != RecvShape::Symbol)
            .unwrap_or_else(|| panic!("no written call to {name}"))
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
            assert_eq!(found.sites[0].path, "<core>/Kernel.rb");
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

    /// Classes whose methods say what they return, for the chain tests.
    const TYPED: &str = "class Doc\n  sig { returns(Title) }\n  def title\n  end\nend\n\
                         class Title\n  def shout\n  end\nend\n";

    #[test]
    fn a_literal_receiver_is_its_class() {
        let found = answer(
            "class W\n  def go\n    \"x\".upcase\n  end\nend\n",
            "upcase",
        );
        assert_eq!(found.owner.as_deref(), Some("String"));
        assert_eq!(found.resolved_via.as_deref(), Some("literal"));
    }

    #[test]
    fn a_chain_is_typed_from_the_previous_calls_return_type() {
        let source = format!("{TYPED}class W\n  def go\n    Doc.new.title.shout\n  end\nend\n");
        let found = answer(&source, "shout");
        assert_eq!(found.status, Status::Resolved);
        assert_eq!(found.owner.as_deref(), Some("Title"));
        assert_eq!(found.resolved_via.as_deref(), Some("chain"));
    }

    #[test]
    fn an_untyped_receiver_takes_the_return_type_every_definition_agrees_on() {
        let source = format!("{TYPED}class W\n  def go(x)\n    x.title.shout\n  end\nend\n");
        let found = answer(&source, "shout");
        assert_eq!(found.status, Status::Resolved);
        assert_eq!(found.owner.as_deref(), Some("Title"));
        assert_eq!(found.resolved_via.as_deref(), Some("chain:name"));
    }

    #[test]
    fn a_definition_that_declares_nothing_is_a_competitor() {
        let source = format!(
            "{TYPED}class Book\n  def title\n  end\nend\n\
             class Loud\n  def shout\n  end\nend\n\
             class W\n  def go(x)\n    x.title.shout\n  end\nend\n"
        );
        let found = answer(&source, "shout");
        assert_eq!(found.status, Status::Ambiguous);
        assert_eq!(found.owner.as_deref(), Some("Title"));
        assert_eq!(found.confidence, 0.5);
        // The name `x` chose nothing; the previous call's return type did.
        assert!(
            found.candidates[0].why.contains("previous call"),
            "{}",
            found.candidates[0].why
        );
    }

    #[test]
    fn definitions_that_declare_different_types_leave_the_chain_untyped() {
        let source = format!(
            "{TYPED}class Book\n  sig {{ returns(Doc) }}\n  def title\n  end\nend\n\
             class W\n  def go(x)\n    x.title.shout\n  end\nend\n"
        );
        assert_eq!(answer(&source, "shout").status, Status::Residue);
    }

    #[test]
    fn a_block_decides_which_overload_a_chain_takes() {
        let source = "class Shelf\n  \
            sig { params(block: NilClass).returns(Enumerator) }\n  \
            sig { params(block: T.proc.void).returns(Array) }\n  \
            def each_book(&block)\n  end\nend\n\
            class W\n  def go(s)\n    s.each_book.next\n    s.each_book { }.last\n  end\nend\n";
        assert_eq!(owner(source, "next").as_deref(), Some("Enumerator"));
        assert_eq!(owner(source, "last").as_deref(), Some("Array"));
    }

    #[test]
    fn a_method_with_no_declared_return_ends_the_chain() {
        let source = "class Box\n  def contents\n  end\nend\n\
                      class W\n  def go\n    Box.new.contents.upcase\n  end\nend\n";
        assert_eq!(answer(source, "upcase").status, Status::Residue);
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

    /// The rails miss: `post = Post.first` two lines up, and an earlier
    /// method's `post = Cpk::Post.create!` casting the deciding vote.
    #[test]
    fn a_write_in_another_method_has_no_vote() {
        let source = "class A\n  def go\n  end\nend\nclass B\n  def go\n  end\nend\n\
                      class W\n  def one\n    x = B.new\n  end\n  \
                      def two\n    x = A.new\n    x.go\n  end\nend\n";
        let found = answer(source, "go");
        assert_eq!(found.status, Status::Resolved);
        assert_eq!(found.owner.as_deref(), Some("A"));
        assert_eq!(found.confidence, 1.0);
    }

    #[test]
    fn a_write_replaced_before_the_read_has_no_vote() {
        let source = "class A\n  def go\n  end\nend\nclass B\n  def go\n  end\nend\n\
                      class W\n  def one\n    x = B.new\n    x = A.new\n    x.go\n  end\nend\n";
        assert_eq!(answer(source, "go").owner.as_deref(), Some("A"));
    }

    #[test]
    fn branches_that_type_it_differently_are_ambiguous_and_say_so() {
        let source = "class A\n  def go\n  end\nend\nclass B\n  def go\n  end\nend\n\
                      class W\n  def one(c)\n    if c\n      x = A.new\n    else\n      \
                      x = B.new\n    end\n    x.go\n  end\nend\n";
        let found = answer(source, "go");
        assert_eq!(found.status, Status::Ambiguous);
        assert_eq!(found.confidence, 0.5);
        assert_eq!(found.agreement.as_deref(), Some("1/2"));
        assert_eq!(found.candidates.len(), 1, "the other branch's landing");
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
        let reason = found.reason.unwrap();
        assert!(reason.contains("nothing indexed in its ancestors"));
        // It checked the index, not the cause.
        assert!(!reason.contains("method_missing") && !reason.contains("gem"));
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
