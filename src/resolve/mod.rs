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

pub(crate) mod members;
mod rabl;
pub(crate) mod refs;
pub(crate) mod vars;
pub(crate) mod views;

/// The class a RABL template's `self` is (DEC-526).
pub(crate) const RABL_ENGINE: &str = rabl::ENGINE;

use crate::core::{Assign, Call, Def, Facts, Pos, RecvShape, RecvValue, ValueShape, rspec};
use crate::tree::{Kind, Site, Status, Tree};
use serde::Serialize;

/// How the receiver's type was established, and how strongly.
#[derive(Clone)]
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
    /// The type is one the object conforms to — declared by a `sig`, or
    /// read from a convention — not the class it was made as, so it may be
    /// any subclass (DEC-140). Unsure is `false`: it rules nothing back in.
    pub(super) bound: bool,
}

impl Receiver {
    /// The method this receiver runs: through `self`'s chain when the
    /// receiver is the scope the call is written in (DEC-105).
    pub(super) fn lookup(&self, tree: &Tree, name: &str) -> Option<crate::tree::MethodDef> {
        match self.via {
            "self" => tree.lookup_self(&self.fqn, self.singleton, name),
            // A view's `self`: the app's helpers, then ActionView's — or the
            // controller a `helper_method` sends the name to (DEC-521).
            "view" if self.fqn == crate::tree::views::ACTION_VIEW => tree.lookup_in_view(name),
            // A RABL template's `self`: its engine, which sends what it lacks
            // to the view (DEC-526).
            "rabl" if self.fqn == rabl::ENGINE => tree
                .lookup(rabl::ENGINE, false, name)
                .or_else(|| tree.lookup_in_view(name)),
            // A top-level `def` is a private method of Object, ahead of
            // Kernel — when only one file writes it, since which of several
            // is loaded is not the index's to say.
            "main" => match top_level_defs(tree, name).as_slice() {
                [only] => Some(crate::tree::MethodDef {
                    owner: "Object".to_string(),
                    ..only.clone()
                }),
                _ => tree.lookup(&self.fqn, self.singleton, name),
            },
            _ => tree.lookup(&self.fqn, self.singleton, name),
        }
    }
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
    /// agreed — a count, not a calibration (DEC-011). For a residue it is how
    /// often its first candidate ran, on the gold sets, among residues resting
    /// on the same evidence (DEC-442).
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
    /// What `confidence` counts: assignments that agreed / were considered,
    /// when a rung inferred a type; for a residue, the evidence its first
    /// candidate rests on.
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
    if let Some(member) = example_member(tree, facts, call, path) {
        return member_answer(tree, call, member, path);
    }
    // A partial's local, handed it by the renders that reach it (DEC-525).
    if let Some(local) = views::partial_local(tree, call, path) {
        return local_answer(call, local);
    }
    if call.recv == RecvShape::Symbol
        && let Some(target) = &call.stands_for
    {
        return symbol_answer(tree, facts, call, target, path);
    }
    let shape = call.recv.as_str();
    match receiver_of(tree, facts, call, path).map(|r| rival_with(tree, call, r)) {
        Some(receiver) => {
            if let Some(predicate) = &call.stands_for
                && answered_by_matchers(tree, &receiver, &call.name)
            {
                return predicate_answer(tree, facts, call, predicate, path);
            }
            match lookup_on(tree, call, &receiver)
                .map(|found| initialize_for(tree, call, &receiver, &found).unwrap_or(found))
                .or_else(|| constructed_without_core(tree, call, &receiver))
            {
                Some(found)
                    if let Some(answer) = through_delegate(tree, call, &receiver, &found) =>
                {
                    answer
                }
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
                    // A name `helper_method` exposes runs the method Rails
                    // generates at that line, which sends it on (DEC-521).
                    let mut sites = sites;
                    if matches!(receiver.via, "view" | "rabl")
                        && let Some(at) = tree.exposed_at(&found.owner, &call.name)
                    {
                        sites.push(at);
                    }
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
                        None if let Some(found) = on_any_object(tree, &call.name, &receiver) => {
                            object_answer(tree, call, &receiver, found)
                        }
                        None if defined_nowhere(tree, call) => {
                            let reason = nowhere(tree, call, path);
                            residue(tree, call, path, Some(receiver), &reason)
                        }
                        None => {
                            let reason = unmixed_reason(tree, call, &receiver.fqn);
                            residue(tree, call, path, Some(receiver), &reason)
                        }
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
                // A view whose ActionView the index lacks: its helpers were
                // asked, and ActionView's own could not be (DEC-521).
                None if matches!(receiver.via, "view" | "rabl")
                    && !tree.is_known(crate::tree::views::ACTION_VIEW) =>
                {
                    residue(
                        tree,
                        call,
                        path,
                        Some(receiver),
                        "the call runs on a view, which has no helper by this name, and \
                         actionview is not indexed",
                    )
                }
                // A view looked through everything Rails gives it; a maker of
                // unnamed methods deep in ActionView's chain is no lead.
                None if matches!(receiver.via, "view" | "rabl") && !defined_nowhere(tree, call) => {
                    let reason = match receiver.via {
                        "rabl" => {
                            "the call runs on a RABL template, and neither its engine nor \
                                   the view has this name: no helper under app/helpers, \
                                   controller `helper_method` or ActionView method"
                        }
                        _ => {
                            "the call runs on a view, and nothing it looks through has this \
                              name: no helper under app/helpers, controller `helper_method` or \
                              ActionView method"
                        }
                    };
                    residue(tree, call, path, Some(receiver), reason)
                }
                // Ruby's lookup fails, and a `method_missing` hands the name
                // on: a relation's to its model (DEC-116), a class's
                // `delegate_missing_to` to its target (DEC-112).
                None if let Some(answer) = handed_on(tree, facts, call, path, &receiver) => answer,
                // A method made from a name the source does not state may be
                // this one, whether or not the name is defined elsewhere: the
                // scope that makes such methods is the specific answer (DEC-130).
                None if let Some((maker, how)) =
                    tree.dynamic_in_chain(&receiver.fqn, receiver.singleton, &call.name) =>
                {
                    let does = match how.iter().all(|how| how.maker.forwards()) {
                        true => "sends a name it lacks on to another object",
                        false => "defines methods its source does not name",
                    };
                    let reason = format!(
                        "{CHECKED}, but {maker} {does} ({})",
                        tree.dynamic_note(&how)
                    );
                    residue(tree, call, path, Some(receiver), &reason)
                }
                None if defined_nowhere(tree, call) => {
                    let reason = nowhere(tree, call, path);
                    residue(tree, call, path, Some(receiver), &reason)
                }
                // The type is settled and Ruby would still not find the method
                // in what is indexed. Say what was checked, never why: the
                // cause is exactly what was not seen.
                None => residue(tree, call, path, Some(receiver), CHECKED),
            }
        }
        None if rspec::in_group(&call.nesting) && call.recv == RecvShape::Implicit => residue(
            tree,
            call,
            path,
            None,
            "the call is in a block handed to a method that may run it on another object",
        ),
        None if defined_nowhere(tree, call) => {
            let reason = nowhere(tree, call, path);
            residue(tree, call, path, None, &reason)
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

/// `X.new` that reaches core's `new` runs the `initialize` an `X` finds, and
/// that is where a reader of the call wants to go (DEC-541). A custom `new`
/// is its own answer, and core's `initialize` is no better than core's `new`.
fn initialize_for(
    tree: &Tree,
    call: &Call,
    receiver: &Receiver,
    found: &crate::tree::MethodDef,
) -> Option<crate::tree::MethodDef> {
    if call.name != "new" || !receiver.singleton || !crate::tree::is_core(&found.site.path) {
        return None;
    }
    tree.lookup(&receiver.fqn, false, "initialize")
        .filter(|init| !crate::tree::is_core(&init.site.path))
}

/// With no core indexed, `X.new` finds no `new` at all; a class's own
/// `initialize` is still what it runs.
fn constructed_without_core(
    tree: &Tree,
    call: &Call,
    receiver: &Receiver,
) -> Option<crate::tree::MethodDef> {
    if call.name != "new" || !receiver.singleton || tree.kind_of(&receiver.fqn) != Some("class") {
        return None;
    }
    tree.lookup(&receiver.fqn, false, "initialize")
}

/// The method `--refs` at a call is asked about, as (name, class side): the
/// call's own name — or `initialize`, where an `X.new` was answered with the
/// `initialize` it runs (DEC-541) — on the side of the method it landed on,
/// which its receiver decides, not the `def` it is written in (DEC-563).
pub(crate) fn asked_at(tree: &Tree, call: &Call, answer: &MethodAnswer) -> (String, bool) {
    let landed_on = |singleton: bool, name: &str| {
        answer.owner.as_deref().is_some_and(|owner| {
            tree.lookup(owner, singleton, name).is_some_and(|found| {
                answer.sites.first().is_some_and(|site| {
                    site.path == found.site.path && site.line == found.site.line
                })
            })
        })
    };
    if call.name == "new" && landed_on(false, "initialize") {
        return ("initialize".to_string(), false);
    }
    let singleton = match (landed_on(true, &call.name), landed_on(false, &call.name)) {
        (true, false) => true,
        (false, true) => false,
        _ => call.singleton,
    };
    (call.name.clone(), singleton)
}

/// A partial's local: defined where each `render` hands it (DEC-525).
fn local_answer(call: &Call, local: views::PartialLocal) -> MethodAnswer {
    let types: Vec<&String> = local.types.iter().fold(Vec::new(), |mut seen, t| {
        if !seen.contains(&t) {
            seen.push(t);
        }
        seen
    });
    MethodAnswer {
        status: match local.sites.len() {
            1 => Status::Resolved,
            _ => Status::Ambiguous,
        },
        confidence: share(1, local.sites.len()),
        resolved_via: Some("render".to_string()),
        receiver: call.recv.as_str(),
        receiver_type: match types.as_slice() {
            [one] => Some((*one).clone()),
            _ => None,
        },
        receiver_kind: None,
        owner: None,
        kind: None,
        defined_via: Some("render".to_string()),
        sites: local.sites,
        agreement: None,
        unresolved_ancestors: Vec::new(),
        candidates: Vec::new(),
        reason: None,
    }
}

/// Why a call in a module found nothing through the classes that mix it in:
/// none does, or those that do lack the name — on the side the call runs
/// on, which in an `included do` block is the class's own.
fn unmixed_reason(tree: &Tree, call: &Call, module: &str) -> String {
    let module = crate::tree::public_name(module);
    if tree.includers_of(module).is_empty() {
        return format!(
            "the call is inside module {module}, and no class the index knows of mixes it in"
        );
    }
    if call.singleton {
        return format!(
            "the call runs on the classes that include {module}, as a class method, \
             and none of them has a class method of this name"
        );
    }
    format!("the call is inside module {module}, and no class that mixes it in defines this name")
}

/// Why a name no indexed file defines has no answer. Distinct from a known
/// receiver whose ancestors lack it: there, the name exists and the chain
/// does not reach it; here, nothing trekr read defines it at all.
/// What a residue on a known receiver says was checked.
const CHECKED: &str =
    "the receiver's type is known, and nothing indexed in its ancestors defines this name";

const NOWHERE: &str = "nothing trekr indexed defines this name anywhere — not this checkout, \
     its gems or Ruby core; a gem may generate it at runtime (Devise's \
     `authenticate_user!` is one), or define it in a gem that is not installed";

/// The same, where no core is loaded: then the claim cannot cover core.
const NOWHERE_NO_CORE: &str = "nothing trekr indexed defines this name — not this checkout or \
     its gems — and Ruby core is not indexed for this checkout (no Ruby found for it, or its \
     Ruby carries no rbs gem; see `trekr --status`), so a core method is not known either";

/// Why a name defined nowhere is residue: the file's own marker that may
/// make it, ahead of a gem (DEC-162).
fn nowhere(tree: &Tree, call: &Call, path: &str) -> String {
    let makers = tree.dynamic_in_file(path, &call.name);
    if makers.is_empty() && !tree.has_core() {
        return NOWHERE_NO_CORE.to_string();
    }
    if makers.is_empty() {
        return NOWHERE.to_string();
    }
    format!(
        "nothing trekr indexed defines this name, but this file makes methods its \
         source does not name ({}), which may include it",
        tree.dynamic_note(&makers)
    )
}

/// Does no indexed definition, anywhere, carry this call's name?
fn defined_nowhere(tree: &Tree, call: &Call) -> bool {
    tree.named(&call.name).is_empty()
}

/// Where a name the receiver lacks goes instead, when its class says.
pub(super) fn handed_on(
    tree: &Tree,
    facts: &Facts,
    call: &Call,
    path: &str,
    receiver: &Receiver,
) -> Option<MethodAnswer> {
    to_the_model(tree, facts, call, path, receiver)
        .or_else(|| forwarded(tree, call, path, receiver))
}

/// Ruby's lookup from the receiver, but for a scope's body: the relation
/// Rails builds has a delegation to each model class method that Kernel also
/// answers (`display`, `format`), ahead of Kernel's own, so the model's wins
/// (DEC-136).
pub(super) fn lookup_on(
    tree: &Tree,
    call: &Call,
    receiver: &Receiver,
) -> Option<crate::tree::MethodDef> {
    let found = receiver.lookup(tree, &call.name)?;
    if receiver.via != "scope" || !crate::tree::is_core(&found.site.path) {
        return Some(found);
    }
    let model = tree.scope_fqn(&call.nesting)?;
    match tree.lookup(&model, true, &call.name) {
        Some(own) if !crate::tree::is_core(&own.site.path) => Some(own),
        _ => Some(found),
    }
}

/// Does a scope's body in this nesting run on an ActiveRecord relation? Only
/// in a model, or a module a model includes: Mongoid and a plain class that
/// defines its own `scope` run it on something else (DEC-136).
fn runs_on_a_relation(tree: &Tree, call: &Call, path: &str) -> bool {
    const BASE: &str = "ActiveRecord::Base";
    // A name split by its superclasses is the variant this file declares.
    let Some(scope) = tree
        .scope_fqn(&call.nesting)
        .map(|scope| tree.variant_at(&scope, path))
    else {
        return false;
    };
    tree.inherits(&scope, BASE)
        || tree.kind_of(&scope) == Some("module")
            && tree
                .includers_of(&scope)
                .iter()
                .any(|includer| tree.inherits(includer, BASE))
}

/// The relation a scope's body runs on hands a name it lacks to its model's
/// class methods — another scope, a class method — as
/// `ActiveRecord::Delegation` does (DEC-116); so does a relation a chain
/// returns, of the model the chain started from (DEC-444).
fn to_the_model(
    tree: &Tree,
    facts: &Facts,
    call: &Call,
    path: &str,
    receiver: &Receiver,
) -> Option<MethodAnswer> {
    let model = relation_model(tree, facts, call, path, receiver, 0)?;
    let found = tree.lookup(&model, true, &call.name)?;
    let via = if receiver.via == "scope" {
        "scope"
    } else {
        "relation"
    };
    Some(MethodAnswer {
        status: Status::Resolved,
        confidence: 1.0,
        resolved_via: Some(via.to_string()),
        receiver: call.recv.as_str(),
        receiver_kind: tree.kind_of(&model).map(str::to_string),
        receiver_type: Some(model),
        owner: Some(found.owner.clone()),
        kind: Some(found.kind()),
        defined_via: found.declared_via(),
        sites: vec![found.site.clone()],
        agreement: None,
        unresolved_ancestors: Vec::new(),
        candidates: Vec::new(),
        reason: None,
    })
}

/// Is `fqn` an ActiveRecord relation — a `has_many` reader's
/// CollectionProxy as much as a query's Relation?
pub(super) fn is_relation(tree: &Tree, fqn: &str) -> bool {
    crate::tree::public_name(fqn) == RELATION || tree.inherits(fqn, RELATION)
}

/// The model whose class methods a relation receiver of `call` answers
/// with: the class a scope's body is in, the class a chain starts from
/// (`Post.where(…)`), or the class a `has_many` reader names. `None` when
/// the chain does not say — a local, a parameter.
pub(super) fn relation_model(
    tree: &Tree,
    facts: &Facts,
    call: &Call,
    path: &str,
    receiver: &Receiver,
    depth: usize,
) -> Option<String> {
    if receiver.singleton || !is_relation(tree, &receiver.fqn) || depth >= MAX_CHAIN {
        return None;
    }
    if receiver.via == "scope" {
        return tree.scope_fqn(&call.nesting);
    }
    let Some(RecvValue::Call(at)) = &call.recv_value else {
        return None;
    };
    let previous = facts
        .calls
        .iter()
        .find(|c| c.pos == *at && c.recv != RecvShape::Symbol)?;
    let before = receiver_of(tree, facts, previous, path)?;
    if before.singleton && tree.inherits(&before.fqn, "ActiveRecord::Base") {
        return Some(before.fqn);
    }
    if let Some(model) = relation_model(tree, facts, previous, path, &before, depth + 1) {
        return Some(model);
    }
    let reader = before.lookup(tree, &previous.name)?;
    let records = reader.records.as_deref()?;
    tree.returned_class(&reader, records)
}

/// A call that lands on a `delegate … to: :x`: the method `x`'s type runs,
/// as `--refs` counts it (DEC-166), with the delegate kept as the second
/// site. The type is a bound (DEC-140), so a subclass of it that overrides
/// the name makes the answer ambiguous, naming each (DEC-211). `None` when
/// the delegate's target has no known type, or its type lacks the name: the
/// delegate is then the answer.
fn through_delegate(
    tree: &Tree,
    call: &Call,
    receiver: &Receiver,
    landed: &crate::tree::MethodDef,
) -> Option<MethodAnswer> {
    let refs::Delegated::To { fqn, bound } = refs::delegated(tree, receiver, landed)? else {
        return None;
    };
    let found = tree.lookup(&fqn, false, &call.name)?;
    let target = Receiver {
        fqn: fqn.clone(),
        singleton: false,
        via: "delegate",
        bound,
        agreeing: receiver.agreeing,
        total: receiver.total,
        ambiguous: receiver.ambiguous,
        rivals: Vec::new(),
    };
    let overrides: Vec<Candidate> = if bound {
        overrides_of(
            tree,
            &target,
            &call.name,
            &found,
            "a subclass overrides it, and the delegate's target may be one",
        )
    } else {
        Vec::new()
    };
    let holder = crate::tree::public_name(&landed.owner);
    let to = landed.forwards_to.as_deref().unwrap_or_default();
    Some(MethodAnswer {
        status: if receiver.ambiguous || !overrides.is_empty() {
            Status::Ambiguous
        } else {
            Status::Resolved
        },
        confidence: match overrides.len() {
            0 => share(receiver.agreeing, receiver.total),
            n => share(1, n + 1),
        },
        resolved_via: Some("delegate".to_string()),
        receiver: call.recv.as_str(),
        receiver_kind: tree.kind_of(&fqn).map(str::to_string),
        receiver_type: Some(fqn.clone()),
        owner: Some(found.owner.clone()),
        kind: Some(found.kind()),
        defined_via: found.declared_via(),
        sites: vec![found.site.clone(), landed.site.clone()],
        agreement: agreement(receiver),
        unresolved_ancestors: Vec::new(),
        candidates: overrides.into_iter().take(MAX_CANDIDATES).collect(),
        reason: Some(format!(
            "sent on by the `delegate` in {holder} to `{to}`, typed {fqn}"
        )),
    })
}

/// A name the receiver lacks, when its class `delegate_missing_to`s a
/// target: the target's method, looked up on the class the target's reader
/// returns. Residue that says so when that class is not known, or lacks the
/// name too.
pub(super) fn forwarded(
    tree: &Tree,
    call: &Call,
    path: &str,
    receiver: &Receiver,
) -> Option<MethodAnswer> {
    let catcher = tree.lookup(&receiver.fqn, receiver.singleton, "method_missing")?;
    let target = catcher.forwards_to.as_deref()?;
    let holder = crate::tree::public_name(&catcher.owner).to_string();
    let typed = tree
        .lookup(&receiver.fqn, receiver.singleton, target)
        .and_then(|reader| {
            let returns = reader.returns_for(Some(0), false)?.to_string();
            tree.returned_class(&reader, &returns)
        });
    let Some(fqn) = typed else {
        return Some(residue(
            tree,
            call,
            path,
            None,
            &format!(
                "{holder} hands a name it lacks to `{target}` (delegate_missing_to), \
                 whose type is not determined"
            ),
        ));
    };
    let target_receiver = Receiver {
        fqn: fqn.clone(),
        singleton: false,
        via: "delegate_missing_to",
        bound: true,
        agreeing: receiver.agreeing,
        total: receiver.total,
        ambiguous: receiver.ambiguous,
        rivals: Vec::new(),
    };
    let Some(found) = tree.lookup(&fqn, false, &call.name) else {
        return Some(residue(
            tree,
            call,
            path,
            Some(target_receiver),
            &format!(
                "{holder} hands a name it lacks to `{target}` (delegate_missing_to), \
                 typed {fqn}, and nothing indexed in its ancestors defines this name"
            ),
        ));
    };
    Some(MethodAnswer {
        status: if receiver.ambiguous {
            Status::Ambiguous
        } else {
            Status::Resolved
        },
        confidence: share(receiver.agreeing, receiver.total),
        resolved_via: Some("delegate_missing_to".to_string()),
        receiver: call.recv.as_str(),
        receiver_kind: tree.kind_of(&fqn).map(str::to_string),
        receiver_type: Some(fqn),
        owner: Some(found.owner.clone()),
        kind: Some(found.kind()),
        defined_via: found.declared_via(),
        sites: vec![found.site.clone()],
        agreement: agreement(receiver),
        unresolved_ancestors: Vec::new(),
        candidates: Vec::new(),
        reason: None,
    })
}

/// Is this a name only `RSpec::Matchers#method_missing` answers — no method
/// of its own, on an example that has RSpec's matchers? ExampleGroup's own
/// `method_missing` comes first and hands every such name on with `super`.
fn answered_by_matchers(tree: &Tree, receiver: &Receiver, name: &str) -> bool {
    receiver.via == "example_group"
        && !receiver.singleton
        && tree.lookup(&receiver.fqn, false, name).is_none()
        && tree
            .ancestors(&receiver.fqn)
            .chain
            .iter()
            .any(|ancestor| ancestor == rspec::MATCHERS)
        && tree
            .lookup(rspec::MATCHERS, false, "method_missing")
            .is_some_and(|catcher| catcher.owner == rspec::MATCHERS)
}

/// A predicate matcher answers with the predicate it calls on the subject:
/// `be_empty` is `empty?`, or `empties?` when that is what the subject has,
/// as BePredicate falls back to the present tense (DEC-090).
fn predicate_answer(
    tree: &Tree,
    facts: &Facts,
    matcher: &Call,
    predicate: &Call,
    path: &str,
) -> MethodAnswer {
    let answer = call_at(tree, facts, predicate, path);
    let answer = match rspec::present_tense(&predicate.name) {
        Some(present) if answer.status == Status::Residue && matcher.name.starts_with("be_") => {
            let present = Call {
                name: present,
                ..predicate.clone()
            };
            let fallback = call_at(tree, facts, &present, path);
            if fallback.status == Status::Residue {
                answer
            } else {
                fallback
            }
        }
        _ => answer,
    };
    if answer.status == Status::Residue {
        let why = answer.reason.clone().unwrap_or_default();
        return MethodAnswer {
            reason: Some(format!(
                "`{}` is RSpec's predicate matcher, which calls `{}` on the expectation's \
                 subject: {why}",
                matcher.name, predicate.name
            )),
            ..answer
        };
    }
    MethodAnswer {
        resolved_via: Some("predicate_matcher".to_string()),
        ..answer
    }
}

/// A symbol that names a method answers with that method, looked up on the
/// object that will call it: the receiver of `send` and its kin, `self` for a
/// class-level macro (DEC-093).
fn symbol_answer(
    tree: &Tree,
    facts: &Facts,
    symbol: &Call,
    target: &Call,
    path: &str,
) -> MethodAnswer {
    let answer = call_at(tree, facts, target, path);
    if answer.status == Status::Residue {
        let why = answer.reason.clone().unwrap_or_default();
        return MethodAnswer {
            receiver: symbol.recv.as_str(),
            reason: Some(format!(
                "the symbol names a method of what it is sent to: {why}"
            )),
            ..answer
        };
    }
    MethodAnswer {
        receiver: symbol.recv.as_str(),
        resolved_via: Some("symbol".to_string()),
        ..answer
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
pub(super) fn on_the_example(tree: &Tree, facts: &Facts, call: &Call, path: &str) -> bool {
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

/// The example group's own method a call names, when the call runs on the
/// example: written there, or in a block handed to a helper of the same
/// group that names it (DEC-113).
pub(super) fn example_member<'f>(
    tree: &Tree,
    facts: &'f Facts,
    call: &Call,
    path: &str,
) -> Option<Member<'f>> {
    let member = group_member(tree, facts, call)?;
    if on_the_example(tree, facts, call, path) {
        return Some(member);
    }
    let helper = facts
        .calls
        .iter()
        .find(|c| Some(c.pos) == call.block_owner && c.recv != RecvShape::Symbol)?;
    let admitted = on_the_example(tree, facts, helper, path)
        && !evaluates_its_block(helper)
        && group_member(tree, facts, helper).is_some_and(|of| of.same_group(&member));
    admitted.then_some(member)
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
    ) || (call.recv == RecvShape::Implicit
        // A custom matcher's block is the body of a matcher, not of the
        // example: its `match` is the DSL's, not RSpec::Matchers' (DEC-091).
        && matches!(call.name.as_str(), "define" | "matcher"))
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
                && group_member(tree, facts, owner).is_none()
                && tree
                    .lookup(rspec::EXAMPLE_GROUP, owner.singleton, &owner.name)
                    .is_some()
        }
        // Ruby's own: a class core or the stdlib declares, sending a method
        // one of them defines or no one indexed does (DEC-180).
        RecvShape::Const => {
            let Some(class) = owner
                .recv_text
                .as_deref()
                .and_then(|name| tree.resolve_at(name, &owner.nesting, path).fqn)
                .and_then(|fqn| tree.namespace_named(&fqn))
            else {
                return false;
            };
            let rubys = |site: &crate::tree::Site| {
                crate::tree::is_core(&site.path) || tree.in_stdlib(&site.path)
            };
            tree.sites(&class).iter().any(rubys)
                && tree
                    .lookup(&class, true, &owner.name)
                    .is_none_or(|method| rubys(&method.site))
        }
        RecvShape::Symbol | RecvShape::Super => false,
    }
}

/// What a group's own name is: a `let`, `subject` or `def` of this file, or
/// a method of a shared group the group includes (DEC-092).
pub(super) enum Member<'f> {
    Here(&'f Def),
    Shared(Box<crate::tree::MethodDef>),
}

impl Member<'_> {
    /// Do both come from one group: one shared group's module, or one
    /// group's body in this file?
    fn same_group(&self, other: &Member<'_>) -> bool {
        match (self, other) {
            (Member::Here(a), Member::Here(b)) => a.nesting == b.nesting,
            (Member::Shared(a), Member::Shared(b)) => a.owner == b.owner,
            _ => false,
        }
    }

