//! Who reads an example group's own method — a `let`, a `subject`, a `def`
//! in its body. `--refs`, `--dead` and the editor's references all ask
//! [`reads`], so they cannot disagree (DEC-490).
//!
//! A group is a class only its file sees (DEC-084), so a member of one is
//! read from that file, from the bodies of the shared groups included where
//! it is visible, and from the helpers every group mixes in. A top-level
//! shared group's member is its module's (DEC-092), read from the groups that
//! include it, wherever they are written.
//!
//! Which call reads it is Ruby's lookup at runtime: the innermost group with
//! the name wins, and a hook, a `let` or a `def` runs in every group nested in
//! its own, where a nested group's override answers instead (DEC-096).

use super::refs::{Counts, Reference, Ruling, Tier};
use super::{Member, member_at};
use crate::core::{Call, Def, Facts, Kind, RecvShape, rspec};
use crate::tree::Tree;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

/// The checkout's files, as the caller holds them: the CLI reads them from
/// disk, the editor prefers its open buffers.
pub(crate) trait Files {
    /// A file's facts, by its checkout-relative path.
    fn facts(&self, path: &str) -> Option<Arc<Facts>>;
    /// The checkout-relative files that call `name`, as the index lists them.
    fn calling(&self, name: &str) -> Vec<String>;
}

/// A group member: a `let`, `let!`, `subject` or `def` written in an example
/// group's body, or in a top-level shared group's.
pub(crate) fn is_member(def: &Def) -> bool {
    def.kind == Kind::Method
        && !def.singleton
        && (def.is_group_member() || rspec::is_shared_member(def))
}

/// `subject(:x)` defines `x`, and `subject` as another name for it: this is
/// that `subject`, which a question about `x` already answers.
pub(crate) fn names_a_named_subject(def: &Def, facts: &Facts) -> bool {
    def.name == "subject"
        && facts.defs.iter().any(|other| {
            other.name != "subject"
                && other.via == def.via
                && other.nesting == def.nesting
                && other.end_line == def.end_line
                && other.pos.line <= def.pos.line
        })
}

/// Calls that read the `subject` without naming it: `is_expected`, a bare
/// `should`, and `its`, whose group's subject is an attribute of it.
const SUBJECT_READS: [&str; 4] = ["is_expected", "should", "should_not", "its"];

/// Calls that send a name they are handed.
const SENDS: [&str; 9] = [
    "send",
    "public_send",
    "__send__",
    "try",
    "try!",
    "respond_to?",
    "method",
    "public_method",
    "instance_variable_get",
];

/// Calls that include a shared group by its name.
pub(crate) const SHARED_INCLUDERS: [&str; 4] = [
    "include_context",
    "include_examples",
    "it_behaves_like",
    "it_should_behave_like",
];

/// RSpec's own group-body macros, known without rspec-core indexed: none
/// reads a member by name.
const DSL: [&str; 58] = [
    "describe",
    "context",
    "feature",
    "example_group",
    "xdescribe",
    "xcontext",
    "xfeature",
    "fdescribe",
    "fcontext",
    "ffeature",
    "shared_examples",
    "shared_context",
    "shared_examples_for",
    "it",
    "specify",
    "example",
    "scenario",
    "focus",
    "fit",
    "fspecify",
    "fexample",
    "fscenario",
    "xit",
    "xspecify",
    "xexample",
    "xscenario",
    "skip",
    "pending",
    "before",
    "after",
    "around",
    "prepend_before",
    "append_before",
    "prepend_after",
    "append_after",
    "let",
    "let!",
    "subject",
    "subject!",
    "its",
    "include_context",
    "include_examples",
    "it_behaves_like",
    "it_should_behave_like",
    "include",
    "extend",
    "prepend",
    "private",
    "protected",
    "public",
    "attr_reader",
    "attr_writer",
    "attr_accessor",
    "alias_method",
    "define_method",
    "described_class",
    "metadata",
    "render_views",
];

/// The member asked about, and the file that writes it.
pub(crate) struct Asked<'a> {
    pub(crate) path: &'a str,
    pub(crate) def: &'a Def,
}

impl Asked<'_> {
    /// The names a call reads it by: `subject(:x)` is `x` and `subject`.
    fn names(&self) -> Vec<&str> {
        let mut names = vec![self.def.name.as_str()];
        if self.is_subject() && self.def.name != "subject" {
            names.push("subject");
        }
        names
    }

    fn is_subject(&self) -> bool {
        self.def
            .via
            .as_deref()
            .is_some_and(|via| via.starts_with("subject"))
    }

    /// The module a top-level shared group's member belongs to.
    pub(crate) fn module(&self) -> Option<&str> {
        rspec::is_shared_member(self.def).then(|| self.def.nesting[0].trim_start_matches("::"))
    }

    /// The owner an answer names: the shared group's module, or the group's
    /// class as RSpec names it.
    pub(crate) fn owner(&self) -> String {
        match self.module() {
            Some(module) => module.to_string(),
            None => rspec::class_name(&self.def.nesting),
        }
    }

    /// Is this what the lookup at `path` found?
    fn is(&self, path: &str, found: &Member<'_>) -> bool {
        let span = self.def.pos.line..=self.def.end_line;
        match found {
            Member::Here(def) => {
                path == self.path
                    && (def.pos == self.def.pos
                        || (self.is_subject()
                            && def.via == self.def.via
                            && def.nesting == self.def.nesting
                            && def.end_line == self.def.end_line
                            && span.contains(&def.pos.line)))
            }
            Member::Shared(method) => {
                self.module() == Some(method.owner.as_str())
                    && span.contains(&method.site.line)
                    && crate::core::paths::names_file(&method.site.path, self.path)
            }
        }
    }
}

