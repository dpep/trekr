//! An example group's own methods on the command line: `--dead`'s rows for
//! a `let`, a `subject` and a group's `def`, from the reads
//! `resolve::members` finds, which `--refs` lists (DEC-490).

use crate::core::{Def, Facts};
use crate::resolve::members::{self, Asked, Context, Files, Reads};
use crate::store::Store;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

/// The checkout's files, each parsed once: an editor's open copy where it
/// has one, else the disk's.
pub(crate) struct CheckoutFiles<'a> {
    store: &'a Store,
    root: &'a Path,
    root_str: &'a str,
    /// Open buffers' text, by checkout-relative path.
    open: HashMap<String, String>,
    held: RefCell<HashMap<String, Option<Arc<Facts>>>>,
}

impl<'a> CheckoutFiles<'a> {
    pub(crate) fn new(store: &'a Store, root: &'a Path, root_str: &'a str) -> CheckoutFiles<'a> {
        CheckoutFiles::with_open(store, root, root_str, HashMap::new())
    }

    pub(crate) fn with_open(
        store: &'a Store,
        root: &'a Path,
        root_str: &'a str,
        open: HashMap<String, String>,
    ) -> CheckoutFiles<'a> {
        CheckoutFiles {
            store,
            root,
            root_str,
            open,
            held: RefCell::new(HashMap::new()),
        }
    }
}

impl Files for CheckoutFiles<'_> {
    fn facts(&self, path: &str) -> Option<Arc<Facts>> {
        if let Some(held) = self.held.borrow().get(path) {
            return held.clone();
        }
        let bytes = match self.open.get(path) {
            Some(text) => Some(text.as_bytes().to_vec()),
            None => std::fs::read(self.root.join(path)).ok(),
        };
        let facts = bytes.map(|bytes| Arc::new(crate::extract::extract(&bytes)));
        self.held
            .borrow_mut()
            .insert(path.to_string(), facts.clone());
        facts
    }

    fn calling(&self, name: &str) -> Vec<String> {
        self.store
            .files_calling(self.root_str, name)
            .unwrap_or_default()
    }
}

/// What kind of member a row is: `let`, `subject`, or a group's `def`.
pub(super) fn kind(def: &Def) -> &'static str {
    match def.via.as_deref() {
        Some(via) if via.starts_with("subject") => "subject",
        Some(_) => "let",
        None => "method",
    }
}

/// A `let!` or `subject!` runs for every example, read or not: removing one
/// changes what each example sets up, so `--dead` says nothing of it.
pub(super) fn eager(def: &Def) -> bool {
    def.via.as_deref().is_some_and(|via| via.ends_with('!'))
}

/// The group a member is written in, as RSpec names its class, without
/// the `RSpec::ExampleGroups::` every group shares.
fn group_name(asked: &Asked<'_>) -> String {
    let owner = asked.owner();
    if let Some(name) = owner.strip_prefix("RSpec::SharedExampleGroups::") {
        return format!("shared group {name}");
    }
    owner
        .strip_prefix("RSpec::ExampleGroups::")
        .map(str::to_string)
        .unwrap_or(owner)
}

/// `--dead`'s row for a member nothing reads, or `None` when something does.
pub(super) fn dead_row(
    context: &Context<'_>,
    file: &str,
    relative: &str,
    def: &Def,
) -> Option<serde_json::Value> {
    let asked = Asked {
        path: relative,
        def,
    };
    let reads = members::reads(context, &asked, false);
    if reads.counts.confirmed + reads.counts.possible > 0 {
        return None;
    }
    let tier = match reads.overridden_by.is_empty() {
        true => "unreferenced",
        false => "shadowed",
    };
    let caveat = reads.caveats.join(", ");
    Some(serde_json::json!({
        "kind": kind(def),
        "name": def.name,
        "owner": asked.owner(),
        "group": group_name(&asked),
        "singleton": false,
        "visibility": def.visibility.as_str(),
        "path": file,
        "line": def.pos.line,
        "col": def.pos.col,
        "end_line": def.end_line,
        "tier": tier,
        "confirmed": 0,
        "possible": 0,
        "overridden_by": reads.overridden_by,
        "shared_groups_read": reads.shared_groups,
        "helpers_read": reads.helpers,
        "confidence": if caveat.is_empty() { "clear" } else { "lower" },
        "caveat": caveat,
        "reason": reason(tier, &reads),
    }))
}