    /// The owner an answer names, as `member_answer` reports it.
    pub(super) fn owner(&self) -> String {
        match self {
            Member::Here(def) if rspec::is_shared_member(def) => {
                def.nesting[0].trim_start_matches("::").to_string()
            }
            Member::Here(def) => rspec::class_name(&def.nesting),
            Member::Shared(found) => found.owner.clone(),
        }
    }
}

/// A method an enclosing example group defines — a `let`, a `subject`, a
/// `def` in its body — which only this file can see (DEC-084), or one a
/// top-level shared group it includes defines, which any file can (DEC-092).
///
/// The innermost group that has the name wins, as the subclass does: its own
/// definitions first, the last one written, as a redefined method does; then
/// the shared groups it includes, the last included first. A symbol is the
/// definition itself only where it is written: `let(:name)`.
pub(super) fn group_member<'f>(tree: &Tree, facts: &'f Facts, call: &Call) -> Option<Member<'f>> {
    let named = |def: &&Def| def.kind == crate::core::Kind::Method && def.name == call.name;
    if call.recv == RecvShape::Symbol {
        return facts
            .defs
            .iter()
            .filter(named)
            .filter(|def| def.is_group_member() || rspec::is_shared_member(def))
            .find(|def| def.pos == call.pos)
            .map(Member::Here);
    }
    if !matches!(call.recv, RecvShape::Implicit | RecvShape::SelfRecv) {
        return None;
    }
    member_at(tree, facts, &call.nesting, &call.name, call.singleton)
}