/// What reads a member, and what was looked at to say so.
#[derive(Default)]
pub(crate) struct Reads {
    /// Every read found, ordered as `--refs` lists them; the excluded ones
    /// only when asked for.
    pub(crate) found: Vec<Reference>,
    pub(crate) counts: Counts,
    /// The overriding definitions that answer a call of its name in its
    /// reach instead of it, as `path:line`.
    pub(crate) overridden_by: Vec<String>,
    /// What may read it unseen: a name sent at runtime, a macro not read.
    pub(crate) caveats: Vec<String>,
    /// How many shared groups' bodies were read for it.
    pub(crate) shared_groups: usize,
    /// How many helper modules were read for it.
    pub(crate) helpers: usize,
}

impl Reads {
    fn caveat(&mut self, why: String) {
        if !self.caveats.contains(&why) {
            self.caveats.push(why);
        }
    }
}

/// The member a call names, when a group's lookup finds one (DEC-084):
/// written in this file, or in a shared group's module, by where its module
/// writes it.
pub(crate) enum Named {
    Here(Box<Def>),
    Shared { site: String, line: u32 },
}

pub(crate) fn named_by(tree: &Tree, facts: &Facts, call: &Call) -> Option<Named> {
    match super::group_member(tree, facts, call)? {
        Member::Here(def) => Some(Named::Here(Box::new(def.clone()))),
        Member::Shared(found) => Some(Named::Shared {
            site: found.site.path.clone(),
            line: found.site.line,
        }),
    }
}

/// What a call in a shared group's body reads when the body does not define
/// the name: the member each group that includes the body defines, where
/// the call runs (DEC-490). A top-level shared group's includers are the
/// files that call the name — `let(:name)` is a call of it — and one written
/// in a group, its own file's.
pub(crate) fn includer_members(
    context: &Context<'_>,
    path: &str,
    facts: &Facts,
    call: &Call,
) -> Vec<(String, Def)> {
    let tree = context.tree;
    if !matches!(call.recv, RecvShape::Implicit | RecvShape::SelfRecv)
        || !rspec::in_group(&call.nesting)
    {
        return Vec::new();
    }
    let top = call
        .nesting
        .last()
        .and_then(|segment| rspec::shared_module_of(segment));
    let local = facts
        .local_shared
        .iter()
        .filter(|local| call.nesting.ends_with(&local.body))
        .max_by_key(|local| local.body.len());
    let (module, body) = match (top, local) {
        (_, Some(local)) => (local.module.clone(), Some(&local.body)),
        (Some(module), None) => (module, None),
        (None, None) => return Vec::new(),
    };
    match member_at(tree, facts, &call.nesting, &call.name, false) {
        Some(Member::Here(def)) if body.is_none_or(|body| def.nesting.ends_with(body)) => {
            return Vec::new();
        }
        Some(Member::Shared(found)) if found.owner == module => return Vec::new(),
        _ => {}
    }
    let mut paths = vec![path.to_string()];
    if body.is_none() {
        paths.extend(
            context
                .files
                .calling(&call.name)
                .into_iter()
                .filter(|p| p != path),
        );
    }
    let hook = !call.in_example;
    let mut found: Vec<(String, Def)> = Vec::new();
    for includer in paths {
        let Some(held) = (match includer == path {
            true => context.files.facts(path),
            false => context.files.facts(&includer),
        }) else {
            continue;
        };
        let scan = Scan::new(tree, &includer, &held, None);
        for (level, _, _) in includes(&held).filter(|(_, included, _)| **included == module) {
            let mut answers = scan.answers(tree, level, &call.name, hook);
            if hook {
                answers.extend(member_at(tree, &held, level, &call.name, false));
            }
            for answer in answers {
                let Member::Here(def) = answer else { continue };
                // Its own body's, read lexically from an includer in its file.
                if body.is_some_and(|body| def.nesting.ends_with(body)) {
                    continue;
                }
                if !found
                    .iter()
                    .any(|(p, d)| *p == includer && d.pos == def.pos)
                {
                    found.push((includer.clone(), def.clone()));
                }
            }
        }
    }
    found
}

/// What every member's question shares: the files, and what each module
/// whose methods run on an example calls, read once.
pub(crate) struct Context<'a> {
    tree: &'a Tree,
    files: &'a dyn Files,
    /// The modules mixed into every example group.
    helpers: Vec<String>,
    modules: RefCell<HashMap<String, Arc<ModuleCalls>>>,
}

/// What a module's methods call with no receiver, by name, and where they
/// send a name they compute.
#[derive(Default)]
struct ModuleCalls {
    calls: HashMap<String, Vec<(String, crate::core::Pos)>>,
    sends: Vec<(String, u32, String)>,
}