fn reason(tier: &str, reads: &Reads) -> String {
    match tier {
        "shadowed" => format!(
            "every call of its name in reach runs an override instead: {}",
            reads.overridden_by.join(", ")
        ),
        _ => format!(
            "no call reads it, in its group or those nested in it, in {} shared group(s) included there, or in {} helper(s) every group mixes in",
            reads.shared_groups, reads.helpers
        ),
    }
}

/// How `--dead`'s text names a member row: `let(:widget) in Widget::Saved`.
pub(super) fn dead_name(row: &serde_json::Value) -> String {
    let name = row["name"].as_str().unwrap_or_default();
    let group = row["group"].as_str().unwrap_or_default();
    match row["kind"].as_str() {
        Some("subject") if name == "subject" => format!("subject in {group}"),
        Some("subject") => format!("subject(:{name}) in {group}"),
        Some("let") => format!("let(:{name}) in {group}"),
        Some("shared_group") => format!("shared group {name:?}"),
        _ => format!("def {name} in {group}"),
    }
}

/// The member at a position: written there (`let(:widget)`, `def helper`),
/// or the one a call there reads. A bare `FILE:LINE` takes a member defined
/// on the line. Its file's checkout-relative path, and its definition.
pub(crate) fn member_at_position(
    tree: &crate::tree::Tree,
    files: &CheckoutFiles<'_>,
    relative: &str,
    line: u32,
    col: u32,
) -> Option<(String, Def)> {
    use crate::resolve::members::{Named, is_member, named_by};
    let facts = files.facts(relative)?;
    if col == 0 {
        return facts
            .defs
            .iter()
            .filter(|def| def.pos.line == line && is_member(def))
            .find(|def| !members::names_a_named_subject(def, &facts))
            .map(|def| (relative.to_string(), def.clone()));
    }
    match super::position::at_facts(&facts, line, col)? {
        super::position::Under::Definition(def) if is_member(&def) => {
            Some((relative.to_string(), def))
        }
        super::position::Under::Call(call) => match named_by(tree, &facts, &call)? {
            Named::Here(def) => Some((relative.to_string(), *def)),
            Named::Shared { site, line } => {
                let root = tree.site_path("");
                let path = site.strip_prefix(&root)?.to_string();
                let facts = files.facts(&path)?;
                let def = facts
                    .defs
                    .iter()
                    .find(|def| def.pos.line == line && def.name == call.name && is_member(def))?
                    .clone();
                Some((path, def))
            }
        },
        _ => None,
    }
}

/// `--refs FILE:LINE:COL` on an example group's member: every read, tiered,
/// with where each was found (DEC-490).
pub(super) fn refs_answer(
    context: &Context<'_>,
    written: &str,
    path: &str,
    def: &Def,
    include_excluded: bool,
) -> (serde_json::Value, Reads) {
    let asked = Asked { path, def };
    let reads = members::reads(context, &asked, include_excluded);
    let answer = serde_json::json!({
        "query": written,
        "status": "resolved",
        "kind": kind(def),
        "name": def.name,
        "owner": asked.owner(),
        "group": group_name(&asked),
        "definition": [{ "path": path, "line": def.pos.line, "col": def.pos.col }],
        "counts": reads.counts,
        "overridden_by": reads.overridden_by,
        "shared_groups_read": reads.shared_groups,
        "helpers_read": reads.helpers,
        "caveats": reads.caveats,
        "references": null,
    });
    (answer, reads)
}