/// The group member `name` is, for a call written at `nesting`: as
/// `group_member` finds it, without the call.
pub(super) fn member_at<'f>(
    tree: &Tree,
    facts: &'f Facts,
    nesting: &[String],
    name: &str,
    singleton: bool,
) -> Option<Member<'f>> {
    if !rspec::in_group(nesting) {
        return None;
    }
    let named = |def: &&Def| def.kind == crate::core::Kind::Method && def.name == name;
    for at in 0..nesting.len() {
        let level = &nesting[at..];
        if !rspec::is_group(&level[0]) {
            continue;
        }
        let own = facts
            .defs
            .iter()
            .filter(named)
            .filter(|def| def.singleton == singleton && def.nesting == level)
            .max_by_key(|def| def.pos);
        if let Some(def) = own {
            return Some(Member::Here(def));
        }
        if singleton {
            continue;
        }
        let included = facts
            .shared_includes
            .iter()
            .rev()
            .filter(|(group, _)| group == level)
            .map(|(_, module)| module.clone())
            .chain(rspec::shared_module_of(&level[0]));
        for module in included {
            if let Some(found) = tree.lookup(&module, false, name) {
                return Some(Member::Shared(Box::new(found)));
            }
        }
    }
    None
}

/// The group member `name` a call written at `nesting` sees, of this file's
/// own: the innermost group's, the last written.
fn visible_member<'f>(facts: &'f Facts, nesting: &[String], name: &str) -> Option<&'f Def> {
    (0..nesting.len())
        .map(|at| &nesting[at..])
        .filter(|level| rspec::is_group(&level[0]))
        .find_map(|level| {
            facts
                .defs
                .iter()
                .filter(|def| def.kind == crate::core::Kind::Method && !def.singleton)
                .filter(|def| def.name == name && def.nesting == level)
                .max_by_key(|def| def.pos)
        })
}