impl<'a> Context<'a> {
    pub(crate) fn new(tree: &'a Tree, files: &'a dyn Files) -> Context<'a> {
        Context {
            tree,
            files,
            helpers: helper_modules(tree),
            modules: RefCell::new(HashMap::new()),
        }
    }

    fn module_calls(&self, module: &str) -> Arc<ModuleCalls> {
        if let Some(held) = self.modules.borrow().get(module) {
            return held.clone();
        }
        let mut found = ModuleCalls::default();
        for path in module_paths(self.tree, module) {
            let Some(facts) = self.files.facts(&path) else {
                continue;
            };
            let in_module =
                |call: &Call| self.tree.scope_fqn(&call.nesting).as_deref() == Some(module);
            for call in facts.calls.iter().filter(|call| {
                matches!(call.recv, RecvShape::Implicit | RecvShape::SelfRecv) && in_module(call)
            }) {
                found
                    .calls
                    .entry(call.name.clone())
                    .or_default()
                    .push((path.clone(), call.pos));
            }
            // A gem's generic `send(name)` is its own business: only the
            // checkout's helpers are read for one that may be a member's.
            if !path.starts_with('/') {
                for (line, shape) in computed_sends(&facts, in_module) {
                    found.sends.push((path.clone(), line, shape));
                }
            }
        }
        let found = Arc::new(found);
        self.modules
            .borrow_mut()
            .insert(module.to_string(), found.clone());
        found
    }
}

/// The modules `RSpec.configure` mixes into every example group (DEC-088),
/// on either side, the checkout's and the gems': their methods run on a
/// group or its examples, and the metadata filter that may narrow them is
/// not read.
fn helper_modules(tree: &Tree) -> Vec<String> {
    let mut modules: Vec<String> = Vec::new();
    for singleton in [false, true] {
        for (owner, _) in tree.lookup_chain(rspec::EXAMPLE_GROUP, singleton) {
            // rspec-core's own reads of the `subject` are `is_expected` and
            // `should`, which are read where they are written.
            if !modules.contains(&owner)
                && !owner.starts_with("RSpec::Core::")
                && tree.kind_of(&owner) == Some("module")
                && !module_paths(tree, &owner).is_empty()
            {
                modules.push(owner);
            }
        }
    }
    modules
}