/// The text `--refs` prints for a member's reads.
pub(super) fn refs_text(answer: &serde_json::Value, reads: &Reads) -> Vec<String> {
    let mut lines = Vec::new();
    let site = &answer["definition"][0];
    let named = dead_name(answer);
    lines.push(format!(
        "{}:{}:{}  definition  {named}",
        super::shown(site["path"].as_str().unwrap_or_default()),
        site["line"],
        site["col"],
    ));
    for reference in &reads.found {
        lines.push(format!(
            "{}:{}:{}  {:<10} {:<15} {}",
            super::shown(&reference.path),
            reference.line,
            reference.col,
            format!("{:?}", reference.tier).to_lowercase(),
            reference.from.unwrap_or_default(),
            reference.why,
        ));
    }
    let counts = &reads.counts;
    lines.push(format!(
        "\n{} confirmed, {} possible, {} excluded; read in its group and those nested in it, {} shared group(s) and {} helper(s)",
        counts.confirmed, counts.possible, counts.excluded, reads.shared_groups, reads.helpers,
    ));
    if !reads.overridden_by.is_empty() {
        lines.push(format!(
            "  overridden where its name is read: {}",
            reads.overridden_by.join(", ")
        ));
    }
    if !reads.caveats.is_empty() {
        lines.push(format!(
            "  may be read unseen: {}",
            reads.caveats.join(", ")
        ));
    }
    lines
}

/// `--def` on a call in a shared group's body that the body does not
/// answer: the includers' members of the name, one answer each (DEC-490).
/// `None` when no includer defines one.
pub(crate) fn includer_answer(
    tree: &crate::tree::Tree,
    files: &CheckoutFiles<'_>,
    relative: &str,
    call: &crate::core::Call,
    answer: &crate::resolve::MethodAnswer,
) -> Option<crate::resolve::MethodAnswer> {
    let facts = files.facts(relative)?;
    let context = Context::new(tree, files);
    let found = members::includer_members(&context, relative, &facts, call);
    if found.is_empty() {
        return None;
    }
    let sites: Vec<crate::tree::Site> = found
        .iter()
        .map(|(path, def)| crate::tree::Site {
            path: tree.site_path(path),
            line: def.pos.line,
            col: def.pos.col,
            kind: "method".to_string(),
        })
        .collect();
    let (path, def) = &found[0];
    let first = Asked { path, def };
    let n = found.len();
    Some(crate::resolve::MethodAnswer {
        status: match n {
            1 => crate::tree::Status::Resolved,
            _ => crate::tree::Status::Ambiguous,
        },
        confidence: crate::resolve::share(1, n),
        resolved_via: Some("includer".to_string()),
        receiver: answer.receiver,
        receiver_type: answer.receiver_type.clone(),
        receiver_kind: answer.receiver_kind.clone(),
        owner: Some(first.owner()),
        kind: Some(match def.via {
            Some(_) => crate::tree::Kind::Declaration,
            None => crate::tree::Kind::Definition,
        }),
        defined_via: def.via.clone(),
        sites,
        agreement: (n > 1).then(|| format!("{n} groups that include the shared group define it")),
        unresolved_ancestors: Vec::new(),
        candidates: Vec::new(),
        reason: None,
    })
}

/// A shared group the scope writes: its module, the name it is written
/// with, where, and whether metadata it is written with includes it.
struct SharedGroup {
    module: String,
    name: String,
    path: String,
    relative: String,
    line: u32,
    col: u32,
    by_metadata: bool,
}

/// The literal a call on this line hands its first argument, and whether
/// more arguments follow it before the block: `shared_context "x", :db do`.
fn literal_on(source: &[u8], line: u32, after: &str) -> Option<(String, bool)> {
    let text = String::from_utf8_lossy(source.split(|b| *b == b'\n').nth(line as usize - 1)?);
    let rest = &text[text.find(after)? + after.len()..];
    let rest = rest.trim_start().trim_start_matches('(').trim_start();
    let (name, tail) = match rest.chars().next()? {
        quote @ ('"' | '\'') => {
            let body = &rest[1..];
            let end = body.find(quote)?;
            (body[..end].to_string(), &body[end + 1..])
        }
        ':' => {
            let body = &rest[1..];
            let end = body
                .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '?' || c == '!'))
                .unwrap_or(body.len());
            (body[..end].to_string(), &body[end..])
        }
        _ => return None,
    };
    let tail = tail.trim_start().trim_start_matches(')').trim_start();
    Some((name, tail.starts_with(',')))
}

