//! `--dead` for classes, modules and constants (DEC-420): a declaration in
//! scope that no constant reference in the checkout resolves to.
//!
//! The references are the index's `const_ref` rows, resolved by Ruby's own
//! lookup as an editor's references are, so two classes that share a last
//! segment are told apart. A reference to `A::B` is a use of `A`, which holds
//! it; a reference written inside the constant's own body is not a use of it.

use std::collections::{HashMap, HashSet};
use std::path::Path;

mod named;
mod ways;

use super::config::Config;
use super::conventions::Convention;
use super::generated::Generated;
use super::routes::Routes;
use super::views::Views;
use crate::store::{ConstRefRow, Store};
use crate::tree::Tree;
use named::{Named, plain};
use ways::{Ways, routed_controllers};

/// What the checkout's references say about one constant: how many resolve
/// to it (or to a constant it holds) from outside its own body, and where
/// the first of each kind is.
#[derive(Default)]
struct Uses {
    live: usize,
    tests: usize,
    first_test: Option<(String, u32)>,
}

/// A spec or a test, by a directory on its path.
fn in_tests(path: &str) -> bool {
    path.split('/')
        .rev()
        .skip(1)
        .any(|dir| matches!(dir, "spec" | "test" | "tests"))
}

/// `A::B::C` and each namespace above it: `A::B`, `A`.
fn namespaces(fqn: &str) -> impl Iterator<Item = &str> {
    fqn.match_indices("::").map(move |(at, _)| &fqn[..at])
}

/// Every way a reference may spell `fqn`: each suffix, and the rooted whole.
fn spellings(fqn: &str) -> Vec<String> {
    let mut found: Vec<String> = std::iter::once(fqn)
        .chain(fqn.match_indices("::").map(|(at, _)| &fqn[at + 2..]))
        .map(str::to_string)
        .collect();
    found.push(format!("::{fqn}"));
    found
}

/// Is a reference written in `scope` inside `fqn`'s own body?
fn within(scope: &str, fqn: &str) -> bool {
    scope == fqn
        || scope
            .strip_prefix(fqn)
            .is_some_and(|rest| rest.starts_with("::"))
}

/// One declaration `--dead` weighs: its name, kind and first site in scope.
struct Declared {
    fqn: String,
    kind: String,
    path: String,
    line: u32,
    col: u32,
}

/// The classes, modules and constants declared in `files`, each once, at its
/// first site in scope. Not a reopening of a constant a gem or Ruby
/// declares, which deleting the checkout's body would not remove, and
/// nothing under a `db/` (an engine's or a plugin's too): a migration is run
/// by its file's name.
fn declared_in(
    tree: &Tree,
    all: &[(String, String)],
    root: &str,
    files: &HashSet<String>,
) -> Vec<Declared> {
    let prefix = format!("{root}/");
    let mut found = Vec::new();
    for (fqn, kind) in all {
        // A shared example group is a module trekr makes for a group RSpec
        // names by a string (DEC-092), not a constant anyone wrote.
        if !matches!(kind.as_str(), "class" | "module" | "constant")
            || crate::tree::public_name(fqn) != fqn
            || fqn.starts_with("RSpec::SharedExampleGroups::")
        {
            continue;
        }
        let sites = tree.sites(fqn);
        let Some(site) = sites.iter().find(|site| files.contains(&site.path)) else {
            continue;
        };
        if sites.iter().any(|site| !site.path.starts_with(&prefix)) {
            continue;
        }
        let relative = &site.path[prefix.len()..];
        if relative.starts_with("db/") || relative.contains("/db/") {
            continue;
        }
        found.push(Declared {
            fqn: fqn.clone(),
            kind: kind.clone(),
            path: site.path.clone(),
            line: site.line,
            col: site.col,
        });
    }
    found.sort_by(|a, b| (&a.path, a.line, a.col).cmp(&(&b.path, b.line, b.col)));
    found
}