/// The files that write `fqn`: checkout-relative for the checkout's, whole
/// for a gem's; never Ruby core's stubs.
fn module_paths(tree: &Tree, fqn: &str) -> Vec<String> {
    let root = tree.site_path("");
    let mut paths: Vec<String> = tree
        .sites(fqn)
        .into_iter()
        .filter(|site| !crate::tree::is_core(&site.path))
        .map(|site| match tree.in_checkout(&site.path) {
            true => site
                .path
                .strip_prefix(&root)
                .map_or(site.path.clone(), str::to_string),
            false => site.path,
        })
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

/// The checkout-relative files that write `fqn`.
fn checkout_paths(tree: &Tree, fqn: &str) -> Vec<String> {
    let root = tree.site_path("");
    let mut paths: Vec<String> = tree
        .sites(fqn)
        .into_iter()
        .filter(|site| tree.in_checkout(&site.path))
        .filter_map(|site| site.path.strip_prefix(&root).map(str::to_string))
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

/// Calls that write an example into the group they are written in.
const RUNS: [&str; 10] = [
    "it",
    "specify",
    "example",
    "scenario",
    "its",
    "focus",
    "fit",
    "fspecify",
    "fexample",
    "fscenario",
];

/// One file a member may be read in, with what is asked of it.
struct Scan<'f> {
    path: &'f str,
    facts: &'f Facts,
    /// The groups whose examples run: each writes one, includes a shared
    /// group that may, or calls a macro trekr does not read.
    running: Vec<&'f [String]>,
    /// `it_behaves_like` with no block: a group of its own, nested where it
    /// is written, with the shared group's module included.
    nested: Vec<(&'f [String], &'f str)>,
    /// The shared group the member is written in, if it is.
    of: Option<&'f SharedOf>,
}

impl<'f> Scan<'f> {
    fn new(tree: &Tree, path: &'f str, facts: &'f Facts, of: Option<&'f SharedOf>) -> Scan<'f> {
        let mut running: Vec<&[String]> = Vec::new();
        for call in &facts.calls {
            let name = call.name.as_str();
            if call.recv != RecvShape::Implicit || !rspec::in_group(&call.nesting) {
                continue;
            }
            let adds = RUNS.contains(&name)
                || (call.group_body
                    && !DSL.contains(&name)
                    && !name.starts_with("attr_")
                    && tree.lookup(rspec::EXAMPLE_GROUP, true, name).is_none());
            if adds && !running.contains(&call.nesting.as_slice()) {
                running.push(&call.nesting);
            }
        }
        for (level, _) in &facts.shared_includes {
            if !running.contains(&level.as_slice()) {
                running.push(level);
            }
        }
        let nested = facts
            .nested_includes
            .iter()
            .map(|(level, module)| (level.as_slice(), module.as_str()))
            .collect();
        Scan {
            path,
            facts,
            running,
            nested,
            of,
        }
    }

    /// Does a call of `name` at `nesting` run the member? An example's own
    /// call reads the innermost definition there; a hook's, a `let`'s or a
    /// `def`'s runs for every example of its group and the groups nested in
    /// it, and reads the innermost definition where each runs.
    fn sees(
        &self,
        tree: &Tree,
        asked: &Asked<'_>,
        nesting: &[String],
        name: &str,
        hook: bool,
    ) -> bool {
        self.sees_below(tree, asked, nesting, name, hook, 0)
    }

    /// `sees`, counting only a definition at least `floor` groups deep: one
    /// above it is answered first by what a shared group included there
    /// defines.
    fn sees_below(
        &self,
        tree: &Tree,
        asked: &Asked<'_>,
        nesting: &[String],
        name: &str,
        hook: bool,
        floor: usize,
    ) -> bool {
        self.answers(tree, nesting, name, hook).iter().any(|found| {
            let deep = match found {
                Member::Here(def) => def.nesting.len() >= floor,
                Member::Shared(_) => floor == 0,
            };
            deep && asked.is(self.path, found)
        }) || self.included_sees(asked, nesting, name, hook, floor)
    }

    /// What answers a call of `name` at `nesting`, wherever it runs.
    fn answers(&self, tree: &Tree, nesting: &[String], name: &str, hook: bool) -> Vec<Member<'f>> {
        if !hook {
            return member_at(tree, self.facts, nesting, name, false)
                .into_iter()
                .collect();
        }
        let under = |level: &[String]| level.ends_with(nesting);
        let mut found: Vec<Member<'f>> = self
            .running
            .iter()
            .filter(|level| under(level))
            .filter_map(|level| member_at(tree, self.facts, level, name, false))
            .collect();
        for (level, module) in self.nested.iter().filter(|(level, _)| under(level)) {
            let own = tree
                .lookup(module, false, name)
                .map(|method| Member::Shared(Box::new(method)));
            found.extend(own.or_else(|| member_at(tree, self.facts, level, name, false)));
        }
        found
    }

    /// Where the member is a shared group's — a top-level one's module, or
    /// one written in this file's groups — a group that includes it sees it:
    /// `include_context` in the group itself, unless the group defines the
    /// name too; `it_behaves_like` in a group nested there, which a hook
    /// written here runs in.
    fn included_sees(
        &self,
        asked: &Asked<'_>,
        nesting: &[String],
        name: &str,
        hook: bool,
        floor: usize,
    ) -> bool {
        let Some(of) = self.of else {
            return false;
        };
        if of.local && self.path != asked.path {
            return false;
        }
        // A shared group of this file's that includes the member's copies
        // it too, inside a group of its own.
        let mut within: Vec<&str> = vec![of.module.as_str()];
        if of.local {
            let mut grew = true;
            while grew {
                grew = false;
                for (level, included, _) in includes(self.facts) {
                    if !within.contains(&included.as_str()) {
                        continue;
                    }
                    for outer in &self.facts.local_shared {
                        if level.ends_with(&outer.body) && !within.contains(&outer.module.as_str())
                        {
                            within.push(&outer.module);
                            grew = true;
                        }
                    }
                }
            }
        }
        includes(self.facts)
            .filter(|(level, included, _)| {
                within.contains(&included.as_str()) && level.ends_with(nesting)
            })
            .any(|(level, included, nested)| {
                let under = level.len() > nesting.len();
                // A group the body writes runs only inside the includer's
                // copy of it, which only a hook written here reaches.
                if nested || under || !of.top || *included != of.module {
                    return hook;
                }
                let defined_here = self
                    .facts
                    .defs
                    .iter()
                    .any(|def| is_member(def) && def.name == name && def.nesting == *level);
                !defined_here && floor <= level.len()
            })
    }
}

/// The shared group a member is written in, which a group that includes it
/// copies: its module, whether the member is at the body's top or in a group
/// the body writes, and whether the group is one this file's groups scope.
struct SharedOf {
    module: String,
    top: bool,
    local: bool,
}

/// The module of the shared group a member is written in, if it is one's.
pub(crate) fn shared_group_of(path: &str, def: &Def, own: &Facts) -> Option<String> {
    shared_of(&Asked { path, def }, own).map(|of| of.module)
}

fn shared_of(asked: &Asked<'_>, own: &Facts) -> Option<SharedOf> {
    if let Some(module) = asked.module() {
        return Some(SharedOf {
            module: module.to_string(),
            top: true,
            local: false,
        });
    }
    let nesting = &asked.def.nesting;
    if let Some(module) = nesting
        .last()
        .and_then(|segment| rspec::shared_module_of(segment))
    {
        return Some(SharedOf {
            module,
            top: false,
            local: false,
        });
    }
    own.local_shared
        .iter()
        .filter(|local| nesting.ends_with(&local.body))
        .max_by_key(|local| local.body.len())
        .map(|local| SharedOf {
            module: local.module.clone(),
            top: local.body.len() == nesting.len(),
            local: true,
        })
}

/// Every read of a group member, tiered as `--refs` tiers a method's calls.
pub(crate) fn reads(context: &Context<'_>, asked: &Asked<'_>, keep_all: bool) -> Reads {
    let tree = context.tree;
    let mut out = Reads::default();
    let names = asked.names();
    let reads_subject = names.contains(&"subject");
    let Some(own) = context.files.facts(asked.path) else {
        return out;
    };
    let owner = asked.owner();
    let of = shared_of(asked, &own);

    // A group's member is seen only in its file; a top-level shared group's,
    // in every file that includes the group, which calls its name to read it.
    let mut paths: Vec<String> = vec![asked.path.to_string()];
    if of.as_ref().is_some_and(|of| !of.local) {
        let mut calling: Vec<String> = names
            .iter()
            .copied()
            .chain(SUBJECT_READS.iter().copied().filter(|_| reads_subject))
            .flat_map(|name| context.files.calling(name))
            .filter(|path| path != asked.path)
            .collect();
        calling.sort();
        calling.dedup();
        paths.extend(calling);
    }
    let held: Vec<(String, Arc<Facts>)> = paths
        .into_iter()
        .filter_map(|path| {
            let facts = if path == asked.path {
                own.clone()
            } else {
                context.files.facts(&path)?
            };
            Some((path, facts))
        })
        .collect();

    let push = |out: &mut Reads, reference: Reference| {
        out.counts.record(&reference);
        if keep_all || reference.tier != Tier::Excluded {
            out.found.push(reference);
        }
    };
    let at = |path: &str,
              pos: crate::core::Pos,
              receiver: &'static str,
              nesting: &[String],
              tier: Tier,
              why: &'static str,
              from: &'static str| Reference {
        path: path.to_string(),
        line: pos.line,
        col: pos.col,
        tier,
        receiver,
        receiver_type: rspec::in_group(nesting).then(|| rspec::class_name(nesting)),
        owner: (tier != Tier::Excluded).then(|| owner.clone()),
        why,
        ruling: (tier == Tier::Excluded).then_some(Ruling::DifferentOwner),
        proximity: match tier {
            Tier::Confirmed => 0,
            _ => 1,
        },
        from: Some(from),
    };
    let reference = |path: &str, call: &Call, tier: Tier, why: &'static str, from: &'static str| {
        at(
            path,
            call.pos,
            call.recv.as_str(),
            &call.nesting,
            tier,
            why,
            from,
        )
    };

    // Shared groups included where a read of theirs reaches the member, with
    // the file that includes each.
    let mut shared: Vec<(String, String)> = Vec::new();
    for (path, facts) in &held {
        let scan = Scan::new(tree, path, facts, of.as_ref());
        let mine = path == asked.path;
        for call in &facts.calls {
            let proxy = reads_subject
                && call.recv == RecvShape::Implicit
                && SUBJECT_READS.contains(&call.name.as_str());
            let name = match () {
                _ if proxy => "subject",
                _ if names.contains(&call.name.as_str()) => call.name.as_str(),
                _ => continue,
            };
            match call.recv {
                RecvShape::Symbol => {
                    // The member's own `let(:x)`, and an override's.
                    if facts
                        .defs
                        .iter()
                        .any(|def| def.pos == call.pos && is_member(def))
                    {
                        continue;
                    }
                    let sent = call.stands_for.as_ref().is_some_and(|to| {
                        matches!(to.recv, RecvShape::Implicit | RecvShape::SelfRecv)
                    });
                    if !rspec::in_group(&call.nesting)
                        || !scan.sees(tree, asked, &call.nesting, name, true)
                    {
                        continue;
                    }
                    if sent {
                        push(
                            &mut out,
                            reference(
                                path,
                                call,
                                Tier::Possible,
                                "a symbol sent by name to the example",
                                "symbol",
                            ),
                        );
                    } else if mine {
                        out.caveat(format!(
                            "`:{name}` at line {} is not read as a call",
                            call.pos.line
                        ));
                    }
                }
                RecvShape::Implicit | RecvShape::SelfRecv => {
                    // `its` is written in the body, and runs as an example.
                    let its = proxy && call.name == "its";
                    if !rspec::in_group(&call.nesting) || (call.group_body && !its) {
                        continue;
                    }
                    let hook = !call.in_example && !its;
                    if scan.sees(tree, asked, &call.nesting, name, hook) {
                        let on_example = its || super::on_the_example(tree, facts, call, path);
                        let (tier, why) = match on_example {
                            true => (Tier::Confirmed, "runs on an example that sees it"),
                            false => (
                                Tier::Possible,
                                "in a block handed to a method that may run it as another object",
                            ),
                        };
                        let from = match () {
                            _ if proxy => "subject",
                            _ if !mine => "includer",
                            _ if call.nesting == asked.def.nesting => "group",
                            _ if call.nesting.ends_with(&asked.def.nesting) => "nested_group",
                            _ => "enclosing_group",
                        };
                        push(&mut out, reference(path, call, tier, why, from));
                        continue;
                    }
                    // Its name, answered by another definition wherever the
                    // call runs: an override shadows it there.
                    let found = scan.answers(tree, &call.nesting, name, hook);
                    let overriding: Vec<String> = found
                        .iter()
                        .filter_map(|found| match found {
                            Member::Here(def) if overrides(asked, path, facts, def) => {
                                Some(format!("{path}:{}", def.pos.line))
                            }
                            _ => None,
                        })
                        .collect();
                    let reaches_it = member_at(tree, facts, &call.nesting, name, false)
                        .is_some_and(|found| asked.is(path, &found));
                    if !overriding.is_empty() || reaches_it {
                        for at in overriding {
                            if !out.overridden_by.contains(&at) {
                                out.overridden_by.push(at);
                            }
                        }
                        push(
                            &mut out,
                            reference(
                                path,
                                call,
                                Tier::Excluded,
                                "an override answers it wherever it runs",
                                "nested_group",
                            ),
                        );
                    } else if keep_all && !found.is_empty() {
                        push(
                            &mut out,
                            reference(
                                path,
                                call,
                                Tier::Excluded,
                                "another group's definition answers it",
                                "other_group",
                            ),
                        );
                    }
                }
                _ => {}
            }
        }
        // `super` in an overriding definition calls the one it overrides.
        // A `let`'s block has no method name for the index to record its
        // `super` under, so the block's text is read for one.
        for enclosing in facts.defs.iter().filter(|def| {
            is_member(def) && names.contains(&def.name.as_str()) && def.nesting.len() > 1
        }) {
            let parent = &enclosing.nesting[1..];
            let reaches = member_at(tree, facts, parent, &enclosing.name, false)
                .is_some_and(|found| asked.is(path, &found));
            if !reaches {
                continue;
            }
            if let Some(pos) = super_in(facts, enclosing) {
                let why = "`super` in an overriding definition calls it";
                push(
                    &mut out,
                    at(
                        path,
                        pos,
                        "super",
                        &enclosing.nesting,
                        Tier::Confirmed,
                        why,
                        "super",
                    ),
                );
            }
        }
        for (level, module, _) in includes(facts) {
            let reaches = names
                .iter()
                .any(|name| scan.sees(tree, asked, level, name, true));
            if reaches
                && !shared
                    .iter()
                    .any(|(known, at)| known == module && at == path)
            {
                shared.push((module.clone(), path.clone()));
            }
        }
        if mine {
            reach_caveats(tree, &scan, asked, &names, &mut out);
        }
    }

    // The bodies of the shared groups included where it is visible: a call
    // there that their own definitions do not answer is the includer's.
    let mut bodies: Vec<&str> = Vec::new();
    for (module, includer) in &shared {
        if !bodies.contains(&module.as_str()) {
            bodies.push(module);
        }
        let Some((_, includer_facts)) = held.iter().find(|(path, _)| path == includer) else {
            continue;
        };
        let scan = Scan::new(tree, includer, includer_facts, of.as_ref());
        // A shared group written inside a group is the includer file's own,
        // and RSpec looks it up before the top-level one of its name.
        let local: Vec<&Vec<String>> = includer_facts
            .local_shared
            .iter()
            .filter(|local| local.module == *module)
            .map(|local| &local.body)
            .collect();
        let sources: Vec<BodySource<'_>> = match local.is_empty() {
            true => checkout_paths(tree, module)
                .into_iter()
                .filter_map(|path| Some((path.clone(), context.files.facts(&path)?, None)))
                .collect(),
            false => local
                .into_iter()
                .map(|body| (includer.clone(), includer_facts.clone(), Some(body)))
                .collect(),
        };
        for (path, facts, local) in &sources {
            let in_body = |call: &Call| match local {
                Some(body) => call.nesting.ends_with(body),
                None => call
                    .nesting
                    .last()
                    .and_then(|segment| rspec::shared_module_of(segment))
                    .is_some_and(|of| of == *module),
            };
            for call in &facts.calls {
                let proxy = reads_subject
                    && call.recv == RecvShape::Implicit
                    && SUBJECT_READS.contains(&call.name.as_str());
                let name = match () {
                    _ if proxy => "subject",
                    _ if names.contains(&call.name.as_str()) => call.name.as_str(),
                    _ => continue,
                };
                let its = proxy && call.name == "its";
                if !in_body(call)
                    || !matches!(call.recv, RecvShape::Implicit | RecvShape::SelfRecv)
                    || (call.group_body && !its)
                {
                    continue;
                }
                // The body's own definition: one in a group nested in the
                // body answers there; one at its top answers unless the
                // includer's group defines the name too, which wins.
                let own = match member_at(tree, facts, &call.nesting, name, false) {
                    Some(Member::Here(def)) => match local {
                        Some(body) if def.nesting.ends_with(body) => {
                            Some(def.nesting.len() > body.len())
                        }
                        Some(_) => None,
                        None => Some(true),
                    },
                    Some(Member::Shared(found)) if found.owner == *module => Some(false),
                    _ => None,
                };
                if own == Some(true) {
                    continue;
                }
                let hook = !call.in_example && !its;
                let reached = includes(includer_facts)
                    .filter(|(_, included, _)| *included == module)
                    .any(|(level, _, nested)| {
                        match own {
                            // In `it_behaves_like`'s own group the body's
                            // definition is the nearest.
                            Some(false) if nested => false,
                            Some(false) => {
                                scan.sees_below(tree, asked, level, name, hook, level.len())
                            }
                            _ => scan.sees(tree, asked, level, name, hook),
                        }
                    });
                if reached {
                    push(
                        &mut out,
                        reference(
                            path,
                            call,
                            Tier::Confirmed,
                            "an included shared group's body reads it",
                            "shared_group",
                        ),
                    );
                }
            }
            sends_in(facts, path, &names, in_body)
                .into_iter()
                .for_each(|why| out.caveat(why));
        }
    }
    out.shared_groups = bodies.len();

    // The helpers every group mixes in, and the modules a group in reach
    // includes: a call of its name there may be the member's, in whichever
    // groups the module reaches.
    let mut modules: Vec<(&str, String)> = context
        .helpers
        .iter()
        .map(|module| {
            (
                "a helper every example group mixes in calls it, for the groups it is mixed into",
                module.clone(),
            )
        })
        .collect();
    for module in group_includes(tree, asked, &own) {
        if !modules.iter().any(|(_, known)| *known == module) {
            modules.push(("a module a group in reach includes calls it", module));
        }
    }
    out.helpers = modules.len();
    for (why, module) in &modules {
        let calls = context.module_calls(module);
        for name in &names {
            for (path, pos) in calls.calls.get(*name).into_iter().flatten() {
                push(
                    &mut out,
                    at(path, *pos, "implicit", &[], Tier::Possible, why, "helper"),
                );
            }
        }
        for (path, line, shape) in &calls.sends {
            if names
                .iter()
                .any(|name| crate::core::shape_matches(shape, name))
            {
                out.caveat(format!(
                    "a name computed at runtime is sent at {}:{line}",
                    short(path)
                ));
            }
        }
    }
    out.found.sort_by_key(super::refs::order);
    out
}