/// `--dead`'s rows for the shared groups the scope writes that no group
/// includes by name (DEC-493), and the modules of those it lists, whose own
/// members go with them.
pub(super) fn dead_shared_groups(
    files: &CheckoutFiles<'_>,
    scope: &[(String, String)],
    rows: &mut Vec<serde_json::Value>,
) -> Vec<String> {
    use crate::core::rspec;
    let mut written: Vec<SharedGroup> = Vec::new();
    for (file, relative) in scope {
        let Some(facts) = files.facts(relative) else {
            continue;
        };
        let Some(source) = facts.source.as_deref() else {
            continue;
        };
        let top = facts
            .defs
            .iter()
            .filter(|def| def.kind == crate::core::Kind::Module)
            .filter_map(|def| {
                let module = def.name.trim_start_matches("::");
                rspec::is_shared_module(module).then(|| (module.to_string(), def.pos))
            });
        let local = facts
            .local_shared
            .iter()
            .map(|local| (local.module.clone(), local.pos));
        for (module, pos) in top.chain(local) {
            let (name, by_metadata) = ["shared_examples_for", "shared_examples", "shared_context"]
                .iter()
                .find_map(|call| literal_on(source, pos.line, call))
                .unwrap_or_else(|| {
                    (
                        module.rsplit("::").next().unwrap_or_default().to_string(),
                        false,
                    )
                });
            written.push(SharedGroup {
                module,
                name,
                path: file.clone(),
                relative: relative.clone(),
                line: pos.line,
                col: pos.col,
                by_metadata,
            });
        }
    }
    if written.is_empty() {
        return Vec::new();
    }
    // Every include of a shared group by name, in the checkout; a local
    // group's only in its own file, which scopes it.
    let mut paths: Vec<String> = members::SHARED_INCLUDERS
        .iter()
        .flat_map(|name| files.calling(name))
        .collect();
    paths.sort();
    paths.dedup();
    let mut included: Vec<(String, String)> = Vec::new();
    let mut unread: Option<String> = None;
    for path in &paths {
        let Some(facts) = files.facts(path) else {
            continue;
        };
        for (_, _, module) in &facts.shared_names {
            included.push((path.clone(), module.clone()));
        }
        for call in facts
            .calls
            .iter()
            .filter(|c| members::SHARED_INCLUDERS.contains(&c.name.as_str()))
        {
            if facts
                .shared_names
                .iter()
                .any(|(pos, _, _)| pos.line == call.pos.line)
            {
                continue;
            }
            // `config.include_context "x"`, and a name trekr cannot read.
            let literal = facts
                .source
                .as_deref()
                .and_then(|source| literal_on(source, call.pos.line, &call.name));
            match literal {
                Some((name, _)) => {
                    included.push((path.clone(), rspec::shared_module(&rspec::base_name(&name))))
                }
                None if unread.is_none() => {
                    unread = Some(format!("{path}:{}", call.pos.line));
                }
                None => {}
            }
        }
    }
    let mut listed = Vec::new();
    for group in written {
        let local = files.facts(&group.relative).is_some_and(|facts| {
            facts
                .local_shared
                .iter()
                .any(|l| l.module == group.module && l.pos.line == group.line)
        });
        let used = included
            .iter()
            .any(|(path, module)| *module == group.module && (!local || *path == group.relative));
        if used {
            continue;
        }
        let tier = match group.by_metadata {
            true => "convention-only",
            false => "unreferenced",
        };
        let caveat = unread
            .as_ref()
            .map(|at| format!("a shared group is included by a name trekr does not read, at {at}"))
            .unwrap_or_default();
        let reason = match group.by_metadata {
            true => "no group includes it by name, but metadata it is written with includes it in the groups that match".to_string(),
            false => "no group includes it by name: no `it_behaves_like`, `include_examples` or `include_context` of it".to_string(),
        };
        rows.push(serde_json::json!({
            "kind": "shared_group",
            "name": group.name,
            "owner": group.module,
            "group": serde_json::Value::Null,
            "singleton": false,
            "visibility": "public",
            "path": group.path,
            "line": group.line,
            "col": group.col,
            "tier": tier,
            "confirmed": 0,
            "possible": 0,
            "confidence": if caveat.is_empty() { "clear" } else { "lower" },
            "caveat": caveat,
            "reason": reason,
        }));
        if !group.by_metadata {
            listed.push(group.module);
        }
    }
    listed
}
