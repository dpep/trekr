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

use crate::core::{Assign, Call, Facts, RecvShape, ValueShape};
use crate::tree::{Site, Status, Tree};
use serde::Serialize;

/// How the receiver's type was established, and how strongly.
struct Receiver {
    fqn: String,
    /// A class-method lookup rather than an instance-method one.
    singleton: bool,
    via: &'static str,
    /// Assignments that agreed on this type, out of those considered. For the
    /// rungs that are a language rule rather than an inference, both are 1.
    agreeing: usize,
    total: usize,
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
                Some(found) => MethodAnswer {
                    status: Status::Resolved,
                    confidence: receiver.agreeing as f64 / receiver.total as f64,
                    resolved_via: Some(receiver.via.to_string()),
                    receiver: shape,
                    receiver_kind: tree.kind_of(&receiver.fqn).map(str::to_string),
                    receiver_type: Some(receiver.fqn.clone()),
                    owner: Some(found.owner.clone()),
                    sites: vec![found.site.clone()],
                    agreement: agreement(&receiver),
                    unresolved_ancestors: Vec::new(),
                    candidates: Vec::new(),
                    reason: None,
                },
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
        confidence: agreeing as f64 / includers.len() as f64,
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
fn receiver_of(tree: &Tree, facts: &Facts, call: &Call) -> Option<Receiver> {
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
            })
        }
        // An assignment first, because it is the more specific evidence; a
        // parameter's declared type is the fallback when there is none.
        RecvShape::Local | RecvShape::Ivar => {
            from_assignments(tree, facts, call).or_else(|| from_sig_params(tree, facts, call))
        }
        RecvShape::Other => None,
    }
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
            let returns = tree.lookup(&owner, true, name)?.sig_returns.clone()?;
            Some((tree.resolve(&returns, nesting).fqn?, false, "sig"))
        }
        ValueShape::Other => None,
    }
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

    let mut ranked: Vec<(u8, Candidate)> = tree
        .named(&call.name)
        .into_iter()
        .map(|method| {
            let fits = method.accepts(call.argc);
            // Named tiers, not weights: every one of these is a fact a reader
            // can check, and none of them is a constant somebody invented.
            let (tier, why) = match (fits, &here) {
                (true, _) if ancestors.contains(&method.owner) => (
                    0,
                    "arity fits, and the enclosing class inherits from its owner",
                ),
                (true, Some(scope)) if shares_namespace(scope, &method.owner) => (
                    1,
                    "arity fits, and its owner shares a namespace with the call",
                ),
                (true, _) if method.site.path == path => (2, "arity fits, same file"),
                (true, _) => (3, "arity fits"),
                (false, _) => (4, "defined elsewhere; arity does not fit"),
            };
            (
                tier,
                Candidate {
                    owner: method.owner.clone(),
                    singleton: method.singleton,
                    why,
                    site: method.site.clone(),
                },
            )
        })
        .collect();
    ranked.sort_by_key(|(tier, _)| *tier);

    let total = ranked.len();
    let candidates: Vec<Candidate> = ranked
        .into_iter()
        .take(MAX_CANDIDATES)
        .map(|(_, c)| c)
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
    fn answer(source: &str, name: &str) -> MethodAnswer {
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