/// Where a shared group's body is read: the file, and for one a group of
/// that file writes, the body's own nesting.
type BodySource<'a> = (String, Arc<Facts>, Option<&'a Vec<String>>);

/// Every shared group a file includes, by the group it is included where
/// a lookup from that group finds its members: `include_context` and
/// `include_examples` into the group itself, `it_behaves_like` into a group
/// nested in it, which looks up through it.
fn includes(facts: &Facts) -> impl Iterator<Item = (&Vec<String>, &String, bool)> {
    let same = facts
        .shared_includes
        .iter()
        .map(|(level, module)| (level, module, false));
    let nested = facts
        .nested_includes
        .iter()
        .map(|(level, module)| (level, module, true));
    same.chain(nested)
}

/// Where `def`'s body calls `super`, read from its text.
fn super_in(facts: &Facts, def: &Def) -> Option<crate::core::Pos> {
    let source = facts.source.as_deref()?;
    let lines = source.split(|b| *b == b'\n').enumerate();
    for (at, line) in lines.skip(def.pos.line as usize - 1) {
        let number = at as u32 + 1;
        if number > def.end_line {
            break;
        }
        let text = String::from_utf8_lossy(line);
        let mut from = 0;
        while let Some(found) = text[from..].find("super") {
            let start = from + found;
            let end = start + "super".len();
            let word = |c: char| c.is_alphanumeric() || c == '_';
            let before = text[..start].chars().next_back();
            let after = text[end..].chars().next();
            if !before.is_some_and(|c| word(c) || c == '.' || c == ':') && !after.is_some_and(word)
            {
                return Some(crate::core::Pos {
                    line: number,
                    col: start as u32 + 1,
                });
            }
            from = end;
        }
    }
    None
}