/// The uses of every constant in `wanted`, and of each namespace in it
/// through the constants it holds. A count of live uses stops mattering at
/// one, so a reference that could only add to constants already used is not
/// resolved: the tests' are read last, and only for what is still unused.
///
/// A template's constants, and an extensionless script's, count as
/// references written at the top level, where each looks them up.
///
/// A constant is spelled through anything that has its namespace among its
/// ancestors, too: `Store::MAILER_KEY` when `Store` includes `Keys`,
/// `Child::LIMIT` when `Child < Base` — `heirs` is that map.
fn uses_of(
    tree: &Tree,
    store: &Store,
    root: &str,
    wanted: &HashSet<String>,
    heirs: &HashMap<String, Vec<String>>,
    unindexed: &[(&str, &str)],
) -> anyhow::Result<HashMap<String, Uses>> {
    let mut spelled: HashMap<String, Vec<&str>> = HashMap::new();
    // What a reference Ruby resolves but the index cannot names: a compact
    // `class Rename` under a namespace Zeitwerk makes from a directory
    // (`DataTableColumn::Rename` in `discourse_workflows/data_table_column/`)
    // is declared at the top level, so `DiscourseWorkflows::DataTableColumn
    // ::Rename` resolves to nothing. Read only for a reference that does not
    // resolve, by its own spelling or the one its file's path gives.
    let mut unplaced: HashMap<String, Vec<&str>> = HashMap::new();
    let prefix = format!("{root}/");
    for fqn in wanted {
        for spelling in spellings(fqn) {
            spelled.entry(spelling).or_default().push(fqn);
        }
        if let Some((owner, tail)) = fqn.rsplit_once("::") {
            for heir in heirs.get(owner).into_iter().flatten() {
                for spelling in spellings(&format!("{heir}::{tail}")) {
                    spelled.entry(spelling).or_default().push(fqn);
                }
            }
        }
        let head = fqn.split("::").next().unwrap_or(fqn);
        if head == fqn || !tree.sites(head).is_empty() {
            continue;
        }
        let aliases = tree
            .sites(fqn)
            .iter()
            .filter_map(|site| site.path.strip_prefix(&prefix).and_then(zeitwerk_name))
            .collect::<Vec<_>>();
        for spelling in
            std::iter::once(fqn.clone()).chain(aliases.iter().flat_map(|a| spellings(a)))
        {
            unplaced.entry(spelling).or_default().push(fqn);
        }
    }
    let names: Vec<String> = spelled.keys().chain(unplaced.keys()).cloned().collect();
    let mut rows: Vec<ConstRefRow> = store.const_refs_named(root, &names)?;
    rows.extend(
        unindexed
            .iter()
            .filter(|(written, _)| spelled.contains_key(*written))
            .map(|(written, template)| ConstRefRow {
                path: template.to_string(),
                name: written.to_string(),
                nesting: Vec::new(),
                line: 0,
            }),
    );
    rows.sort_by_key(|row| in_tests(&row.path));
    let mut resolved: HashMap<(String, Vec<String>), Option<String>> = HashMap::new();
    let mut scopes: HashMap<Vec<String>, String> = HashMap::new();
    let mut uses: HashMap<String, Uses> = HashMap::new();
    let live = |uses: &HashMap<String, Uses>, fqn: &str| uses.get(fqn).is_some_and(|u| u.live > 0);
    for row in rows {
        let settled = spelled.get(&row.name).is_some_and(|fqns| {
            fqns.iter().all(|fqn| {
                std::iter::once(*fqn)
                    .chain(namespaces(fqn))
                    .all(|used| !wanted.contains(used) || live(&uses, used))
            })
        });
        if settled {
            continue;
        }
        let key = (row.name.clone(), row.nesting.clone());
        let fqn = resolved
            .entry(key)
            .or_insert_with(|| tree.resolve(&row.name, &row.nesting).fqn)
            .clone();
        let fqns: Vec<String> = match fqn {
            Some(fqn) => vec![fqn],
            None => match unplaced.get(&row.name) {
                Some(fqns) => fqns.iter().map(|f| f.to_string()).collect(),
                None => continue,
            },
        };
        let scope = scopes
            .entry(row.nesting.clone())
            .or_insert_with(|| tree.scope_fqn(&row.nesting).unwrap_or_default())
            .clone();
        let test = in_tests(&row.path);
        for used in fqns
            .iter()
            .flat_map(|fqn| std::iter::once(fqn.as_str()).chain(namespaces(fqn)))
        {
            if !wanted.contains(used) || within(&scope, used) {
                continue;
            }
            let entry = uses.entry(used.to_string()).or_default();
            if test {
                entry.tests += 1;
                entry
                    .first_test
                    .get_or_insert_with(|| (row.path.clone(), row.line));
            } else {
                entry.live += 1;
            }
        }
    }
    Ok(uses)
}