fn member_answer(tree: &Tree, call: &Call, member: Member<'_>, path: &str) -> MethodAnswer {
    let (owner, kind, defined_via, site) = match &member {
        Member::Here(def) => {
            let kind = Kind::of(def.via.as_deref());
            let site = Site {
                path: tree.site_path(path),
                line: def.pos.line,
                col: def.pos.col,
                kind: "method".to_string(),
            };
            let via = (kind == Kind::Declaration)
                .then(|| def.via.clone())
                .flatten();
            (member.owner(), kind, via, site)
        }
        Member::Shared(found) => (
            found.owner.clone(),
            found.kind(),
            found.declared_via(),
            found.site.clone(),
        ),
    };
    MethodAnswer {
        status: Status::Resolved,
        confidence: 1.0,
        resolved_via: Some("example_group".to_string()),
        receiver: call.recv.as_str(),
        receiver_type: (call.recv != RecvShape::Symbol).then(|| rspec::class_name(&call.nesting)),
        receiver_kind: (call.recv != RecvShape::Symbol).then(|| "class".to_string()),
        owner: Some(owner),
        kind: Some(kind),
        defined_via,
        sites: vec![site],
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
    overrides_of(
        tree,
        receiver,
        name,
        found,
        "a subclass overrides it, and `self` may be one",
    )
}

/// The definitions of `name` a receiver of `receiver`'s type may run in
/// place of `found`: a subclass's, or a module's a subclass mixes in.
fn overrides_of(
    tree: &Tree,
    receiver: &Receiver,
    name: &str,
    found: &crate::tree::MethodDef,
    why: &'static str,
) -> Vec<Candidate> {
    // Every definition of the name, not every subclass: the name's list is
    // short, and a class with thousands of descendants is not.
    tree.named(name)
        .iter()
        .filter(|method| {
            method.singleton == receiver.singleton
                && method.owner != found.owner
                && overrides_for_self(tree, receiver, method)
        })
        .map(|method| Candidate {
            owner: method.owner.clone(),
            singleton: method.singleton,
            why,
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
/// A call on `self` in a module's instance method runs on whatever mixes
/// the module in, and every such object is an Object: when no includer
/// answers, what Object's chain has — `Kernel#Pathname`, `Integer()` — is
/// what it runs (DEC-314).
pub(crate) fn on_any_object(
    tree: &Tree,
    name: &str,
    receiver: &Receiver,
) -> Option<crate::tree::MethodDef> {
    if receiver.singleton || receiver.via != "self" || tree.kind_of(&receiver.fqn) != Some("module")
    {
        return None;
    }
    tree.lookup("Object", false, name)
}

fn object_answer(
    tree: &Tree,
    call: &Call,
    receiver: &Receiver,
    found: crate::tree::MethodDef,
) -> MethodAnswer {
    MethodAnswer {
        status: Status::Resolved,
        confidence: share(1, 1),
        resolved_via: Some("object".to_string()),
        receiver: call.recv.as_str(),
        receiver_kind: tree.kind_of(&receiver.fqn).map(str::to_string),
        receiver_type: Some(receiver.fqn.clone()),
        owner: Some(found.owner.clone()),
        kind: Some(found.kind()),
        defined_via: found.declared_via(),
        sites: vec![found.site.clone()],
        agreement: None,
        unresolved_ancestors: Vec::new(),
        candidates: Vec::new(),
        reason: None,
    }
}

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

/// The methods a definition overrides: what a `super` written in it would
/// reach, from each class that can run it. Whoever calls one of those can
/// reach this one instead, framework code the checkout never names included
/// (DEC-121). Each is `Owner#name`, or `Owner.name` on the singleton side.
pub(crate) fn overridden(tree: &Tree, def: &crate::core::Def, path: &str) -> Vec<String> {
    let probe = Call {
        name: def.name.clone(),
        recv: RecvShape::Super,
        recv_text: None,
        nesting: def.nesting.clone(),
        singleton: def.singleton,
        recv_pos: None,
        recv_value: None,
        block_owner: None,
        in_example: false,
        group_body: false,
        in_scope: false,
        stands_for: None,
        argc: None,
        block: false,
        pos: def.pos,
    };
    let Ok(landings) = super_landings(tree, &probe, path) else {
        return Vec::new();
    };
    let mut overridden: Vec<String> = Vec::new();
    for method in landings.per_class.iter().filter_map(|(_, m)| m.as_ref()) {
        let separator = if method.singleton { "." } else { "#" };
        let label = format!(
            "{}{separator}{}",
            crate::tree::public_name(&method.owner),
            method.name
        );
        if !overridden.contains(&label) {
            overridden.push(label);
        }
    }
    overridden
}

/// What a `def` the source does not settle is defined on (DEC-562).
pub(crate) enum DefinedOn {
    /// One object of this class: `def clock.x` on a typed local, or a `def`
    /// in its `instance_eval` block.
    Object(String),
    /// Whatever its block runs on, or an object trekr cannot type.
    Unknown,
}

/// `None` for a `def` its scope owns, which includes one in a block that a
/// method of Ruby's own runs as it stands (`each`, `tap`): DEC-391's rule.
pub(crate) fn defined_on(
    tree: &Tree,
    facts: &Facts,
    def: &crate::core::Def,
    path: &str,
) -> Option<DefinedOn> {
    let probe = |recv, recv_text, recv_pos, block_owner| Call {
        name: def.name.clone(),
        recv,
        recv_text,
        nesting: def.nesting.clone(),
        singleton: def.singleton,
        recv_pos,
        recv_value: None,
        block_owner,
        in_example: false,
        group_body: false,
        in_scope: false,
        stands_for: None,
        argc: None,
        block: false,
        pos: def.pos,
    };
    let object = |receiver: Option<Receiver>| {
        receiver
            .filter(|r| !r.singleton && !r.ambiguous)
            .map_or(DefinedOn::Unknown, |r| DefinedOn::Object(r.fqn))
    };
    match def.unsettled.as_ref()? {
        crate::core::Unsettled::Object { local, at } => {
            // The writes the receiver's read can see, and only when they agree:
            // a parameter, or a value of no type, leaves the object unknown.
            let read = probe(RecvShape::Local, Some(local.clone()), Some(*at), None);
            let agreed = receiver_of(tree, facts, &read, path).filter(|r| r.agreeing == r.total);
            Some(object(agreed))
        }
        crate::core::Unsettled::Block(at) => {
            let inside = probe(RecvShape::Implicit, None, None, Some(*at));
            if !self_unsettled(tree, facts, &inside, path) {
                return None;
            }
            let runs = facts
                .calls
                .iter()
                .find(|c| c.pos == *at && c.recv != RecvShape::Symbol);
            Some(match runs {
                Some(call) if matches!(call.name.as_str(), "instance_eval" | "instance_exec") => {
                    let explicit = !matches!(call.recv, RecvShape::Implicit | RecvShape::SelfRecv);
                    let receiver = explicit
                        .then(|| receiver_of(tree, facts, call, path))
                        .flatten();
                    object(receiver)
                }
                _ => DefinedOn::Unknown,
            })
        }
    }
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

/// What a `scope`'s body runs on (DEC-116).
pub(super) const RELATION: &str = "ActiveRecord::Relation";

/// How many calls back a chain is followed. Each step needs a declared
/// return type to continue, so the bound is a guard, not a tuning knob.
const MAX_CHAIN: usize = 4;

/// The ladder, `depth` calls into a chain.
fn typed_at(tree: &Tree, facts: &Facts, call: &Call, path: &str, depth: usize) -> Option<Receiver> {
    let _resolving = ChainMemo::enter();
    ladder(tree, facts, call, path, depth)
}

/// What `assigned_chain` answered for a call, by the call's file, position
/// and depth, within one outermost `typed_at`: a read after N conditional
/// rewrites (`q = q.where(…) if x`) sees every earlier write, and each of
/// those its own, so following them afresh grew with N to the chain's depth.
type ChainAnswers =
    std::collections::HashMap<(usize, String, Pos, usize), Option<(String, bool, &'static str)>>;

thread_local! {
    /// The memo and how many `typed_at` frames share it; the outermost
    /// clears it, since the tree and the files may change between answers.
    static CHAIN_MEMO: std::cell::RefCell<(usize, ChainAnswers)> =
        std::cell::RefCell::new((0, std::collections::HashMap::new()));
}

struct ChainMemo;

impl ChainMemo {
    fn enter() -> Self {
        CHAIN_MEMO.with(|memo| memo.borrow_mut().0 += 1);
        ChainMemo
    }
}

impl Drop for ChainMemo {
    fn drop(&mut self) {
        CHAIN_MEMO.with(|memo| {
            let mut memo = memo.borrow_mut();
            memo.0 -= 1;
            if memo.0 == 0 {
                memo.1.clear();
            }
        });
    }
}

fn ladder(tree: &Tree, facts: &Facts, call: &Call, path: &str, depth: usize) -> Option<Receiver> {
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
                    bound: false,
                    agreeing: 1,
                    total: 1,
                    ambiguous: false,
                    rivals: Vec::new(),
                });
            }
            // A scope's body runs on the model's relation (DEC-116).
            if call.in_scope && tree.is_known(RELATION) && runs_on_a_relation(tree, call, path) {
                return Some(Receiver {
                    fqn: RELATION.to_string(),
                    singleton: false,
                    via: "scope",
                    bound: false,
                    agreeing: 1,
                    total: 1,
                    ambiguous: false,
                    rivals: Vec::new(),
                });
            }
            // An `on_load` block runs on the class that runs the hook (DEC-214).
            if let Some(hooked) = on_load_receiver(tree, facts, call) {
                return Some(hooked);
            }
            // A template runs on its view context (DEC-521).
            if let Some(view) = view_receiver(tree, call, path) {
                return Some(view);
            }
            // `describe` on `main`, which sends it to `RSpec` (DEC-115).
            if call.recv_value == Some(RecvValue::Main) {
                return tree.is_known(rspec::RSPEC).then(|| Receiver {
                    fqn: rspec::RSPEC.to_string(),
                    singleton: true,
                    via: "main",
                    bound: false,
                    agreeing: 1,
                    total: 1,
                    ambiguous: false,
                    rivals: Vec::new(),
                });
            }
            // A call at the top of a file, in no block, runs on `main`, an
            // Object (DEC-445) — in a plain script.
            if call.nesting.is_empty() && call.block_owner.is_none() && tree.is_known("Object") {
                return on_main(tree, facts, call, path);
            }
            let (fqn, singleton) = match made_side(tree, facts, call) {
                Some(Made::Side(side)) if call.singleton => (tree.scope_fqn(&call.nesting)?, side),
                Some(Made::IncludersInstance) => (tree.scope_fqn(&call.nesting[1..])?, false),
                _ => (tree.scope_fqn(&call.nesting)?, call.singleton),
            };
            tree.is_known(&fqn).then_some(Receiver {
                fqn,
                singleton,
                via: "self",
                bound: true,
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
                bound: false,
                agreeing: 1,
                total: 1,
                ambiguous: false,
                rivals: Vec::new(),
            })
        }
        // An assignment first, because it is the more specific evidence; a
        // parameter's declared type is the fallback when there is none.
        RecvShape::Local | RecvShape::Ivar => from_assignments(tree, facts, call, path, depth)
            // A template's `@post` is what its controller's action wrote.
            .or_else(|| match call.recv {
                RecvShape::Ivar => views::view_ivar(tree, call, path),
                _ => None,
            })
            .or_else(|| from_sig_params(tree, facts, call, path))
            // A RABL `node` block's parameter is the template's object.
            .or_else(|| rabl::param_receiver(tree, facts, call, path))
            // Last, because it is the only rung resting on a naming habit
            // rather than on something the code states.
            .or_else(|| from_receiver_name(tree, call, path)),
        RecvShape::Other => chained(tree, facts, call, path, depth),
        // A symbol names the method, never the receiver, so there is nothing
        // here to type. `super` is typed by its own rule, `super_landings`.
        // A RABL `attributes :title` names `title` on the template's object
        // (DEC-526).
        RecvShape::Symbol => rabl::symbol_receiver(tree, facts, call, path),
        RecvShape::Super => None,
    }
}

/// `self` in an `ActiveSupport.on_load(:name)` block, or in a `def` written
/// directly in one (DEC-214): the block is evaluated in each class that runs
/// the hook, so a call directly in it is on that class, and a `def` there
/// defines a method of its instances (DEC-104) — which may be a subclass's.
/// Two classes that run the hook are one's reading and the other's rival.
pub(crate) fn on_load_receiver(tree: &Tree, facts: &Facts, call: &Call) -> Option<Receiver> {
    let (hook, singleton, bound) = match call.block_owner {
        Some(owner) => {
            let hook = facts.hook_blocks.iter().find(|hook| hook.owner == owner)?;
            (hook, true, false)
        }
        None => {
            let def = enclosing_method(facts, call.pos.line)?;
            let hook = facts
                .hook_blocks
                .iter()
                .filter(|hook| hook.lines.0 < def.pos.line && def.end_line <= hook.lines.1)
                .max_by_key(|hook| hook.lines.0)?;
            // Only a `def` whose body is the block's own: one in a class
            // written inside it is that class's.
            let in_class = facts.defs.iter().any(|scope| {
                matches!(
                    scope.kind,
                    crate::core::Kind::Class | crate::core::Kind::Module
                ) && hook.lines.0 < scope.pos.line
                    && scope.pos.line < def.pos.line
                    && def.end_line <= scope.end_line
            });
            if in_class {
                return None;
            }
            (hook, call.singleton, true)
        }
    };
    let bases = tree.hooked(&hook.name);
    let (first, rest) = bases.split_first()?;
    Some(Receiver {
        fqn: first.clone(),
        singleton,
        via: "on_load",
        bound,
        agreeing: 1,
        total: bases.len(),
        ambiguous: !rest.is_empty(),
        rivals: rest.iter().map(|base| (base.clone(), singleton)).collect(),
    })
}

/// Where a call on `self` in a macro's block runs, when not where it is written.
enum Made {
    /// The written scope's class side (`true`) or its instances.
    Side(bool),
    /// An instance of whatever includes the concern whose `ClassMethods`
    /// the block is written in (DEC-390).
    IncludersInstance,
}

/// The side a call on the class runs on when it is written in a block a
/// class-level macro makes a method of (DEC-260): `test "x" do … end`, where
/// `test` hands its `&block` to `define_method`, runs on an instance. A block
/// in between handed to anything but Ruby's ways of changing `self` is taken
/// to yield to it, as a block in a class body is read everywhere else.
fn made_side(tree: &Tree, facts: &Facts, call: &Call) -> Option<Made> {
    let mut current = call;
    // Each step goes to an enclosing block, which is earlier in the file.
    for _ in 0..facts.calls.len() {
        let at = current.block_owner?;
        let owner = facts
            .calls
            .iter()
            .find(|c| c.pos == at && c.recv != RecvShape::Symbol)?;
        if evaluates_its_block(owner) {
            return None;
        }
        match owner.recv {
            RecvShape::Implicit | RecvShape::SelfRecv
                if owner.singleton && runs_its_block_on_an_instance(&owner.name) =>
            {
                return Some(Made::Side(false));
            }
            // A concern's class method runs on its includers' class, so a
            // callback it declares runs on their instances.
            RecvShape::Implicit | RecvShape::SelfRecv
                if in_class_methods(owner) && runs_its_block_on_an_instance(&owner.name) =>
            {
                return Some(Made::IncludersInstance);
            }
            RecvShape::Implicit | RecvShape::SelfRecv if owner.singleton => {
                if let Some(side) = macro_side(tree, owner) {
                    return Some(Made::Side(side));
                }
            }
            RecvShape::Implicit | RecvShape::SelfRecv => return None,
            _ => {}
        }
        current = owner;
    }
    None
}

/// Is `self` for a call on it unsettled by the blocks around it? A block
/// handed to a method that is not Ruby's own may be run on another object —
/// a DSL's `instance_exec` — and no body trekr reads says whether it is
/// (DEC-391). A block whose `self` a rule already places is settled: a Rails
/// callback's, a macro's that makes a method of it, a concern's `included`.
pub(super) fn self_unsettled(tree: &Tree, facts: &Facts, call: &Call, path: &str) -> bool {
    if call.block_owner.is_none() || made_side(tree, facts, call).is_some() {
        return false;
    }
    let mut current = call;
    // Each step goes to an enclosing block, which is earlier in the file.
    for _ in 0..facts.calls.len() {
        let Some(at) = current.block_owner else {
            return false;
        };
        let Some(owner) = facts
            .calls
            .iter()
            .find(|c| c.pos == at && c.recv != RecvShape::Symbol)
        else {
            return true;
        };
        if !keeps_self(tree, facts, owner, path) {
            return true;
        }
        current = owner;
    }
    true
}

/// Does the method a block is handed to run it as it stands? Ruby's own
/// methods do, short of the ones that exist to change `self`, and so does a
/// concern's `included`, which trekr reads as the includer's class body.
fn keeps_self(tree: &Tree, facts: &Facts, owner: &Call, path: &str) -> bool {
    if evaluates_its_block(owner)
        || matches!(
            owner.name.as_str(),
            "define_method" | "define_singleton_method"
        )
    {
        return false;
    }
    let on_self = matches!(owner.recv, RecvShape::Implicit | RecvShape::SelfRecv);
    if on_self && owner.singleton && matches!(owner.name.as_str(), "included" | "prepended") {
        return true;
    }
    let rubys =
        |site: &crate::tree::Site| crate::tree::is_core(&site.path) || tree.in_stdlib(&site.path);
    match receiver_of(tree, facts, owner, path) {
        Some(receiver) => lookup_on(tree, owner, &receiver).is_some_and(|found| rubys(&found.site)),
        // An untyped value's method: an iterator, when Ruby has one by the name.
        None => tree
            .named(&owner.name)
            .iter()
            .any(|found| rubys(&found.site)),
    }
}

/// A call in a method of a concern's `ClassMethods` (or `class_methods do`),
/// the extractor's `instance_side` rule: in its body `self` is the module.
fn in_class_methods(call: &Call) -> bool {
    !call.singleton && call.nesting.len() > 1 && call.nesting[0] == "ClassMethods"
}

/// A class-level Rails macro that `instance_exec`s its block on the
/// instance (DEC-342): a callback — `before_action`, `after_commit`,
/// `around_perform`, a model's own `define_model_callbacks` — `validate`,
/// and `rescue_from`. Its body is ActiveSupport's, which builds the call at
/// runtime, so the macro is known by its name rather than read.
fn runs_its_block_on_an_instance(name: &str) -> bool {
    matches!(name, "validate" | "rescue_from")
        || ["before_", "after_", "around_"]
            .iter()
            .any(|prefix| name.len() > prefix.len() && name.starts_with(prefix))
}

/// The side a block handed to this call on the class runs on, when the
/// method it lands on is a macro that makes a method of the block.
fn macro_side(tree: &Tree, macro_call: &Call) -> Option<bool> {
    let class = tree.scope_fqn(&macro_call.nesting)?;
    let found = tree.lookup(&class, true, &macro_call.name)?;
    // A macro is an instance method run on the class: a module the class
    // extends, or `Module`'s own.
    if found.singleton {
        return None;
    }
    tree.block_side(&found.owner, &macro_call.name)
}

/// A receiver that is a value: a literal is its class, and a call returns
/// what its `sig` says — `x.gsub(a, b).downcase` is a String (DEC-077).
fn chained(tree: &Tree, facts: &Facts, call: &Call, path: &str, depth: usize) -> Option<Receiver> {
    match call.recv_value.as_ref()? {
        RecvValue::Literal(class) => Some(Receiver {
            fqn: tree.resolve(class, &[]).fqn?,
            singleton: false,
            via: "literal",
            bound: false,
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
        RecvValue::Main => None,
        // `is_expected`: the example's `subject`, as if it were written.
        RecvValue::Subject => {
            let subject = Call {
                name: "subject".to_string(),
                recv: RecvShape::Implicit,
                recv_text: None,
                recv_pos: None,
                recv_value: None,
                argc: Some(0),
                block: false,
                stands_for: None,
                ..call.clone()
            };
            let_typed(tree, facts, &subject, path, depth + 1)
        }
    }
}

/// A `let` or `subject` answers with what its block returns, typed as an
/// assignment's value is (DEC-096).
fn let_typed(
    tree: &Tree,
    facts: &Facts,
    call: &Call,
    path: &str,
    depth: usize,
) -> Option<Receiver> {
    if !matches!(call.recv, RecvShape::Implicit | RecvShape::SelfRecv)
        || !rspec::in_group(&call.nesting)
        || !on_the_example(tree, facts, call, path)
    {
        return None;
    }
    let def = match group_member(tree, facts, call) {
        Some(Member::Here(def)) => def,
        None if call.name == "subject" => return implicit_subject(tree, facts, call, path),
        _ => return None,
    };
    let typed = |def: &Def| {
        type_of(
            tree,
            facts,
            def.value.as_ref()?,
            &def.nesting,
            def.pos,
            path,
            depth,
            0,
        )
    };
    let (fqn, singleton, via) = typed(def)?;
    // A hook or a `let` runs in the groups nested in this one too, each with
    // its own `let`s; an example's block runs only here.
    let overrides: Vec<&Def> = if call.in_example {
        Vec::new()
    } else {
        facts
            .defs
            .iter()
            .filter(|other| other.is_group_member() && other.name == call.name)
            .filter(|other| {
                other.nesting.len() > call.nesting.len() && other.nesting.ends_with(&call.nesting)
            })
            .collect()
    };
    let mut rivals: Vec<(String, bool)> = Vec::new();
    let mut agreeing = 1;
    let mut bound = declares_a_bound(via);
    for other in &overrides {
        match typed(other) {
            Some((other, _, via)) if other == fqn => {
                agreeing += 1;
                bound |= declares_a_bound(via);
            }
            Some((other, side, _)) if !rivals.iter().any(|(r, _)| *r == other) => {
                rivals.push((other, side));
            }
            _ => {}
        }
    }
    let total = 1 + overrides.len();
    Some(Receiver {
        fqn,
        singleton,
        via: "let",
        agreeing,
        total,
        ambiguous: agreeing < total,
        rivals,
        bound,
    })
}

/// What `X.new` makes (DEC-133): an instance of `X`, unless the class side
/// of `X` has a `new` of its own that says it returns something else — by a
/// `sig`, or by what it returns. `None` when that is nothing the index can
/// place, or when its paths disagree (DEC-165): `made_by_new_all` has each.
pub(super) fn made_by_new(tree: &Tree, class: &str) -> Option<String> {
    match made_by_new_all(tree, class).as_slice() {
        [only] => Some(only.clone()),
        _ => None,
    }
}

/// Every class `X.new` can make: a `new` that returns `super` makes what the
/// next `new` up the class side makes, down to `Class#new`'s `X` (DEC-165).
/// Empty when a path names a class the index cannot place.
pub(super) fn made_by_new_all(tree: &Tree, class: &str) -> Vec<String> {
    let mut made: Vec<String> = Vec::new();
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
            break;
        };
        let mut follows = false;
        for part in written.split('|') {
            if part == "super" {
                follows = true;
                continue;
            }
            match tree.returned_class(&found, part) {
                Some(other) if !made.contains(&other) => made.push(other),
                Some(_) => {}
                None => return Vec::new(),
            }
        }
        if !follows {
            return made;
        }
        new = tree.after_on_class_side(class, &found, "new");
    }
    if !made.iter().any(|m| m == class) {
        made.insert(0, class.to_string());
    }
    made
}

/// RSpec's implicit `subject`, when no group in reach writes one: an instance
/// of the class the innermost group describes, or the module itself, as
/// `MemoizedHelpers#subject` makes it (DEC-114).
fn implicit_subject(tree: &Tree, facts: &Facts, call: &Call, path: &str) -> Option<Receiver> {
    let described = described_by(tree, facts, &call.nesting, path)?;
    let singleton = tree.kind_of(&described) != Some("class");
    let fqn = match singleton {
        true => described,
        false => made_by_new(tree, &described)?,
    };
    Some(Receiver {
        singleton,
        fqn,
        via: "implicit_subject",
        bound: false,
        agreeing: 1,
        total: 1,
        ambiguous: false,
        rivals: Vec::new(),
    })
}

/// `described_class`, which RSpec answers with the constant the innermost
/// group that names one describes: a call on it is that class's own method
/// (DEC-120). A `let` of the same name is the `let`.
fn described_class(tree: &Tree, facts: &Facts, call: &Call, path: &str) -> Option<Receiver> {
    if call.name != "described_class"
        || call.recv != RecvShape::Implicit
        || call.argc != Some(0)
        || call.block
        || !rspec::in_group(&call.nesting)
        || group_member(tree, facts, call).is_some()
        || !on_the_example(tree, facts, call, path)
    {
        return None;
    }
    Some(Receiver {
        fqn: described_by(tree, facts, &call.nesting, path)?,
        singleton: true,
        via: "described_class",
        bound: false,
        agreeing: 1,
        total: 1,
        ambiguous: false,
        rivals: Vec::new(),
    })
}

/// The class or module the innermost group around `nesting` that describes
/// a constant describes.
fn described_by(tree: &Tree, facts: &Facts, nesting: &[String], path: &str) -> Option<String> {
    let (level, class) = (0..nesting.len()).find_map(|at| {
        let level = &nesting[at..];
        facts
            .described
            .iter()
            .find(|(group, _)| group == level)
            .map(|(_, class)| (level, class))
    })?;
    tree.resolve_at(class, level, path).fqn
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
    // `post.title` in a partial handed `post` (DEC-525).
    if let Some(receiver) = views::partial_local_type(tree, previous, path) {
        return Some(receiver);
    }
    if let Some(receiver) = let_typed(tree, facts, previous, path, depth)
        .or_else(|| described_class(tree, facts, previous, path))
    {
        return Some(receiver);
    }
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
    // `x.class` is the class `x` is an instance of, not `Kernel#class`'s
    // declared `Class`; on `self` a subclass's override still counts
    // (DEC-081). Not in a module: `self.class` there is whichever class
    // includes it, whose own class method wins (DEC-137).
    if previous.name == "class"
        && previous.argc == Some(0)
        && !receiver.singleton
        && tree.kind_of(&receiver.fqn) == Some("class")
    {
        return Some(Receiver {
            singleton: true,
            via: if receiver.via == "self" {
                "self"
            } else {
                "chain"
            },
            ..receiver
        });
    }
    // `Foo.new.bar`, as `x = Foo.new` types `x` — and `self.new` may make a
    // subclass, so whether the type is a bound carries over.
    if previous.name == "new" && receiver.singleton {
        return Some(Receiver {
            fqn: made_by_new(tree, &receiver.fqn)?,
            singleton: false,
            via: "chain",
            ..receiver
        });
    }
    returned_from(tree, receiver, previous)
}

/// What `previous`'s method on `receiver` returns, when a signature says.
fn returned_from(tree: &Tree, receiver: Receiver, previous: &Call) -> Option<Receiver> {
    let method = tree.lookup(&receiver.fqn, receiver.singleton, &previous.name)?;
    let (declarer, returns) = match method.returns_for(previous.argc, previous.block) {
        Some(returns) => (method.clone(), returns.to_string()),
        None => tree.declared_returns(&method, previous.argc, previous.block)?,
    };
    let fqn = tree.returned_class(&declarer, &returns)?;
    // `self` may be a subclass, whose own reader is the one that runs.
    if receiver.via == "self" && overridden_apart(tree, &receiver, &method, &fqn, previous) {
        return None;
    }
    Some(Receiver {
        fqn,
        singleton: false,
        via: "chain",
        rivals: Vec::new(),
        bound: true,
        ..receiver
    })
}

/// Does a class below the receiver override the method a chain steps
/// through, returning something that is not the method's class or below
/// it — or saying nothing? Then the step's type is the base's, not
/// necessarily the object's (DEC-392).
fn overridden_apart(
    tree: &Tree,
    receiver: &Receiver,
    method: &crate::tree::MethodDef,
    returned: &str,
    previous: &Call,
) -> bool {
    tree.named(&previous.name).iter().any(|other| {
        other.singleton == method.singleton
            && other.owner != method.owner
            && tree.inherits(&other.owner, &receiver.fqn)
            && other
                .returns_for(previous.argc, previous.block)
                .and_then(|returns| tree.returned_class(other, returns))
                .is_none_or(|class| class != returned && !tree.inherits(&class, returned))
    })
}

/// A call whose receiver has no type returns what every definition of its
/// name that says so agrees on (`Tree::agreed_return`); one that declares
/// nothing makes the answer `ambiguous`.
fn by_return_types(tree: &Tree, previous: &Call) -> Option<Receiver> {
    // Returns its receiver, which is exactly what is unknown here.
    if crate::core::IDENTITY.contains(&previous.name.as_str()) {
        return None;
    }
    let agreed = tree.agreed_return(&previous.name, previous.argc, previous.block)?;
    Some(Receiver {
        fqn: agreed.fqn.clone(),
        singleton: false,
        via: "chain:name",
        bound: true,
        agreeing: agreed.agreeing,
        total: agreed.total,
        ambiguous: agreed.agreeing < agreed.total,
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
/// 1. the name resolves to a constant the tree actually knows, and not one
///    every object already is — `object` is `Object`, `Kernel` or
///    `BasicObject` only by coincidence of spelling;
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
    // A split name has no chain of its own; the variant this file reaches does
    // (DEC-072), and the corroborations below need one.
    let fqn = tree.variant_at(&tree.resolve_at(&named, &call.nesting, path).fqn?, path);
    // (1) continued: a type every object has narrows nothing (DEC-172).
    if fqn == "Object" || tree.inherits("Object", &fqn) {
        return None;
    }
    // (2) it has to actually answer the call.
    tree.lookup(&fqn, false, &call.name)?;
    // (3) a competing reading in the enclosing scope disqualifies the guess —
    // one the receiver could be, which a private method is not: an explicit
    // receiver cannot call it. Core's `Kernel#autoload?` is no reading of
    // `cref.autoload?` (DEC-240).
    if let Some(scope) = tree.scope_fqn(&call.nesting)
        && scope != fqn
        && tree
            .lookup(&scope, false, &call.name)
            .is_some_and(|method| method.visibility != "private")
    {
        return None;
    }
    let others = tree
        .named(&call.name)
        .iter()
        .filter(|method| method.owner != fqn)
        .map(|method| method.owner.as_str())
        .collect::<std::collections::HashSet<_>>()
        .len();
    Some(Receiver {
        fqn,
        singleton: false,
        via: "receiver_name",
        bound: true,
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
        bound: true,
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
fn from_assignments(
    tree: &Tree,
    facts: &Facts,
    call: &Call,
    path: &str,
    depth: usize,
) -> Option<Receiver> {
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
        // A custom `new` whose paths make different classes is one write
        // with each type, as `rescue A, B => e` is (DEC-165).
        if let ValueShape::New(name) = &assign.value
            && let Some(class) = class_named(tree, name, &assign.nesting)
        {
            let made = made_by_new_all(tree, &class);
            if made.len() > 1 {
                votes.extend(made.into_iter().map(|fqn| (fqn, false, "local:new")));
                continue;
            }
        }
        if let Some(vote) = type_of(
            tree,
            facts,
            &assign.value,
            &assign.nesting,
            assign.pos,
            path,
            depth,
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
    // Any write that may hold a subclass makes the read one that may.
    let bound = votes
        .iter()
        .any(|(f, _, via)| *f == fqn && declares_a_bound(via));
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
        bound,
    })
}

/// The class or module a constant written here names, through a second
/// name for it: `Mutex` is `Thread::Mutex` (DEC-240).
fn class_named(tree: &Tree, name: &str, nesting: &[String]) -> Option<String> {
    tree.namespace_named(&tree.resolve(name, nesting).fqn?)
}

/// Does a value typed this way conform to its type rather than being made
/// as it? A `sig`'s type, a finder's (STI hands back a subclass) and a
/// `rescue`'s are bounds; `X.new`, a literal and a constant are the class.
fn declares_a_bound(via: &str) -> bool {
    matches!(via, "sig" | "sig:step" | "finder" | "local:rescue")
}

/// Every local read in a source → the writes that may have set it.
fn reaching_writes(source: &[u8]) -> std::collections::HashMap<Pos, Vec<Pos>> {
    let vars = crate::resolve::vars::analyze(source);
    let lines = crate::extract::LineIndex::new(source);
    let at = |occurrence: &crate::resolve::vars::Occurrence| lines.pos(occurrence.span.start);
    vars.occurrences
        .iter()
        .filter(|o| o.sigil == crate::resolve::vars::Sigil::Local && o.read)
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
#[allow(clippy::too_many_arguments)]
fn type_of(
    tree: &Tree,
    facts: &Facts,
    value: &ValueShape,
    nesting: &[String],
    // Where the value is written: a local it names was last set before here.
    at: Pos,
    path: &str,
    depth: usize,
    steps: usize,
) -> Option<(String, bool, &'static str)> {
    // `x = y; y = x` is legal Ruby and would otherwise spin.
    if depth > 4 {
        return None;
    }
    match value {
        ValueShape::New(name) => {
            let class = class_named(tree, name, nesting)?;
            Some((made_by_new(tree, &class)?, false, "local:new"))
        }
        ValueShape::Rescued(name) => {
            Some((class_named(tree, name, nesting)?, false, "local:rescue"))
        }
        // `x = Foo` holds the class itself, so `x.bar` is a class method.
        ValueShape::Const(name) => Some((class_named(tree, name, nesting)?, true, "local:const")),
        ValueShape::Same(other) => {
            let next = last_write_before(facts, other, at)?;
            type_of(
                tree,
                facts,
                &next.value,
                &next.nesting,
                next.pos,
                path,
                depth + 1,
                steps,
            )
        }
        // Core knows what an Array is now, so `out = []` types `out`.
        ValueShape::Literal(class) => Some((tree.resolve(class, &[]).fqn?, false, "literal")),
        // One step, and only one: type the receiver from its own assignment,
        // then read the `sig` of the method called on it. Chaining further is
        // what rwr measured drowning.
        ValueShape::LocalCall {
            recv,
            name,
            at: called,
        } => {
            if steps > 0 {
                return None;
            }
            let stepped = || {
                let assign = last_write_before(facts, recv, at)?;
                let (owner, singleton, _) = type_of(
                    tree,
                    facts,
                    &assign.value,
                    &assign.nesting,
                    assign.pos,
                    path,
                    depth + 1,
                    steps + 1,
                )?;
                let method = tree.lookup(&owner, singleton, name)?;
                let returns = method.sig_returns.as_deref()?;
                Some((tree.returned_class(&method, returns)?, false, "sig:step"))
            };
            stepped().or_else(|| assigned_chain(tree, facts, (*called)?, path, depth))
        }
        // A `sig` names a usable class for 64 % of signatures against 3.9 %
        // from syntax alone (PLAN §2) — the highest-yield rung on the ladder.
        ValueShape::SelfCall(name) => {
            // `widget = thing`, where `thing` is a `let` (DEC-096).
            if let Some(member) = visible_member(facts, nesting, name)
                && let Some(value) = &member.value
            {
                return type_of(
                    tree,
                    facts,
                    value,
                    &member.nesting,
                    member.pos,
                    path,
                    depth + 1,
                    steps,
                )
                .map(|(fqn, singleton, _)| (fqn, singleton, "let"));
            }
            let scope = tree.scope_fqn(nesting)?;
            let method = tree.lookup(&scope, false, name)?;
            let returns = method.sig_returns.as_deref()?;
            Some((tree.returned_class(&method, returns)?, false, "sig"))
        }
        ValueShape::ConstCall { recv, name, at } => {
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
            // an array returns an array — so it is tried before what a gem's
            // signature lends (`where`), and it says `finder`, not `sig`.
            if is_finder(name) {
                return Some((owner, false, "finder"));
            }
            assigned_chain(tree, facts, (*at)?, path, depth)
        }
        ValueShape::Chain(at) => assigned_chain(tree, facts, *at, path, depth),
        ValueShape::Other => None,
    }
}

/// What the call at `at` returns, read as a chain reads it — the receiver
/// typed, then the method's signature (DEC-444) — for a variable it is
/// assigned to.
fn assigned_chain(
    tree: &Tree,
    facts: &Facts,
    at: Pos,
    path: &str,
    depth: usize,
) -> Option<(String, bool, &'static str)> {
    if depth >= MAX_CHAIN {
        return None;
    }
    let key = (
        std::ptr::from_ref(facts) as usize,
        path.to_string(),
        at,
        depth,
    );
    if let Some(known) = CHAIN_MEMO.with(|memo| memo.borrow().1.get(&key).cloned()) {
        return known;
    }
    let answer = (|| {
        let call = facts
            .calls
            .iter()
            .find(|c| c.pos == at && c.recv != RecvShape::Symbol)?;
        let receiver = typed_at(tree, facts, call, path, depth + 1)?;
        let returned = returned_from(tree, receiver, call)?;
        Some((returned.fqn, false, "sig"))
    })();
    CHAIN_MEMO.with(|memo| memo.borrow_mut().1.insert(key, answer.clone()));
    answer
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
pub(crate) fn share(agreeing: usize, total: usize) -> f64 {
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
        .iter()
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

/// A receiver whose writes disagree, when the type they most agree on lacks
/// the name and another has it: that one, still ambiguous, since the value
/// may be either (DEC-165).
pub(super) fn rival_with(tree: &Tree, call: &Call, receiver: Receiver) -> Receiver {
    if receiver.rivals.is_empty() || lookup_on(tree, call, &receiver).is_some() {
        return receiver;
    }
    let Some(at) = receiver
        .rivals
        .iter()
        .position(|(fqn, singleton)| tree.lookup(fqn, *singleton, &call.name).is_some())
    else {
        return receiver;
    };
    let mut rivals = receiver.rivals.clone();
    let (fqn, singleton) = rivals.remove(at);
    rivals.insert(0, (receiver.fqn.clone(), receiver.singleton));
    Receiver {
        fqn,
        singleton,
        agreeing: 1,
        ambiguous: true,
        rivals,
        ..receiver
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
            why: match receiver.via {
                "on_load" => "another class that runs the hook",
                "view" | "rabl" => "another controller that may render the template exposes it",
                _ => "another write the receiver's read can see gives it this type",
            },
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
    let mut ranked: Vec<(u8, bool, i32, bool, Candidate)> = tree
        .named(&call.name)
        .iter()
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
                // Last, among equals: a declaration says the code is elsewhere
                // — `Querying`'s `delegate :order, to: :all` sends to
                // `QueryMethods#order`, which is also a candidate.
                method.kind() == Kind::Declaration,
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
    ranked.sort_by_key(|(tier, from_gem, affinity, declared, _)| {
        (*tier, *from_gem, *affinity, *declared)
    });

    let total = ranked.len();
    let candidates: Vec<Candidate> = ranked
        .into_iter()
        .take(MAX_CANDIDATES)
        .map(|(_, _, _, _, c)| c)
        .collect();
    let reason = if total > candidates.len() {
        format!(
            "{reason}; showing {} of {total} definitions",
            candidates.len()
        )
    } else {
        reason.to_string()
    };

    let typed = receiver.as_ref().is_some_and(|r| r.via != "self");
    let (confidence, agreement) = residue_confidence(call, total, typed);
    MethodAnswer {
        status: Status::Residue,
        confidence,
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
        agreement,
        unresolved_ancestors: truncated,
        candidates,
        reason: Some(reason),
    }
}

/// What a call at the top of a file runs on. `main`, an Object, in a plain
/// script; in a Rake file `main` extends `Rake::DSL`, which answers first.
/// A file evaluated on another object (`instance_eval` of a Gemfile, a
/// `config.ru`, a plugin's `plugin.rb`) is told by its name, or by a
/// top-level call `main` does not answer: then `self` is not stated, and the
/// call is left untyped (DEC-445).
fn on_main(tree: &Tree, facts: &Facts, call: &Call, path: &str) -> Option<Receiver> {
    let main = |fqn: &str| Receiver {
        fqn: fqn.to_string(),
        singleton: false,
        via: "main",
        bound: false,
        agreeing: 1,
        total: 1,
        ambiguous: false,
        rivals: Vec::new(),
    };
    let rake = is_rake_file(path) && tree.is_known(RAKE_DSL);
    if rake && tree.lookup(RAKE_DSL, false, &call.name).is_some() {
        return Some(main(RAKE_DSL));
    }
    if evaluated_elsewhere(path) {
        return None;
    }
    let answers = |name: &str| {
        MAIN_OWN.contains(&name)
            || !top_level_defs(tree, name).is_empty()
            || tree.lookup("Object", false, name).is_some()
            || rake && tree.lookup(RAKE_DSL, false, name).is_some()
    };
    // A top-level `def`'s body is not the file's: it runs on whatever calls it.
    let in_a_def = |line: u32| {
        facts.defs.iter().any(|def| {
            def.kind == crate::core::Kind::Method
                && def.nesting.is_empty()
                && (def.pos.line..=def.end_line).contains(&line)
        })
    };
    let foreign = facts.calls.iter().any(|other| {
        other.nesting.is_empty()
            && other.block_owner.is_none()
            && !in_a_def(other.pos.line)
            && other.recv == RecvShape::Implicit
            && other.recv_value.is_none()
            && !answers(&other.name)
    });
    (!foreign).then(|| main("Object"))
}

/// `self` in a view template: an instance of the class Rails compiles it
/// into, a subclass of `ActionView::Base` with the controller's helpers
/// (DEC-521). Every block in a template keeps it — `form_with` and `each`
/// yield values, not a new `self` — so only a class or module the template
/// writes is another scope. A name a `helper_method` exposes is sent to the
/// controller that renders the template, which is then the receiver.
fn view_receiver(tree: &Tree, call: &Call, path: &str) -> Option<Receiver> {
    if !call.nesting.is_empty() {
        return None;
    }
    let (fqn, via) = match crate::tree::views::ViewTemplate::of(path)? {
        crate::tree::views::ViewTemplate::Erb => (crate::tree::views::ACTION_VIEW, "view"),
        crate::tree::views::ViewTemplate::Rabl => (rabl::ENGINE, "rabl"),
    };
    let engine_has_it = via == "rabl" && tree.lookup(rabl::ENGINE, false, &call.name).is_some();
    if !engine_has_it && let Some(controller) = views::exposed_receiver(tree, &call.name, path, via)
    {
        return Some(controller);
    }
    Some(Receiver {
        fqn: fqn.to_string(),
        singleton: false,
        via,
        bound: false,
        agreeing: 1,
        total: 1,
        ambiguous: false,
        rivals: Vec::new(),
    })
}

/// The methods `main` has of its own, on its singleton, which no class
/// declares: `private def x` at the top of a file is a plain script's.
const MAIN_OWN: [&str; 5] = ["public", "private", "include", "using", "define_method"];

/// What Rake extends `main` with before it loads a Rakefile or a `.rake`.
const RAKE_DSL: &str = "Rake::DSL";

fn is_rake_file(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path);
    file.ends_with(".rake") || matches!(file, "Rakefile" | "rakefile" | "Rakefile.rb")
}

/// A file a library reads and evaluates on an object of its own, whose
/// methods may share names with Kernel's (`gem` in a Gemfile is Bundler's):
/// a Gemfile, a Rack `config.ru`, a Jbuilder view.
fn evaluated_elsewhere(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path);
    matches!(file, "Gemfile" | "gems.rb")
        || [".gemfile", ".ru", ".jbuilder"]
            .iter()
            .any(|ext| file.ends_with(ext))
}

/// The `def`s written at the top of a file, outside any class and any
/// block: Object's. A gem's counts — fabrication's `Fabricator` is how every
/// fabricator file starts — but not one in a block, which is whatever the
/// block runs on, once it runs (thor's `instance_eval do def task`, DEC-445).
fn top_level_defs(tree: &Tree, name: &str) -> Vec<crate::tree::MethodDef> {
    tree.named(name)
        .iter()
        .filter(|method| method.owner.is_empty() && !method.singleton)
        .filter(|method| method.via.as_deref() != Some(crate::core::TOP_LEVEL_BLOCK))
        .cloned()
        .collect()
}

/// At most this many definitions of a name is "few" (DEC-442).
const FEW_DEFINITIONS: usize = 3;

/// How often a residue's first candidate is the method Ruby ran, given what
/// it rests on — counted on the gold sets (DEC-442), not chosen. A call on
/// `self` is ranked by its own class's ancestors and namespace, and a name
/// with few definitions leaves little to choose between; either is right
/// about seven times in ten. A name many classes define, called on a
/// receiver nothing typed, about once in six.
///
/// A receiver whose type is known and lacks the name leaves a guess among
/// other classes: 3 of 13 such residues ran the first candidate (DEC-442
/// addendum).
fn residue_confidence(call: &Call, definitions: usize, typed: bool) -> (f64, Option<String>) {
    let on_self = matches!(call.recv, RecvShape::Implicit | RecvShape::SelfRecv);
    let shared = match definitions {
        1 => "1 definition has the name".to_string(),
        n => format!("{n} definitions share the name"),
    };
    match definitions {
        0 => (0.0, None),
        _ if typed && !on_self => (
            0.2,
            Some(format!(
                "the receiver's type is known and lacks the name; {shared}; 0.2 of such \
                 residues ran the first candidate, on the gold sets"
            )),
        ),
        _ if on_self => (
            0.7,
            Some(format!(
                "called on self, so ranked by its own class; {shared}; 0.7 of such \
                 residues ran the first candidate, on the gold sets"
            )),
        ),
        n if n <= FEW_DEFINITIONS => (
            0.7,
            Some(format!(
                "{shared}; 0.7 of residues with at most {FEW_DEFINITIONS} ran the first \
                 candidate, on the gold sets"
            )),
        ),
        _ => (
            0.2,
            Some(format!(
                "{shared}; 0.2 of residues with more than {FEW_DEFINITIONS}, on an untyped \
                 receiver, ran the first candidate, on the gold sets"
            )),
        ),
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
            let path = &found.sites[0].path;
            assert!(
                crate::tree::is_core(path) && path.ends_with("/Kernel.rb"),
                "{path}"
            );
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
    fn a_top_level_call_runs_on_main() {
        // `self` at the top level is `main`, an Object, and a top-level `def`
        // is Object's (DEC-445).
        let source = "def helper\nend\nhelper\n";
        let found = answer(source, "helper");
        assert_eq!(found.status, Status::Resolved);
        assert_eq!(found.owner.as_deref(), Some("Object"));
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
            tree.lookup("Post", false, "body_changed?").is_some(),
            "and the dirty tracking code calls (DEC-111)"
        );
        assert!(
            tree.lookup("Post", false, "restore_body!").is_none(),
            "but not the family's uncalled rest"
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
    fn an_enum_with_a_prefix_spells_the_prefixed_name() {
        for source in [
            "class Post\n  enum status: { draft: 0 }, _prefix: true\nend\n",
            "class Post\n  enum :status, { draft: 0 }, prefix: true\nend\n",
        ] {
            let tree = crate::tree::for_test(&[("a.rb", source)]);
            assert!(tree.lookup("Post", false, "draft?").is_none(), "{source}");
            assert!(
                tree.lookup("Post", false, "status_draft?").is_some(),
                "{source}"
            );
        }
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
    fn a_second_step_is_followed_as_a_chain_is() {
        // Each step rests on a signature, as a chain written out does
        // (DEC-444); what rwr found drowning was chasing steps none states.
        let source = "class Deep\n  def touch\n  end\nend\n\
                      class Leaf\n  sig { returns(Deep) }\n  def deep\n  end\nend\n\
                      class Box\n  sig { returns(Leaf) }\n  def leaf\n  end\nend\n\
                      class W\n  def go\n    b = Box.new\n    l = b.leaf\n    \
                      d = l.deep\n    d.touch\n  end\nend\n";
        assert_eq!(answer(source, "touch").owner.as_deref(), Some("Deep"));
    }

    /// Each conditional write can see every write before it, so following
    /// each one's chain afresh grew with the fourth power of the writes.
    #[test]
    fn conditional_rewrites_of_a_chain_are_typed_in_time() {
        let writes = "    q = q.narrow(1) if c\n".repeat(60);
        let source = format!(
            "class Query\n  sig {{ returns(Query) }}\n  def narrow(x)\n  end\n  \
             def done\n  end\nend\n\
             class W\n  def go(c)\n    q = Query.new\n{writes}    q.done\n  end\nend\n"
        );
        let started = std::time::Instant::now();
        assert_eq!(answer(&source, "done").owner.as_deref(), Some("Query"));
        let took = started.elapsed();
        assert!(took.as_secs() < 5, "took {took:?}");
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
        // Defined elsewhere, so the name exists and the module is the story.
        let source = "module Lonely\n  def run\n    nowhere\n  end\nend\n\
                      class Other\n  def nowhere\n  end\nend\n";
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
    fn a_residues_confidence_is_graded_by_what_backs_it() {
        let defs = |n: usize| -> String {
            (0..n)
                .map(|i| format!("class K{i}\n  def save\n  end\nend\n"))
                .collect()
        };
        let untyped =
            |n: usize| format!("{}class W\n  def go\n    thing.save\n  end\nend\n", defs(n));
        let few = answer(&untyped(3), "save");
        let many = answer(&untyped(4), "save");
        let on_self = answer(
            &format!("{}class W\n  def go\n    save\n  end\nend\n", defs(4)),
            "save",
        );
        let none = answer("class W\n  def go\n    thing.save\n  end\nend\n", "save");
        assert_eq!(
            [
                few.confidence,
                many.confidence,
                on_self.confidence,
                none.confidence
            ],
            [0.7, 0.2, 0.7, 0.0]
        );
        assert!(
            many.agreement
                .unwrap()
                .contains("4 definitions share the name")
        );
    }

    #[test]
    fn a_known_receiver_with_no_such_method_says_so_differently() {
        // Another class has it, so the name exists and Box's chain lacks it;
        // a name defined nowhere says that instead (testbed 100).
        let source = "class Box\nend\nclass Crate\n  def missing\n  end\nend\n\
                      class W\n  def go\n    b = Box.new\n    b.missing\n  end\nend\n";
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
