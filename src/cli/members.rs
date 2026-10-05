//! An example group's own methods on the command line: `--dead`'s rows for
//! a `let`, a `subject` and a group's `def`, from the reads
//! `resolve::members` finds, which `--refs` lists (DEC-490).

use crate::core::Def;
use crate::query::members::CheckoutFiles;
use crate::resolve::members::{self, Asked, Context, Files, Reads};

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
    // Every member row is `lower` (DEC-496): on suites held out from the
    // rules that read them, a row with no caveat was not reliably unused.
    let caveat = match reads.caveats.is_empty() {
        true => GRADED_LOWER.to_string(),
        false => said(&reads.caveats),
    };
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
        "confidence": "lower",
        "caveat": caveat,
        "reason": reason(tier, &reads),
    }))
}

/// Why a member row with no caveat of its own is `lower` (DEC-496).
const GRADED_LOWER: &str = "an example group's lets, subjects, defs and shared groups are graded \
     lower: on suites held out from the rules that read them, a row with no caveat was truly \
     unused 79 %, 47 % and 0.2 % of the time (DEC-496)";

/// A row's caveats, the first few: one helper that sends computed names
/// may be cited on every line it does.
fn said(caveats: &[String]) -> String {
    const SHOWN: usize = 3;
    let mut text = caveats
        .iter()
        .take(SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if caveats.len() > SHOWN {
        text.push_str(&format!(", and {} more", caveats.len() - SHOWN));
    }
    text
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
        lines.push(format!("  may be read unseen: {}", said(&reads.caveats)));
    }
    lines
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
                .find_map(|call| members::literal_on(source, pos.line, call))
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
                .and_then(|source| members::literal_on(source, call.pos.line, &call.name));
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
            .unwrap_or_else(|| GRADED_LOWER.to_string());
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
            "confidence": "lower",
            "caveat": caveat,
            "reason": reason,
        }));
        if !group.by_metadata {
            listed.push(group.module);
        }
    }
    listed
}