/// The constant a file defines as Zeitwerk names it: its path under an
/// autoload root (`app/<kind>/`, `lib/`), camelized —
/// `plugins/x/app/services/billing/widget_rename.rb` is `Billing::WidgetRename`.
fn zeitwerk_name(relative: &str) -> Option<String> {
    let stem = relative.strip_suffix(".rb")?;
    let rest = match stem.rfind("app/") {
        Some(at) if at == 0 || stem[..at].ends_with('/') => stem[at + 4..].split_once('/')?.1,
        _ => match stem.rfind("lib/") {
            Some(at) if at == 0 || stem[..at].ends_with('/') => &stem[at + 4..],
            _ => return None,
        },
    };
    Some(
        rest.split('/')
            .map(crate::extract::camelize)
            .collect::<Vec<_>>()
            .join("::"),
    )
}

/// What `--dead` has read of the checkout beside its Ruby, for its methods
/// and its constants alike.
pub(super) struct Sources<'a> {
    pub(super) routes: &'a Routes,
    pub(super) views: &'a Views,
    pub(super) config: &'a Config,
    pub(super) texts: &'a super::built::Texts,
}

/// Pushes a row per class, module or constant in `files` that no reference
/// outside the tests reaches.
pub(super) fn dead_constants(
    tree: &Tree,
    store: &Store,
    root: &Path,
    files: &[std::path::PathBuf],
    sources: Sources<'_>,
    rows: &mut Vec<serde_json::Value>,
) -> anyhow::Result<()> {
    let Sources {
        routes,
        views,
        config,
        texts,
    } = sources;
    let root_str = root.to_string_lossy().into_owned();
    let files: HashSet<String> = files
        .iter()
        .map(|file| file.to_string_lossy().into_owned())
        .collect();
    let all = tree.declared();
    let declared = declared_in(tree, &all, &root_str, &files);
    if declared.is_empty() {
        return Ok(());
    }
    // A namespace is used through what it holds, wherever that is declared.
    let candidates: HashSet<String> = declared.iter().map(|d| d.fqn.clone()).collect();
    let mut wanted = candidates.clone();
    let prefix = format!("{root_str}/");
    let mut classes = HashSet::new();
    let mut tails: HashMap<String, usize> = HashMap::new();
    let mut children: HashMap<String, Vec<String>> = HashMap::new();
    let mut heirs: HashMap<String, Vec<String>> = HashMap::new();
    for (fqn, kind) in &all {
        let sites = tree.sites(fqn);
        if !sites.iter().any(|site| site.path.starts_with(&prefix)) {
            continue;
        }
        if kind == "class" || kind == "module" {
            for ancestor in tree.ancestors(fqn).chain.iter().filter(|a| *a != fqn) {
                heirs
                    .entry(crate::tree::public_name(ancestor).to_string())
                    .or_default()
                    .push(fqn.clone());
            }
        }
        let tail = fqn.rsplit("::").next().unwrap_or(fqn).to_string();
        if kind == "class" || kind == "module" {
            classes.insert(plain(&tail));
        }
        if kind == "class"
            && let Some((namespace, _)) = fqn.rsplit_once("::")
        {
            children
                .entry(namespace.to_string())
                .or_default()
                .push(fqn.clone());
        }
        *tails.entry(tail).or_default() += 1;
        if namespaces(fqn).any(|ns| candidates.contains(ns)) {
            wanted.insert(fqn.clone());
        }
    }
    let mut named = Named::read(texts);
    // A YAML value spells a constant as a whole string does.
    for (constant, (path, line)) in config.constants() {
        named
            .strings
            .entry(constant.to_string())
            .or_insert_with(|| (path.to_string(), line as u32));
    }
    let unindexed: Vec<(&str, &str)> = views
        .constants()
        .chain(
            named
                .script_constants
                .iter()
                .map(|(c, p)| (c.as_str(), p.as_str())),
        )
        .collect();
    let uses = uses_of(tree, store, &root_str, &wanted, &heirs, &unindexed)?;
    let ways = Ways {
        tree,
        routes,
        views,
        routed: routed_controllers(tree, &all, routes, &named),
        named,
        prefix: prefix.clone(),
        classes,
        tails,
        own_listings: Default::default(),
        children,
        foreign_reads: Default::default(),
        rubocop: ways::rubocop_requires(root),
    };
    let live = |fqn: &str| uses.get(fqn).is_some_and(|u| u.live > 0);
    // A namespace holding a constant reached by convention holds a used one.
    let mut reached: HashSet<&str> = HashSet::new();
    for fqn in &wanted {
        if candidates.contains(fqn) || live(fqn) {
            continue;
        }
        let sites = tree.sites(fqn);
        let Some(site) = sites.iter().find(|site| site.path.starts_with(&prefix)) else {
            continue;
        };
        let kind = tree.kind_of(fqn).unwrap_or_default();
        if ways
            .convention(fqn, kind, &site.path[prefix.len()..], site.line)
            .is_some()
        {
            reached.insert(fqn);
        }
    }
    let mut conventions: HashMap<&str, Option<Convention>> = HashMap::new();
    for Declared {
        fqn,
        kind,
        path,
        line,
        ..
    } in &declared
    {
        if !live(fqn) {
            let found = ways.convention(fqn, kind, &path[prefix.len()..], *line);
            if found.is_some() {
                reached.insert(fqn);
            }
            conventions.insert(fqn, found);
        }
    }
    let holds_used = |fqn: &str| {
        reached
            .iter()
            .any(|other| *other != fqn && namespaces(other).any(|ns| ns == fqn))
    };

    let mut found = Vec::new();
    for Declared {
        fqn,
        kind,
        path,
        line,
        col,
    } in &declared
    {
        if live(fqn) || holds_used(fqn) {
            continue;
        }
        let used = uses.get(fqn.as_str());
        let tests = used.map_or(0, |u| u.tests);
        let convention = conventions.remove(fqn.as_str()).flatten();
        let (tier, reason) = match (&convention, used.and_then(|u| u.first_test.as_ref())) {
            (Some(convention), _) => ("convention-only", convention.reason.clone()),
            (None, Some((at, line))) => (
                "test-only",
                format!("referenced only from tests ({tests}, first at {at}:{line})"),
            ),
            (None, None) => ("unreferenced", "no constant reference names it".to_string()),
        };
        let caveat = match tier {
            "convention-only" => String::new(),
            _ => ways.caveats(fqn).join(", "),
        };
        let (owner, name) = fqn.rsplit_once("::").unwrap_or(("", fqn));
        let mut row = serde_json::json!({
            "kind": kind,
            "name": name,
            "owner": owner,
            "visibility": "public",
            "path": path,
            "line": line,
            "col": col,
            "tier": tier,
            "test_refs": tests,
            "confidence": if caveat.is_empty() { "clear" } else { "lower" },
            "caveat": caveat,
            "reason": reason,
        });
        if let Some(convention) = convention {
            row["convention"] = serde_json::json!({ "by": convention.by });
            if let Some((path, line)) = convention.at {
                row["convention"]["path"] = path.into();
                row["convention"]["line"] = line.into();
            }
        }
        found.push(row);
    }
    // Code a generator wrote is reached by the contract it implements, and
    // comes back when it is generated again (DEC-403).
    let generated = Generated::read(
        root,
        found
            .iter()
            .filter(|row| row["tier"] != "convention-only")
            .filter_map(|row| row["path"].as_str()),
    );
    for mut row in found {
        let how = row["path"].as_str().and_then(|path| generated.why(path));
        if let Some(how) = how.filter(|_| row["tier"] != "convention-only") {
            let caveat = match row["caveat"].as_str().unwrap_or_default() {
                "" => String::new(),
                said => format!("{said}, "),
            };
            row["caveat"] = format!(
                "{caveat}in generated code ({how}), which its runtime may load generically"
            )
            .into();
            row["confidence"] = "lower".into();
        }
        rows.push(row);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_spelled_by_each_suffix_and_rooted() {
        assert_eq!(
            spellings("A::B::C"),
            ["A::B::C", "B::C", "C", "::A::B::C"].map(String::from)
        );
    }

    #[test]
    fn a_body_is_within_itself_and_what_it_holds_not_a_sibling() {
        assert!(within("A::B", "A"));
        assert!(within("A", "A"));
        assert!(!within("AB", "A"));
        assert!(!within("", "A"));
    }

    #[test]
    fn a_file_under_an_autoload_root_names_its_constant() {
        assert_eq!(
            zeitwerk_name("plugins/x/app/services/billing/widget_rename.rb").as_deref(),
            Some("Billing::WidgetRename")
        );
        assert_eq!(
            zeitwerk_name("lib/alpha/beta.rb").as_deref(),
            Some("Alpha::Beta")
        );
        assert_eq!(zeitwerk_name("config/application.rb"), None);
    }

    #[test]
    fn a_spec_directory_anywhere_makes_a_test() {
        assert!(in_tests("spec/models/widget_spec.rb"));
        assert!(in_tests("plugins/chat/spec/x_spec.rb"));
        assert!(!in_tests("app/models/spec.rb"));
    }
}