/// Does `def`, found where the member was asked for, override it there?
fn overrides(asked: &Asked<'_>, path: &str, facts: &Facts, def: &Def) -> bool {
    match asked.module() {
        None => {
            path == asked.path
                && def.nesting.len() > asked.def.nesting.len()
                && def.nesting.ends_with(&asked.def.nesting)
        }
        Some(module) => includes(facts)
            .any(|(level, included, _)| included == module && def.nesting.ends_with(level)),
    }
}

/// What in the member's own reach may read it unseen: a name sent at
/// runtime, a group macro trekr does not read, a shared group included by a
/// name it cannot read.
fn reach_caveats(tree: &Tree, scan: &Scan<'_>, asked: &Asked<'_>, names: &[&str], out: &mut Reads) {
    let home: &[String] = match asked.module() {
        Some(_) => &asked.def.nesting[asked.def.nesting.len() - 1..],
        None => &asked.def.nesting,
    };
    let in_reach = |nesting: &[String]| {
        rspec::in_group(nesting) && (nesting.ends_with(home) || home.ends_with(nesting))
    };
    let facts = scan.facts;
    for why in sends_in(facts, scan.path, names, |call| in_reach(&call.nesting)) {
        out.caveat(why);
    }
    for call in &facts.calls {
        if !call.group_body || call.recv != RecvShape::Implicit || !in_reach(&call.nesting) {
            continue;
        }
        let name = call.name.as_str();
        if SHARED_INCLUDERS.contains(&name) {
            let literal = facts
                .shared_names
                .iter()
                .any(|(pos, _, _)| pos.line == call.pos.line);
            if !literal {
                out.caveat(format!(
                    "`{name}` at line {} includes a shared group by a name trekr does not read",
                    call.pos.line
                ));
            }
            continue;
        }
        if DSL.contains(&name)
            || name.starts_with("attr_")
            || tree.lookup(rspec::EXAMPLE_GROUP, true, name).is_some()
        {
            continue;
        }
        out.caveat(format!(
            "`{name}` at line {}, a macro trekr does not read, may read it",
            call.pos.line
        ));
    }
}

/// Each send of a computed name in `facts` that may be one of `names`, as
/// a caveat: `send(name)`, `try("#{prefix}_id")`.
fn sends_in(
    facts: &Facts,
    path: &str,
    names: &[&str],
    wanted: impl Fn(&Call) -> bool,
) -> Vec<String> {
    computed_sends(facts, wanted)
        .into_iter()
        .filter(|(_, shape)| {
            names
                .iter()
                .any(|name| crate::core::shape_matches(shape, name))
        })
        .map(|(line, _)| {
            format!(
                "a name computed at runtime is sent at {}:{line}",
                short(path)
            )
        })
        .collect()
}

/// A path as a row shows it: a gem's from its gem directory.
fn short(path: &str) -> String {
    match path.find("/gems/") {
        Some(at) if path.starts_with('/') => path[at + "/gems/".len()..].to_string(),
        _ => path.to_string(),
    }
}

/// Each line of `facts` that sends a name it computes, with the shape of
/// the name: `send(name)`, `try("#{prefix}_id")`.
fn computed_sends(facts: &Facts, wanted: impl Fn(&Call) -> bool) -> Vec<(u32, String)> {
    let Some(source) = facts.source.as_deref() else {
        return Vec::new();
    };
    let lines: Vec<&[u8]> = source.split(|b| *b == b'\n').collect();
    let mut out = Vec::new();
    for call in &facts.calls {
        if !SENDS.contains(&call.name.as_str())
            || !matches!(call.recv, RecvShape::Implicit | RecvShape::SelfRecv)
            || call.argc.unwrap_or(1) == 0
            || !wanted(call)
        {
            continue;
        }
        let Some(line) = lines.get(call.pos.line as usize - 1) else {
            continue;
        };
        let after = line
            .get(call.pos.col as usize - 1 + call.name.len()..)
            .unwrap_or_default();
        if let Some(shape) = computed_name(&String::from_utf8_lossy(after)) {
            out.push((call.pos.line, shape));
        }
    }
    out
}

/// The modules a group in the member's reach includes in its body, as
/// `include Helpers` writes them.
fn group_includes(tree: &Tree, asked: &Asked<'_>, own: &Facts) -> Vec<String> {
    let home = &asked.def.nesting;
    let mut modules = Vec::new();
    for call in &own.calls {
        let in_reach = call.nesting.ends_with(home) || home.ends_with(&call.nesting);
        if !call.group_body
            || call.name != "include"
            || !in_reach
            || !rspec::in_group(&call.nesting)
        {
            continue;
        }
        for written in own
            .const_refs
            .iter()
            .filter(|r| r.pos.line == call.pos.line && r.nesting == call.nesting)
        {
            if let Some(fqn) = tree
                .resolve_at(&written.name, &written.nesting, asked.path)
                .fqn
                && !modules.contains(&fqn)
            {
                modules.push(fqn);
            }
        }
    }
    modules
}

/// The shape of a name a send computes, from the text after the method's
/// name: `*` for a value, `prefix_*` for `"prefix_#{x}"`. `None` for a
/// literal, which the index already reads as a call of its name.
fn computed_name(after: &str) -> Option<String> {
    let arg = after
        .trim_start()
        .strip_prefix('(')
        .unwrap_or(after)
        .trim_start();
    let quoted = arg.strip_prefix(":\"").or_else(|| arg.strip_prefix('"'));
    if let Some(body) = quoted {
        let end = body.find('"')?;
        let body = &body[..end];
        if !body.contains("#{") {
            return None;
        }
        let mut shape = String::new();
        let mut rest = body;
        while let Some(open) = rest.find("#{") {
            shape.push_str(&rest[..open]);
            shape.push('*');
            let close = rest[open..]
                .find('}')
                .map_or(rest.len(), |at| open + at + 1);
            rest = &rest[close..];
        }
        shape.push_str(rest);
        return Some(shape);
    }
    if arg.starts_with(':') || arg.starts_with('\'') || arg.is_empty() {
        return None;
    }
    Some("*".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_literal_name_is_not_computed() {
        assert_eq!(computed_name("(:widget)"), None);
        assert_eq!(computed_name(" \"widget\""), None);
        assert_eq!(computed_name("('widget')"), None);
    }

    #[test]
    fn an_interpolated_name_is_its_shape() {
        assert_eq!(computed_name("(\"#{prefix}_id\")").as_deref(), Some("*_id"));
        assert_eq!(computed_name("(:\"item_#{n}\")").as_deref(), Some("item_*"));
    }

    #[test]
    fn a_value_may_be_any_name() {
        assert_eq!(computed_name("(name)").as_deref(), Some("*"));
        assert_eq!(computed_name(" field, value").as_deref(), Some("*"));
    }
}
