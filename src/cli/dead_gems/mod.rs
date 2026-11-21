//! `--dead Gemfile` (DEC-670): the gems a Gemfile or gemspec declares that
//! nothing in the checkout is seen to use, graded as `--dead` grades code.
//!
//! A gem is used through its constants, a `require` of one of its files, or
//! a method only it defines; or through a gem it brings that the app uses
//! and nothing else brings. A gem with none of that is a row — but most such
//! gems are still used, by name, from outside Ruby: an adapter a YAML file
//! names, a command a Procfile runs, a plugin a config lists, a Railtie that
//! configures the app at boot. What was found is the tier; what might still
//! reach it is the caveat. trekr never says a gem is unused.

// Scans Ruby as text: a str slice must say why its offset is a char boundary (DEC-835).
#![deny(clippy::string_slice)]

use super::dead::{DeadTier, Grade};
use super::dead_consts::in_tests;
use super::*;
use crate::gems::gemfile::{self, Declaration, Lock, Require};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// Whether `--dead` was handed a gem manifest: grade the bundle, not code.
pub(super) fn is_manifest(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    matches!(name, "Gemfile" | "gems.rb") || name.ends_with(".gemspec")
}

/// Whether `--dead` was handed a bundle's file: a manifest, or a lockfile
/// or a second bundle's file, which are refused.
pub(super) fn is_bundle_file(path: &Path) -> bool {
    is_manifest(path)
        || lockfile_manifest(path).is_some()
        || is_other_bundle(path)
        || is_evaled(path)
}

/// A file the root's Gemfile reads with `eval_gemfile` (`Gemfile.tools`),
/// directly or through another: its gems are the root bundle's.
fn is_evaled(path: &Path) -> bool {
    let Ok(root) = named_checkout(path) else {
        return false;
    };
    let canonical = |p: &Path| std::fs::canonicalize(p).ok();
    let Some(wanted) = canonical(path) else {
        return false;
    };
    let gemfile = crate::gems::bundle_files(&root).0;
    let mut todo = vec![gemfile];
    let mut seen = HashSet::new();
    while let Some(file) = todo.pop() {
        let Some(at) = canonical(&file) else {
            continue;
        };
        if !seen.insert(at.clone()) {
            continue;
        }
        if at == wanted && seen.len() > 1 {
            return true;
        }
        let dir = file.parent().unwrap_or(&root).to_path_buf();
        if let Ok(raw) = crate::scan::read_source(&file) {
            todo.extend(
                gemfile::declarations(&raw)
                    .evals
                    .iter()
                    .map(|e| dir.join(e)),
            );
        }
    }
    false
}

/// A second bundle's manifest or lock, beside the root's: `Gemfile.next`
/// and `Gemfile.next.lock`, Appraisal's `gemfiles/rails_7.gemfile`. The
/// index located the root bundle's gems, not these.
fn is_other_bundle(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    !is_manifest(path)
        && lockfile_manifest(path).is_none()
        && (name.starts_with("Gemfile.")
            || name.ends_with(".gemfile")
            || name.ends_with(".gemfile.lock"))
}

/// The manifest a lockfile locks: `Gemfile` for `Gemfile.lock`.
fn lockfile_manifest(path: &Path) -> Option<&'static str> {
    match path.file_name()?.to_str()? {
        "Gemfile.lock" => Some("Gemfile"),
        "gems.locked" => Some("gems.rb"),
        _ => None,
    }
}

/// One gem the manifest declares and nothing is seen to use.
#[derive(Serialize)]
struct GemRow {
    kind: &'static str,
    name: String,
    path: String,
    line: u32,
    col: u32,
    tier: DeadTier,
    confidence: Grade,
    caveat: String,
    reason: String,
    version: Option<String>,
    gem_dir: Option<String>,
    groups: Vec<String>,
    bundler_require: &'static str,
    needed_by: Vec<String>,
    executables: Vec<String>,
    test_refs: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    convention: Option<Named>,
}

/// Where something outside Ruby names a gem.
#[derive(Clone, Serialize)]
struct Named {
    by: &'static str,
    path: String,
    line: u32,
    text: String,
}

/// The first evidence a declared gem is used: the answer's `referenced`.
#[derive(Clone, Serialize)]
struct Evidence {
    name: String,
    by: &'static str,
    what: String,
    path: String,
    line: u32,
}

#[derive(Serialize)]
struct Summary {
    candidates: usize,
    declared: usize,
    referenced: usize,
    transitive: usize,
    tiers: std::collections::BTreeMap<&'static str, usize>,
    confidence: std::collections::BTreeMap<&'static str, usize>,
}

/// What the checkout's code says about one gem: the first use outside the
/// tests, and the uses inside them.
#[derive(Default)]
struct Uses {
    live: Option<Evidence>,
    tests: usize,
    first_test: Option<Evidence>,
}

impl Uses {
    fn add(&mut self, evidence: Evidence) {
        let tests = in_tests(&evidence.path);
        self.add_as(evidence, tests);
    }

    fn add_as(&mut self, evidence: Evidence, tests: bool) {
        if tests {
            self.tests += 1;
            self.first_test.get_or_insert(evidence);
        } else {
            self.live.get_or_insert(evidence);
        }
    }

    fn any(&self) -> Option<&Evidence> {
        self.live.as_ref().or(self.first_test.as_ref())
    }
}

pub(super) fn cmd_dead_gems(
    out: Output,
    paths: &[PathBuf],
    explain: bool,
) -> anyhow::Result<ExitCode> {
    if let Some((lockfile, manifest)) = paths.iter().find_map(|p| Some((p, lockfile_manifest(p)?)))
    {
        return Err(Failure::Usage.error(format!(
            "--dead grades the gems a manifest declares: pass the {manifest}, not {}",
            lockfile.display()
        )));
    }
    if let Some(other) = paths.iter().find(|p| is_other_bundle(p) && !is_evaled(p)) {
        return Err(Failure::Usage.error(other_bundle_refusal(other)));
    }
    if let Some(other) = paths.iter().find(|p| !is_manifest(p) && !is_evaled(p)) {
        return Err(Failure::Usage.error(format!(
            "--dead grades a Gemfile's gems apart from code: `--dead Gemfile`, then `--dead {}`",
            other.display()
        )));
    }
    let mut store = open_store()?;
    let mut graded = Vec::new();
    let mut seen = HashSet::new();
    for path in paths {
        if !path.is_file() {
            return Err(Failure::NotFound.error(format!("no such file: {}", path.display())));
        }
        let root = named_checkout(path)?;
        // A link's directory is where it is written, not where it points:
        // a root `Gemfile` may link to `gemfiles/Gemfile.rails7`.
        let canonical = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let dir = canonical(match path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        });
        let manifest = dir.join(path.file_name().unwrap_or_default());
        if dir != canonical(&root) && !is_engine_gemspec(&root, &manifest) && !is_evaled(path) {
            return Err(Failure::Usage.error(subdirectory_refusal(&root, path)));
        }
        // One manifest named twice is graded once.
        if !seen.insert(manifest.clone()) {
            continue;
        }
        if let Some(code) = autoindex::ensure(out, &store, &root, Need::Whole)? {
            return Ok(code);
        }
        let root_str = root.to_string_lossy().into_owned();
        if !store.has_checkout(&root_str)? {
            return not_indexed(out, &root, &store);
        }
        if let Some(warming) = store.warming(&root_str)? {
            crate::usage::outcome(Outcome::NotIndexed);
            let reason = format!(
                "the index has read {} of {} files, and a use may be in one not read yet",
                warming.read, warming.of
            );
            match out {
                Output::Text => println!("--dead lists nothing until the index ends: {reason}"),
                _ => emit_json(
                    out,
                    &serde_json::json!({
                        "status": "warming",
                        "repo": root_str,
                        "reason": reason,
                        "warming": warming_note(&root_str, &warming),
                        "candidates": [],
                    }),
                )?,
            }
            return Ok(ExitCode::from(2));
        }
        let probe = probe(&store, &root);
        freshen(out, &mut store, &root, None, probe);
        answering_in(&store, &root_str);
        graded.push(grade(&store, &root, &manifest)?);
    }
    if graded.len() > 1 {
        TEXT_ABSOLUTE.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    // A gem line two manifests reach (a Gemfile's `gemspec` and the gemspec
    // named too) is graded once, by the file that declares it.
    let mut rows: Vec<GemRow> = Vec::new();
    let mut referenced: Vec<Evidence> = Vec::new();
    let mut declared_keys = HashSet::new();
    let mut transitive_by_root = BTreeMap::new();
    for one in graded {
        for row in one.rows {
            if !rows
                .iter()
                .any(|r| r.path == row.path && r.name == row.name)
            {
                rows.push(row);
            }
        }
        for seen in one.referenced {
            if !referenced.iter().any(|r| r.name == seen.name) {
                referenced.push(seen);
            }
        }
        declared_keys.extend(one.declared);
        transitive_by_root.insert(one.root, one.transitive);
    }
    let declared = declared_keys.len();
    let transitive: usize = transitive_by_root.values().sum();
    let count = |keep: &dyn Fn(&GemRow) -> bool| rows.iter().filter(|r| keep(r)).count();
    let summary = Summary {
        candidates: rows.len(),
        declared,
        referenced: referenced.len(),
        transitive,
        tiers: TIERS
            .iter()
            .map(|tier| (tier_name(*tier), count(&|r| r.tier == *tier)))
            .collect(),
        confidence: [("clear", Grade::Clear), ("lower", Grade::Lower)]
            .into_iter()
            .map(|(name, grade)| (name, count(&|r| r.confidence == grade)))
            .collect(),
    };
    let found = !rows.is_empty();
    if out != Output::Text {
        let answer = serde_json::json!({
            "summary": summary,
            "referenced": referenced,
            "candidates": null,
        });
        emit_listing(out, answer, "candidates", &rows)?;
        return Ok(exit_on(found));
    }
    for row in &rows {
        println!(
            "{:<16} {}  gem {}  — {}{}",
            tier_name(row.tier),
            format_args!("{}:{}", shown(&row.path), row.line),
            row.name,
            row.reason,
            match row.caveat.as_str() {
                "" => String::new(),
                why => format!("   (lower confidence: {why})"),
            }
        );
    }
    if explain {
        if found {
            println!();
        }
        for seen in &referenced {
            println!(
                "referenced       gem {}  — {} `{}` at {}:{}",
                seen.name,
                seen.by,
                seen.what,
                shown(&seen.path),
                seen.line
            );
        }
    }
    let tiers: Vec<String> = TIERS
        .iter()
        .map(|tier| (tier, summary.tiers[tier_name(*tier)]))
        .filter(|(_, n)| *n > 0)
        .map(|(tier, n)| format!("{n} {}", tier_name(*tier)))
        .collect();
    println!(
        "\n{} of {} declared gem(s) are candidates{}{}; {} referenced{}",
        rows.len(),
        summary.declared,
        match tiers.is_empty() {
            true => String::new(),
            false => format!(": {}", tiers.join(", ")),
        },
        match found {
            true => format!(
                " ({} clear, {} lower)",
                summary.confidence["clear"], summary.confidence["lower"]
            ),
            false => String::new(),
        },
        summary.referenced,
        match explain {
            true => String::new(),
            false => " (--explain lists how)".to_string(),
        }
    );
    Ok(exit_on(found))
}

/// A gemspec in the directory of a path gem of the root bundle (rails'
/// `activesupport/activesupport.gemspec`) — one its lockfile names, or with
/// none, its Gemfile: its gems are the root bundle's, which the index
/// located.
fn is_engine_gemspec(root: &Path, manifest: &Path) -> bool {
    let Some(dir) = manifest.parent() else {
        return false;
    };
    if manifest.extension().is_none_or(|e| e != "gemspec") {
        return false;
    }
    let (gemfile, lockfile) = crate::gems::bundle_files(root);
    let lock = crate::scan::read_text(lockfile).ok();
    let declared = declared_in(root, &gemfile);
    path_gem_dirs(root, lock.as_deref(), &gemfile, &declared)
        .iter()
        .any(|(_, gem)| std::fs::canonicalize(gem).is_ok_and(|gem| gem == dir))
}

/// Why a second bundle's file (`Gemfile.next`) is not graded: the index
/// located the root's bundle, or none when the root has no Gemfile — though
/// `.bundle/config` may select this one, which the index does not follow.
fn other_bundle_refusal(path: &Path) -> String {
    let root = named_checkout(path).ok();
    let root_bundle = root
        .as_deref()
        .is_some_and(|root| crate::gems::bundle_files(root).0.is_file());
    if root_bundle {
        return format!(
            "--dead grades the bundle at a checkout's root, whose gems the index located; \
             {} is another bundle's, whose gems it did not locate: pass the root's Gemfile",
            path.display()
        );
    }
    let selected = root
        .as_deref()
        .and_then(crate::gems::configured_gemfile)
        .map(|g| format!(" (its .bundle/config selects {g} by BUNDLE_GEMFILE, which the index does not follow)"))
        .unwrap_or_default();
    format!(
        "--dead grades the bundle at a checkout's root, whose gems the index locates, and \
         the root has no Gemfile or gems.rb{selected}; {} is a bundle the index did not locate",
        path.display()
    )
}

/// Why a manifest in a subdirectory is not graded: its bundle has a lock of
/// its own, which the index did not locate, and graded against the root's
/// every answer would come from the wrong gems and the wrong code.
fn subdirectory_refusal(root: &Path, path: &Path) -> String {
    let shown = crate::core::paths::pretty(&root.to_string_lossy());
    match crate::gems::bundle_files(root).0.is_file() {
        true => format!(
            "--dead grades the bundle at a checkout's root, whose gems the index located; \
             {} is in a subdirectory of {shown}, a bundle of its own that it did not locate",
            path.display()
        ),
        false => format!(
            "--dead grades the bundle at a checkout's root, whose gems the index locates, \
             and {shown} has none; {} is in a subdirectory, a bundle the index did not locate",
            path.display()
        ),
    }
}

/// Counts `evidence` for its gem, unless it is in the gem's own files: a
/// path gem inside the checkout naming itself is no use of it.
fn credit(uses: &mut HashMap<String, Uses>, layers: &Layers, evidence: Evidence) {
    let tests = in_tests(&evidence.path);
    credit_as(uses, layers, evidence, tests);
}

/// As `credit`, counted as a use by the tests or not whatever its path.
fn credit_as(uses: &mut HashMap<String, Uses>, layers: &Layers, evidence: Evidence, tests: bool) {
    if layers.of(&evidence.path) == Layer::Gem(evidence.name.clone()) {
        return;
    }
    uses.entry(evidence.name.clone())
        .or_default()
        .add_as(evidence, tests);
}

const TIERS: [DeadTier; 3] = [
    DeadTier::Unreferenced,
    DeadTier::TestOnly,
    DeadTier::ConventionOnly,
];

fn tier_name(tier: DeadTier) -> &'static str {
    match tier {
        DeadTier::Unreferenced => "unreferenced",
        DeadTier::TestOnly => "test-only",
        _ => "convention-only",
    }
}

struct Graded {
    root: String,
    rows: Vec<GemRow>,
    referenced: Vec<Evidence>,
    /// Each declaration, as the declaring file's path and the gem's name.
    declared: Vec<(String, String)>,
    transitive: usize,
}

/// The gems one manifest declares, each weighed against its checkout.
fn grade(store: &Store, root: &Path, manifest: &Path) -> anyhow::Result<Graded> {
    let root_str = root.to_string_lossy().into_owned();
    let declared = declared_in(root, manifest);
    let mut installed: HashMap<String, String> = store.gem_names(&root_str)?.into_iter().collect();
    let lock_text = crate::scan::read_text(crate::gems::bundle_files(root).1).ok();
    let mut in_checkout = HashSet::new();
    let path_gems = path_gems(root, lock_text.as_deref(), manifest, &declared);
    for (name, dir) in path_gems.found {
        in_checkout.insert(dir.clone());
        installed.entry(name).or_insert(dir);
    }
    let lock = match &lock_text {
        Some(text) => Lock::parse(text),
        None => Lock::from_installed(&installed),
    };
    let direct: HashSet<&str> = declared.iter().map(|(d, _)| d.name.as_str()).collect();
    let transitive = installed
        .keys()
        .chain(lock.needs.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|name| !direct.contains(name.as_str()))
        .count();
    let tree = build_tree(store, &root_str)?;
    let default: HashMap<String, String> = store
        .default_gem_files(&root_str)?
        .into_iter()
        .filter(|(name, _)| direct.contains(name.as_str()) || lock.needs.contains_key(name))
        .map(|(name, path)| (path, name))
        .collect();
    let layers = Layers::new(&root_str, &installed, default, in_checkout);
    let owned = Owned::new(&tree, &layers, &lock);
    let mut uses: HashMap<String, Uses> = HashMap::new();
    let manifests: HashSet<String> = declared
        .iter()
        .map(|(_, file)| file.clone())
        .chain(["Gemfile", "gems.rb", "Gemfile.lock", "gems.locked"].map(String::from))
        .collect();
    let at = |path: &str| format!("{root_str}/{path}");

    let scripts = Scripts::read(root);
    // Constants: each reference resolved as Ruby looks it up, credited to
    // the gem that owns what it names or a namespace holding it.
    let mut resolved: HashMap<(String, Vec<String>), Option<String>> = HashMap::new();
    let refs = store
        .all_const_refs(&root_str)?
        .into_iter()
        .filter(|r| !manifests.contains(&r.path))
        .chain(scripts.const_refs());
    for row in refs {
        let written = row.name.trim_start_matches("::");
        if !written
            .split("::")
            .any(|segment| owned.leaves.contains(segment))
        {
            continue;
        }
        let fqn = resolved
            .entry((row.name.clone(), row.nesting.clone()))
            .or_insert_with(|| tree.resolve(&row.name, &row.nesting).fqn)
            .clone()
            .unwrap_or_else(|| written.to_string());
        for gem in owned.of_name(&fqn) {
            credit(
                &mut uses,
                &layers,
                Evidence {
                    name: gem.clone(),
                    by: "constant",
                    what: fqn.clone(),
                    path: at(&row.path),
                    line: row.line,
                },
            );
        }
    }
    // A class the app reopens is used: `module Devise` in an initializer.
    // A path gem inside the checkout reopening another gem's class counts.
    for (fqn, gems) in &owned.by_fqn {
        let sites: Vec<_> = tree
            .sites(fqn)
            .into_iter()
            .filter(|site| {
                layers.is_checkouts(&site.path)
                    && site
                        .path
                        .strip_prefix(layers.app.as_str())
                        .is_some_and(|relative| !manifests.contains(relative))
            })
            .collect();
        for gem in gems {
            let own = Layer::Gem(gem.clone());
            let Some(site) = sites.iter().find(|site| layers.of(&site.path) != own) else {
                continue;
            };
            credit(
                &mut uses,
                &layers,
                Evidence {
                    name: gem.clone(),
                    by: "declaration",
                    what: fqn.clone(),
                    path: site.path.clone(),
                    line: site.line,
                },
            );
        }
    }

    // Requires: a literal path one of the gems' files answers to.
    let mut features = Features::new(store, &layers, &installed)?;
    features.names.extend(
        direct
            .iter()
            .map(|n| n.to_string())
            .chain(lock.needs.keys().cloned()),
    );
    let requiring = store.files_calling_any(
        &root_str,
        &["require", "load", "autoload"].map(String::from),
    )?;
    let mut sources: Vec<(String, Vec<u8>)> = requiring
        .into_iter()
        .filter(|path| !manifests.contains(path))
        .filter_map(|path| {
            let raw = crate::scan::read_source(root.join(&path)).ok()?;
            Some((path, raw))
        })
        .collect();
    sources.extend(scripts.sources());
    // A path gem's require loads what it names only once that gem is
    // loaded: weighed below, when the rows are known (DEC-770).
    // Each with whether it is credited already as the tests' use.
    let mut pending: Vec<(String, Evidence, bool)> = Vec::new();
    for (path, raw) in &sources {
        let requirer = match layers.of(&at(path)) {
            Layer::Gem(gem) => Some(gem),
            _ => None,
        };
        let source = crate::extract::ruby_source(path, raw);
        for required in crate::query::require::requires_in(&source) {
            if required.anchor.is_some()
                || required.verb == crate::query::require::Verb::RequireRelative
            {
                continue;
            }
            let line = raw[..required.span.start.min(raw.len())]
                .iter()
                .filter(|b| **b == b'\n')
                .count() as u32
                + 1;
            for gem in features.of(&required.path) {
                let evidence = Evidence {
                    name: gem,
                    by: "require",
                    what: required.path.clone(),
                    path: at(path),
                    line,
                };
                match &requirer {
                    Some(by) => pending.push((by.clone(), evidence, false)),
                    None => credit(&mut uses, &layers, evidence),
                }
            }
        }
    }
    for (path, required, line) in scripts.word_requires() {
        for gem in features.of(&required) {
            credit(
                &mut uses,
                &layers,
                Evidence {
                    name: gem,
                    by: "require",
                    what: required.clone(),
                    path: at(&path),
                    line,
                },
            );
        }
    }

    // Methods: a name few gems define, in a module of their own or on a
    // class they reopen, which the checkout calls. A gem's own class's
    // methods are reached through its constant, which is counted above.
    let defines = method_names(store, &root_str, &layers)?;
    let never_loaded: HashSet<&str> = declared
        .iter()
        .filter(|(d, _)| d.require == Require::Off)
        .map(|(d, _)| d.name.as_str())
        .collect();
    let names_called: Vec<(String, String, bool)> = store
        .called_names(&root_str)?
        .into_iter()
        .filter(|(_, path, _)| !manifests.contains(path))
        .collect();
    // Each symbol the checkout writes, with the first file: a plugin's
    // base may load it by one (`provider :saml`).
    let mut symbols: HashMap<String, String> = HashMap::new();
    for (name, path, symbol) in &names_called {
        if *symbol && !in_tests(path) {
            symbols.entry(name.clone()).or_insert_with(|| path.clone());
        }
    }
    let mut called: Vec<(String, String)> = names_called
        .into_iter()
        .filter(|(name, _, _)| defines.contains_key(name))
        .map(|(name, path, _)| (name, path))
        .collect();
    called.extend(
        scripts
            .calls()
            .filter(|(name, _)| defines.contains_key(name)),
    );
    let mut asked: HashSet<(String, bool)> = HashSet::new();
    let mut in_core: HashMap<String, bool> = HashMap::new();
    let rbs_core = installed
        .get("rbs")
        .map(|dir| rbs_core_names(&Path::new(dir).join("core")))
        .unwrap_or_default();
    for (name, path) in called {
        if !asked.insert((name.clone(), in_tests(&path))) {
            continue;
        }
        let defined = &defines[&name];
        // Core's methods are not in the store; the tree knows them.
        let core = *in_core.entry(name.clone()).or_insert_with(|| {
            rbs_core.contains(&name)
                || tree
                    .named(&name)
                    .iter()
                    .any(|def| layers.of(&def.site.path) == Layer::Other)
        });
        if core {
            continue;
        }
        for (gem, row) in &defined.rows {
            // Bundler never loads it, so nothing calls its methods unless
            // a `require` of it is seen, which credits it already.
            if never_loaded.contains(gem.as_str()) {
                continue;
            }
            let owner = crate::tree::ruby_owner(&tree.owner(row)).to_string();
            let mixin = tree.kind_of(&owner) == Some("module");
            if !mixin && owned.owns(gem, &owner) {
                continue;
            }
            credit(
                &mut uses,
                &layers,
                Evidence {
                    name: gem.clone(),
                    by: "method",
                    what: name.clone(),
                    path: at(&path),
                    line: 0,
                },
            );
        }
    }
    for found in uses.values_mut() {
        for evidence in [&mut found.live, &mut found.first_test]
            .into_iter()
            .flatten()
        {
            if evidence.by == "method" {
                evidence.line = call_line(&evidence.path, &evidence.what).unwrap_or(1);
            }
        }
    }

    // A gem the app uses through another: credited to the one declared gem
    // that brings it, when no gem already credited brings it too.
    let direct_names: Vec<String> = declared.iter().map(|(d, _)| d.name.clone()).collect();
    let closures: HashMap<&str, BTreeSet<String>> = direct_names
        .iter()
        .map(|name| (name.as_str(), lock.closure(name)))
        .collect();
    let by_name: HashMap<&str, &Declaration> =
        declared.iter().map(|(d, _)| (d.name.as_str(), d)).collect();
    let mut shared: HashMap<String, (String, Vec<String>)> = HashMap::new();
    loop {
        let used_through: Vec<String> = uses
            .keys()
            .filter(|gem| !direct.contains(gem.as_str()))
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        loop {
            let credited = |name: &str, uses: &HashMap<String, Uses>| {
                uses.get(name).is_some_and(|u| u.any().is_some())
            };
            let mut moved = false;
            for name in &direct_names {
                if credited(name, &uses) {
                    continue;
                }
                for brought in closures[name.as_str()]
                    .iter()
                    .filter(|b| used_through.contains(b))
                {
                    let kept = direct_names.iter().any(|other| {
                        other != name
                            && credited(other, &uses)
                            && closures[other.as_str()].contains(brought)
                    });
                    if kept {
                        continue;
                    }
                    let holders: Vec<String> = direct_names
                        .iter()
                        .filter(|other| {
                            *other != name && closures[other.as_str()].contains(brought)
                        })
                        .cloned()
                        .collect();
                    if holders.is_empty() {
                        let first = uses[brought].any().cloned();
                        if let Some(first) = first {
                            uses.entry(name.clone()).or_default().add(Evidence {
                                name: name.clone(),
                                by: "dependency",
                                what: format!("{brought} ({} `{}`)", first.by, first.what),
                                ..first
                            });
                            moved = true;
                            break;
                        }
                    } else {
                        shared
                            .entry(name.clone())
                            .or_insert_with(|| (brought.clone(), holders));
                    }
                }
            }
            if !moved {
                break;
            }
        }
        // A requirer that is a gem of its own and not a row is loaded; one
        // only the tests use, only by them, until it is used outside them.
        let mut moved = false;
        let mut waiting = Vec::new();
        for (by, evidence, by_tests) in pending {
            let live = match by_name.get(by.as_str()) {
                None => true,
                Some(d) if is_row(&uses, d) => {
                    waiting.push((by, evidence, by_tests));
                    continue;
                }
                Some(_) => uses.get(&by).is_some_and(|u| u.live.is_some()),
            };
            if live {
                credit(&mut uses, &layers, evidence);
                moved = true;
                continue;
            }
            if !by_tests {
                credit_as(&mut uses, &layers, evidence.clone(), true);
                moved = true;
            }
            waiting.push((by, evidence, true));
        }
        pending = waiting;
        if !moved {
            break;
        }
    }

    let texts = Texts::read(root, &manifests);
    let is_row = |declaration: &Declaration| is_row(&uses, declaration);
    let unused: Vec<&Declaration> = declared
        .iter()
        .map(|(d, _)| d)
        .filter(|d| is_row(d))
        .collect();
    // A default gem counts too: rubocop declares ostruct for YARD, which
    // requires it and a newer Ruby no longer ships it.
    let soft = required_by_bundle(store, &layers, &installed, &lock, &unused)?;
    // The gem each gemspec declaring rows is: its dependencies are what it
    // needs, not another gem's need that keeps them in the bundle.
    let declaring: HashMap<&String, Option<String>> = declared
        .iter()
        .map(|(_, file)| file)
        .filter(|file| file.ends_with(".gemspec"))
        .map(|file| {
            let raw = crate::scan::read_source(root.join(file)).ok();
            // A name computed (`File.basename(__FILE__, ".gemspec")`) is
            // most often the file's.
            let stem = Path::new(file)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned());
            (
                file,
                raw.and_then(|raw| gemfile::gemspec_name(&raw)).or(stem),
            )
        })
        .collect();
    let mut rows = Vec::new();
    let mut referenced = Vec::new();
    for (declaration, file) in &declared {
        let name = &declaration.name;
        let found = uses.get(name);
        let tested_only = found.is_some_and(|u| u.live.is_none() && u.tests > 0);
        if !is_row(declaration) {
            if let Some(evidence) = found.and_then(|u| u.any()) {
                referenced.push(Evidence {
                    name: name.clone(),
                    ..evidence.clone()
                });
            }
            continue;
        }
        let gem_dir = installed.get(name).cloned();
        let spec = gem_dir
            .as_deref()
            .map(|dir| installed_spec(dir, name))
            .unwrap_or_default();
        let mut row = GemRow {
            kind: "gem",
            name: name.clone(),
            path: at(file),
            line: declaration.line,
            col: declaration.col,
            tier: DeadTier::Unreferenced,
            confidence: Grade::Clear,
            caveat: String::new(),
            reason: String::new(),
            version: gem_dir.as_deref().and_then(|dir| version_of(dir, name)),
            gem_dir: gem_dir.clone(),
            groups: declaration.groups.clone(),
            bundler_require: match declaration.require {
                Require::Auto => "name",
                Require::Off => "off",
                Require::Paths(_) => "paths",
            },
            needed_by: lock
                .needed_by(name)
                .into_iter()
                .filter(|by| installed.contains_key(by) || lock.needs.contains_key(by))
                .filter(|by| {
                    declaring
                        .get(file)
                        .is_none_or(|own| own.as_deref() != Some(by))
                })
                .collect(),
            executables: spec.executables.clone(),
            test_refs: found.map_or(0, |u| u.tests),
            convention: None,
        };
        let mut reason = Vec::new();
        if tested_only {
            row.tier = DeadTier::TestOnly;
            let first = found
                .and_then(|u| u.first_test.as_ref())
                .expect("a test use");
            reason.push(format!(
                "only tests use it ({}; first {} `{}` at {}:{}), yet it is in {}",
                row.test_refs,
                first.by,
                first.what,
                shown(&first.path),
                first.line,
                match declaration.groups.is_empty() {
                    true => "the default group".to_string(),
                    false => format!("group {}", declaration.groups.join(", ")),
                }
            ));
        } else if let Some(required) = soft.get(name).filter(|r| !r.by_row) {
            let Required { by, path, line, .. } = required;
            row.tier = DeadTier::ConventionOnly;
            reason.push(format!(
                "no Ruby of the checkout uses it, but {by} requires it at {}:{line}, \
                 without depending on it",
                shown(path)
            ));
            row.convention = Some(Named {
                by: "gem",
                path: path.clone(),
                line: *line,
                text: format!("{by} requires it"),
            });
        } else if let Some((short, path)) = short_name(name, &lock, &installed)
            .and_then(|short| Some((short.clone(), symbols.get(&short)?.clone())))
        {
            row.tier = DeadTier::ConventionOnly;
            let line = call_line(&at(&path), &short).unwrap_or(1);
            reason.push(format!(
                "no Ruby uses it, but `:{short}` at {}:{line} may name it to {}",
                path,
                plugin_of(name, &lock, &installed).unwrap_or_default()
            ));
            row.convention = Some(Named {
                by: "symbol",
                path: at(&path),
                line,
                text: format!(":{short}"),
            });
        } else if let Some(named) = texts.naming(&tokens(declaration, &spec.executables)) {
            row.tier = DeadTier::ConventionOnly;
            reason.push(format!(
                "no Ruby uses it, but {} names it at {}:{} (`{}`)",
                match named.by {
                    "command" => "a command",
                    _ => "a config",
                },
                shown(&named.path),
                named.line,
                named.text
            ));
            row.convention = Some(Named {
                path: at(&named.path),
                ..named
            });
        } else {
            reason.push(
                "nothing names a constant it defines, requires a file of it, or calls a \
                 method only it defines"
                    .to_string(),
            );
        }
        if declaration.require == Require::Off {
            reason.push("`require: false`, so Bundler never loads it".to_string());
        }
        if !row.needed_by.is_empty() {
            reason.push(format!(
                "{} needs it too, so the bundle keeps it",
                row.needed_by.join(", ")
            ));
        }
        row.reason = reason.join("; ");
        let caveats = caveats(Weigh {
            declaration,
            gem_dir: gem_dir.as_deref(),
            spec: &spec,
            convention: row.convention.is_some(),
            owned: &owned,
            layers: &layers,
            store,
            tree: &tree,
            lock: &lock,
            installed: &installed,
            shared: shared.get(name),
            same_dir: path_gems.shared.get(name),
        })?;
        let mut caveats = caveats;
        if let Some(required) = soft.get(name).filter(|r| r.by_row) {
            caveats.push(format!(
                "{}, itself a row, requires it ({}:{}), and loads it if anything loads {0}",
                required.by,
                shown(&required.path),
                required.line
            ));
        }
        if !caveats.is_empty() {
            row.confidence = Grade::Lower;
            row.caveat = caveats.join("; ");
        }
        rows.push(row);
    }
    Ok(Graded {
        rows,
        referenced,
        declared: declared
            .iter()
            .map(|(d, file)| (at(file), d.name.clone()))
            .collect(),
        transitive,
        root: root_str,
    })
}

/// A row: no use outside the tests, unless it is in a group for them.
fn is_row(uses: &HashMap<String, Uses>, declaration: &Declaration) -> bool {
    let found = uses.get(&declaration.name);
    let tested_only = found.is_some_and(|u| u.live.is_none() && u.tests > 0);
    let in_test_group = declaration
        .groups
        .iter()
        .any(|g| g == "test" || g == "development");
    found.and_then(|u| u.live.as_ref()).is_none() && !(tested_only && in_test_group)
}

/// The candidates a gem of the bundle requires without depending on them —
/// lograge's `require "logstash-event"` for its Logstash formatter — each
/// with that gem, file and line: an optional dependency, which the app
/// declares for it.
fn required_by_bundle(
    store: &Store,
    layers: &Layers,
    installed: &HashMap<String, String>,
    lock: &Lock,
    candidates: &[&Declaration],
) -> anyhow::Result<HashMap<String, Required>> {
    let mut wanted: HashMap<String, String> = HashMap::new();
    for declaration in candidates {
        let name = &declaration.name;
        let mut paths = vec![name.clone(), name.replace('-', "/")];
        if let Require::Paths(listed) = &declaration.require {
            paths.extend(listed.iter().cloned());
        }
        if let Some(dir) = installed.get(name) {
            let (root, prefix) = layers.store_root(dir);
            paths.extend(store.file_map(root)?.into_keys().filter_map(|path| {
                let feature = path
                    .strip_prefix(prefix.as_str())?
                    .strip_prefix("lib/")?
                    .strip_suffix(".rb")?;
                (!feature.contains('/')).then(|| feature.to_string())
            }));
        }
        for path in paths {
            wanted.entry(path).or_insert_with(|| name.clone());
        }
    }
    let mut found: HashMap<String, Required> = HashMap::new();
    if wanted.is_empty() {
        return Ok(found);
    }
    // Each require of a candidate by another gem, in gem order.
    let mut hits: Vec<(String, Required)> = Vec::new();
    let mut gems: Vec<(&String, &String)> = installed.iter().collect();
    gems.sort();
    for (gem, dir) in gems {
        let (root, prefix) = layers.store_root(dir);
        let requiring = store.files_calling(root, "require")?;
        for path in requiring.iter().filter(|p| p.starts_with(prefix.as_str())) {
            let Ok(raw) = crate::scan::read_source(Path::new(root).join(path)) else {
                continue;
            };
            let mentions = wanted.iter().any(|(needle, candidate)| {
                candidate != gem && raw.windows(needle.len()).any(|w| w == needle.as_bytes())
            });
            if !mentions {
                continue;
            }
            for required in crate::query::require::requires_in(&raw) {
                // minitest's `require_relative "parallel"` is its own file.
                if required.anchor.is_some()
                    || required.verb == crate::query::require::Verb::RequireRelative
                {
                    continue;
                }
                let Some(candidate) = wanted.get(&required.path) else {
                    continue;
                };
                if candidate == gem || lock.closure(gem).contains(candidate) {
                    continue;
                }
                let line = raw[..required.span.start.min(raw.len())]
                    .iter()
                    .filter(|b| **b == b'\n')
                    .count() as u32
                    + 1;
                hits.push((
                    candidate.clone(),
                    Required {
                        by: gem.clone(),
                        path: format!("{root}/{path}"),
                        line,
                        by_row: false,
                    },
                ));
            }
        }
    }
    // A requirer that is not a row is loaded, and so is a row it requires:
    // hchain_d (used) requires hchain_b, which requires hchain_c.
    let rows: HashSet<&str> = candidates.iter().map(|d| d.name.as_str()).collect();
    let mut loaded: HashSet<&str> = installed
        .keys()
        .map(String::as_str)
        .filter(|gem| !rows.contains(gem))
        .collect();
    loop {
        let mut moved = false;
        for (candidate, required) in &hits {
            if found.contains_key(candidate) || !loaded.contains(required.by.as_str()) {
                continue;
            }
            found.insert(candidate.clone(), required.clone());
            loaded.insert(candidate.as_str());
            moved = true;
        }
        if !moved {
            break;
        }
    }
    // One only rows require lowers it: nothing is seen to load them,
    // though a Railtie may (better_errors, binding_of_caller).
    for (candidate, required) in hits {
        found.entry(candidate).or_insert(Required {
            by_row: true,
            ..required
        });
    }
    Ok(found)
}

/// Another gem's require of a candidate: who, where, and whether the
/// requirer is itself a row, which only lowers the candidate.
#[derive(Clone)]
struct Required {
    by: String,
    path: String,
    line: u32,
    by_row: bool,
}

/// The path gems inside the checkout (not the checkout itself), each with
/// its directory under `root`: the lockfile's `PATH` specs, and a `path:`
/// the lockfile does not name. Their files are indexed as the checkout's,
/// and are theirs — unless two gems are found in one directory, whose files
/// are told apart from neither's: those are in `shared`, each with the
/// directory and the others there.
struct PathGems {
    found: Vec<(String, String)>,
    shared: HashMap<String, (String, Vec<String>)>,
}

fn path_gems(
    root: &Path,
    lockfile: Option<&str>,
    manifest: &Path,
    declared: &[(Declaration, String)],
) -> PathGems {
    let mut found = path_gem_dirs(root, lockfile, manifest, declared);
    let mut by_dir: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, dir) in &found {
        by_dir.entry(dir.clone()).or_default().push(name.clone());
    }
    let mut shared = HashMap::new();
    for (dir, names) in by_dir.into_iter().filter(|(_, names)| names.len() > 1) {
        for name in &names {
            let others = names.iter().filter(|n| *n != name).cloned().collect();
            shared.insert(name.clone(), (dir.clone(), others));
        }
    }
    found.retain(|(name, _)| !shared.contains_key(name));
    PathGems { found, shared }
}

fn path_gem_dirs(
    root: &Path,
    lockfile: Option<&str>,
    manifest: &Path,
    declared: &[(Declaration, String)],
) -> Vec<(String, String)> {
    let checkout = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut found = lockfile
        .map(|text| crate::gems::path_gems_in(root, text))
        .unwrap_or_default();
    for (declaration, _) in declared {
        let Some(path) = &declaration.path else {
            continue;
        };
        if found.iter().any(|(name, _)| *name == declaration.name) {
            continue;
        }
        let Ok(source) = std::fs::canonicalize(manifest.parent().unwrap_or(root).join(path)) else {
            continue;
        };
        // A `path` block names a directory several gems are found in.
        let sole = declared
            .iter()
            .filter(|(d, _)| d.path.as_ref() == Some(path))
            .count()
            == 1;
        let Some(dir) = crate::gems::path_gem_dir(source, &declaration.name, sole) else {
            continue;
        };
        if dir.starts_with(&checkout) && dir != checkout {
            found.push((declaration.name.clone(), dir));
        }
    }
    found
        .into_iter()
        .filter_map(|(name, dir)| {
            let relative = dir.strip_prefix(&checkout).ok()?;
            Some((name, root.join(relative).to_string_lossy().into_owned()))
        })
        .collect()
}

/// The gems the manifest declares, each with the file (relative to the
/// checkout) that does: a Gemfile's `gemspec` adds the checkout's gemspecs'.
/// A name declared twice is graded at its first line.
fn declared_in(root: &Path, manifest: &Path) -> Vec<(Declaration, String)> {
    let relative = |path: &Path| {
        path.strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned()
    };
    let read = |path: &Path| {
        crate::scan::read_source(path)
            .map(|raw| gemfile::declarations(&raw))
            .unwrap_or_default()
    };
    let mut files = Vec::new();
    let mut gemspecs = Vec::new();
    // The manifest, then each file it evals (`eval_gemfile`), in turn.
    let mut todo = vec![manifest.to_path_buf()];
    let mut read_once = HashSet::new();
    while let Some(file) = todo.pop() {
        if !read_once.insert(std::fs::canonicalize(&file).unwrap_or_else(|_| file.clone())) {
            continue;
        }
        let found = read(&file);
        let dir = file.parent().unwrap_or(root).to_path_buf();
        todo.extend(found.evals.iter().rev().map(|e| dir.join(e)));
        gemspecs.extend(found.gemspecs.iter().map(|g| dir.join(g)));
        files.push((file, found.gems));
    }
    for dir in gemspecs {
        let mut specs: Vec<PathBuf> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "gemspec"))
            .collect();
        specs.sort();
        for spec in specs {
            let gems = read(&spec).gems;
            files.push((spec, gems));
        }
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (file, gems) in files {
        for gem in gems {
            if seen.insert(gem.name.clone()) {
                out.push((gem, relative(&file)));
            }
        }
    }
    out
}

/// Which checkout a site's path is in: the app, a gem by name, or neither
/// (Ruby core, the stdlib).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Layer {
    App,
    Gem(String),
    /// A stub declaring what a library compiles, or what RSpec wires: it
    /// neither owns a constant nor keeps a gem from owning one.
    Stub,
    Other,
}

struct Layers {
    app: String,
    /// Gem roots, longest first, so a gem nested in another's clone wins.
    gems: Vec<(String, String)>,
    /// The stdlib's files a default gem owns, when the manifest declares
    /// that gem: Ruby's copy of json is json's, whichever copy answers.
    default: HashMap<String, String>,
    /// The directories of path gems inside the checkout, indexed with it.
    in_checkout: HashSet<String>,
}

impl Layers {
    fn new(
        app: &str,
        installed: &HashMap<String, String>,
        default: HashMap<String, String>,
        in_checkout: HashSet<String>,
    ) -> Layers {
        let mut gems: Vec<(String, String)> = installed
            .iter()
            .map(|(name, dir)| (format!("{dir}/"), name.clone()))
            .collect();
        gems.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.cmp(b)));
        Layers {
            app: format!("{app}/"),
            gems,
            default,
            in_checkout,
        }
    }

    /// Whether `path` is the checkout's own code: the app's, or a path gem's
    /// inside it — not a gem bundled into `vendor/bundle`.
    fn is_checkouts(&self, path: &str) -> bool {
        match self.of(path) {
            Layer::App => true,
            Layer::Gem(_) => self.in_checkout.iter().any(|dir| {
                path.strip_prefix(dir.as_str())
                    .is_some_and(|r| r.starts_with('/'))
            }),
            _ => false,
        }
    }

    /// The store root holding a gem's files, and the prefix of its paths
    /// there: its own, or the checkout's for a path gem inside it. A gem
    /// bundled into `vendor/bundle` is under the checkout too, but is its
    /// own root.
    fn store_root<'a>(&'a self, dir: &'a str) -> (&'a str, String) {
        let app = self.app.trim_end_matches('/');
        match dir.strip_prefix(&self.app) {
            Some(relative) if self.in_checkout.contains(dir) => (app, format!("{relative}/")),
            _ => (dir, String::new()),
        }
    }

    fn of(&self, path: &str) -> Layer {
        if let Some((_, name)) = self
            .gems
            .iter()
            .find(|(dir, _)| path.starts_with(dir.as_str()))
        {
            return Layer::Gem(name.clone());
        }
        if let Some(name) = self.default.get(path) {
            return Layer::Gem(name.clone());
        }
        if path.starts_with(&self.app) {
            return Layer::App;
        }
        if crate::tree::is_stdlib_stub(path) || crate::tree::is_rspec_stub(path) {
            return Layer::Stub;
        }
        Layer::Other
    }
}

/// Which gem each constant belongs to. One only gems declare belongs to the
/// gem every other declaring gem depends on (the one they reopen); when no
/// one gem is that, to each of them. Ruby core's and the stdlib's belong to
/// none.
struct Owned {
    by_fqn: BTreeMap<String, Vec<String>>,
    /// Each owned constant's last segment: a reference naming none of them
    /// cannot reach one, and is never resolved.
    leaves: HashSet<String>,
    /// Each gem's Railtie or Engine, with its file and line.
    railties: HashMap<String, (String, String, u32)>,
}

impl Owned {
    fn new(tree: &Tree, layers: &Layers, lock: &Lock) -> Owned {
        let mut by_fqn = BTreeMap::new();
        let mut railties = HashMap::new();
        let mut declared = tree.declared();
        declared.sort();
        for (fqn, kind) in declared {
            let sites = tree.sites(&fqn);
            let mut gems: BTreeSet<String> = BTreeSet::new();
            let mut other = false;
            for site in &sites {
                match layers.of(&site.path) {
                    Layer::Gem(name) => {
                        gems.insert(name);
                    }
                    Layer::Other => other = true,
                    Layer::App | Layer::Stub => {}
                }
            }
            if other || gems.is_empty() {
                continue;
            }
            let owners = owners_of(&fqn, &kind, &sites, gems, layers, lock);
            if owners.is_empty() {
                continue;
            }
            if kind == "class" && owners.len() == 1 && tree.inherits(&fqn, "Rails::Railtie") {
                let site = sites
                    .iter()
                    .find(|s| layers.of(&s.path) == Layer::Gem(owners[0].clone()));
                if let Some(site) = site {
                    railties.entry(owners[0].clone()).or_insert((
                        fqn.clone(),
                        site.path.clone(),
                        site.line,
                    ));
                }
            }
            by_fqn.insert(fqn, owners);
        }
        let leaves = by_fqn
            .keys()
            .map(|fqn| fqn.rsplit("::").next().unwrap_or(fqn).to_string())
            .collect();
        Owned {
            by_fqn,
            leaves,
            railties,
        }
    }

    /// The gems owning `fqn` or the nearest namespace holding it that one owns.
    fn of_name(&self, fqn: &str) -> Vec<String> {
        let mut name = fqn;
        loop {
            if let Some(gems) = self.by_fqn.get(name) {
                return gems.clone();
            }
            match name.rsplit_once("::") {
                Some((outer, _)) => name = outer,
                None => return Vec::new(),
            }
        }
    }

    fn owns(&self, gem: &str, fqn: &str) -> bool {
        self.by_fqn
            .get(fqn)
            .is_some_and(|gems| gems.iter().any(|g| g == gem))
    }
}

/// Of the gems declaring `fqn`, the one it belongs to: the one whose
/// `lib/` has the file named for it (`lib/rack.rb` for `Rack`), else the
/// one named for it, else the one every other depends on. When none picks
/// one, a class is each of theirs and a module none of theirs.
fn owners_of(
    fqn: &str,
    kind: &str,
    sites: &[crate::tree::Site],
    gems: BTreeSet<String>,
    layers: &Layers,
    lock: &Lock,
) -> Vec<String> {
    if gems.len() == 1 {
        return gems.into_iter().collect();
    }
    let flat = |s: &str| s.to_ascii_lowercase().replace('_', "");
    let wanted: Vec<String> = fqn.split("::").map(flat).collect();
    let named: BTreeSet<String> = sites
        .iter()
        .filter(|site| {
            site.path
                .rsplit_once("/lib/")
                .and_then(|(_, rest)| rest.strip_suffix(".rb"))
                .is_some_and(|rest| rest.split('/').map(flat).collect::<Vec<_>>() == wanted)
        })
        .filter_map(|site| match layers.of(&site.path) {
            Layer::Gem(name) => Some(name),
            _ => None,
        })
        .collect();
    if named.len() == 1 {
        return named.into_iter().collect();
    }
    // The gem named for it: rubocop's `RuboCop`, whose `lib/rubocop.rb`
    // only requires the files that declare it.
    let whole = fqn.to_ascii_lowercase().replace(['_', ':'], "");
    let by_name: Vec<&String> = gems
        .iter()
        .filter(|g| g.to_ascii_lowercase().replace(['_', '-'], "") == whole)
        .collect();
    if let [only] = by_name.as_slice() {
        return vec![(*only).clone()];
    }
    let base: Vec<String> = gems
        .iter()
        .filter(|g| {
            gems.iter()
                .all(|o| o == *g || lock.closure(o).contains(g.as_str()))
        })
        .cloned()
        .collect();
    match base.len() {
        1 => base,
        // A namespace several gems open is none of theirs: what each puts
        // in it is.
        _ if kind == "module" => Vec::new(),
        _ => gems.into_iter().collect(),
    }
}

/// How many gems may define a name for a call of it to count for each:
/// `push_bulk` is sidekiq-bulk's mixin and Sidekiq::Client's own method.
const FEW: usize = 3;

/// The gems defining a method name, each with one row, when no more than
/// [`FEW`] do and neither the checkout nor its Ruby's stdlib does.
struct Defines {
    rows: Vec<(String, crate::store::MethodRow)>,
}

fn method_names(
    store: &Store,
    root: &str,
    layers: &Layers,
) -> anyhow::Result<HashMap<String, Defines>> {
    let roots = store.tree_roots(root)?;
    let mut by_name: HashMap<String, Option<Defines>> = HashMap::new();
    store.each_method(&roots, |row| {
        if matches!(
            row.via.as_deref(),
            Some("private" | "protected" | "public" | "module_function")
        ) {
            return;
        }
        let entry = by_name
            .entry(row.name.clone())
            .or_insert_with(|| Some(Defines { rows: Vec::new() }));
        let Some(defines) = entry else { return };
        let Layer::Gem(gem) = layers.of(&row.path) else {
            *entry = None;
            return;
        };
        if defines.rows.iter().any(|(g, _)| *g == gem) {
            return;
        }
        if defines.rows.len() == FEW {
            *entry = None;
            return;
        }
        defines.rows.push((gem, row));
    })?;
    Ok(by_name
        .into_iter()
        .filter_map(|(name, defines)| Some((name, defines?)))
        .collect())
}

/// The method names the bundle's rbs declares for Ruby core, under its
/// `core/`. Core is read from the Ruby's own rbs, which may be older: rbs
/// 4's `Module#ruby2_keywords` is core's, not a polyfill rbs is used for.
fn rbs_core_names(dir: &Path) -> HashSet<String> {
    let mut names = HashSet::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(dir) = todo.pop() {
        for path in std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
        {
            if path.is_dir() {
                todo.push(path);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rbs") {
                continue;
            }
            let Ok(text) = crate::scan::read_text(&path) else {
                continue;
            };
            for line in text.lines() {
                let line = line.trim_start();
                let line = line
                    .strip_prefix("private ")
                    .or_else(|| line.strip_prefix("public "))
                    .unwrap_or(line);
                let Some(rest) = line.strip_prefix("def ") else {
                    continue;
                };
                let rest = rest
                    .strip_prefix("self?.")
                    .or_else(|| rest.strip_prefix("self."))
                    .unwrap_or(rest);
                if let Some((name, _)) = rest.split_once(':') {
                    names.insert(name.trim().to_string());
                }
            }
        }
    }
    names
}

/// The first line of `path` that calls `name`.
fn call_line(path: &str, name: &str) -> Option<u32> {
    let raw = crate::scan::read_source(path).ok()?;
    let facts = crate::extract::extract_file(path, &raw);
    facts
        .calls
        .iter()
        .filter(|call| call.name == name)
        .map(|call| call.pos.line)
        .min()
}

/// Each `require` path a gem's files answer to, from what the index holds
/// of its `lib/`: `lib/kaminari/config.rb` is `kaminari/config`.
struct Features {
    by_path: HashMap<String, Vec<String>>,
    names: HashSet<String>,
}

impl Features {
    fn new(
        store: &Store,
        layers: &Layers,
        installed: &HashMap<String, String>,
    ) -> anyhow::Result<Features> {
        let mut by_path: HashMap<String, Vec<String>> = HashMap::new();
        for (name, dir) in installed {
            let (root, prefix) = layers.store_root(dir);
            for path in store.file_map(root)?.into_keys() {
                let Some(feature) = path
                    .strip_prefix(prefix.as_str())
                    .and_then(|p| p.strip_prefix("lib/"))
                    .and_then(|p| p.strip_suffix(".rb"))
                else {
                    continue;
                };
                by_path
                    .entry(feature.to_string())
                    .or_default()
                    .push(name.clone());
            }
        }
        for gems in by_path.values_mut() {
            gems.sort();
        }
        Ok(Features {
            by_path,
            names: installed.keys().cloned().collect(),
        })
    }

    /// The gems a required path names: the one whose `lib/` holds it, else
    /// a gem named as its first segment is (a compiled `pg_ext`, a file
    /// the index leaves out).
    fn of(&self, required: &str) -> Vec<String> {
        let path = required.strip_suffix(".rb").unwrap_or(required);
        if let Some(gems) = self.by_path.get(path) {
            return gems.clone();
        }
        let head = path.split('/').next().unwrap_or(path);
        [head.to_string(), path.replace('/', "-")]
            .into_iter()
            .filter(|name| self.names.contains(name))
            .take(1)
            .collect()
    }
}

/// Ruby the index does not read: a file with no extension that Prism
/// parses without an error (`bin/setup`, `Vagrantfile`, `fastlane/Fastfile`),
/// and files Ruby in all but name — a Builder, Arbre or Jb view, a Thor
/// file or Capistrano task, an `.irbrc` — whose requires, constants and
/// calls count too. A Slim view, which nothing here parses, counts by its
/// words.
struct Scripts {
    files: Vec<(String, Vec<u8>, crate::core::Facts)>,
    /// Each Slim view's words: a constant path or a method name, by line.
    words: Vec<(String, Vec<(u32, String)>)>,
}

/// Plain Ruby the index leaves out, by extension or by name.
const PLAIN_RUBY: [&str; 7] = [
    ".builder", ".arb", ".jb", ".axlsx", ".prawn", ".thor", ".cap",
];
const PLAIN_RUBY_NAMES: [&str; 2] = [".irbrc", ".pryrc"];

impl Scripts {
    fn read(root: &Path) -> Scripts {
        let mut scripts = Scripts {
            files: Vec::new(),
            words: Vec::new(),
        };
        for path in crate::scan::listed(root, &[]) {
            let name = path.rsplit('/').next().unwrap_or(&path);
            let plain = PLAIN_RUBY.iter().any(|ext| name.ends_with(ext))
                || PLAIN_RUBY_NAMES.contains(&name);
            let slim = name.ends_with(".slim");
            // One the index reads (`Rakefile`) is in the store already.
            if !plain && !slim && (name.contains('.') || crate::scan::is_ruby(&path)) {
                continue;
            }
            let Ok(raw) = crate::scan::read_source(root.join(&path)) else {
                continue;
            };
            if slim {
                let ruby = crate::extract::slim::slim_ruby(&String::from_utf8_lossy(&raw));
                scripts.words.push((path, words(&ruby)));
                continue;
            }
            // A file with no extension is Ruby when it parses as Ruby: a
            // shebang, a Ruby DSL's own name (`Fastfile`, `Steepfile`), or
            // neither. Text that is not Ruby hardly ever parses.
            if !plain && (raw.contains(&0) || !parses_as_code(&raw)) {
                continue;
            }
            let facts = crate::extract::extract(&raw);
            scripts.files.push((path, raw, facts));
        }
        scripts
    }

    fn const_refs(&self) -> impl Iterator<Item = crate::store::ConstRefRow> + '_ {
        let parsed = self.files.iter().flat_map(|(path, _, facts)| {
            facts.const_refs.iter().map(|r| crate::store::ConstRefRow {
                path: path.clone(),
                name: r.name.clone(),
                nesting: r.nesting.clone(),
                line: r.pos.line,
            })
        });
        let worded = self.words.iter().flat_map(|(path, words)| {
            words
                .iter()
                .filter(|(_, word)| word.starts_with(|c: char| c.is_ascii_uppercase()))
                .map(|(line, word)| crate::store::ConstRefRow {
                    path: path.clone(),
                    name: word.clone(),
                    nesting: Vec::new(),
                    line: *line,
                })
        });
        parsed.chain(worded)
    }

    fn sources(&self) -> impl Iterator<Item = (String, Vec<u8>)> + '_ {
        self.files
            .iter()
            .map(|(path, raw, _)| (path.clone(), raw.clone()))
    }

    fn calls(&self) -> impl Iterator<Item = (String, String)> + '_ {
        let parsed = self.files.iter().flat_map(|(path, _, facts)| {
            facts.calls.iter().map(|c| (c.name.clone(), path.clone()))
        });
        let worded = self.words.iter().flat_map(|(path, words)| {
            words
                .iter()
                .filter(|(_, word)| !word.starts_with(|c: char| c.is_ascii_uppercase()))
                .map(|(_, word)| (word.clone(), path.clone()))
        });
        parsed.chain(worded)
    }

    /// The paths a Slim view's `require "…"` names, each with its line.
    fn word_requires(&self) -> impl Iterator<Item = (String, String, u32)> + '_ {
        self.words.iter().flat_map(|(path, words)| {
            words.iter().filter_map(|(line, word)| {
                let required = word.strip_prefix("require ")?;
                Some((path.clone(), required.to_string(), *line))
            })
        })
    }
}

/// Whether text with no extension is Ruby: it parses, and holds something
/// prose does not — a literal, a block, parentheses, a receiver, an
/// assignment, a definition. A `TODO` reading "remove Tomlrb" parses as
/// two bare calls on a constant, and is prose.
fn parses_as_code(raw: &[u8]) -> bool {
    struct Code(bool);
    impl Code {
        fn check(&mut self, node: &ruby_prism::Node<'_>) {
            use ruby_prism::Node;
            let worded = match node {
                Node::ProgramNode { .. }
                | Node::StatementsNode { .. }
                | Node::ArgumentsNode { .. }
                | Node::ConstantReadNode { .. }
                | Node::LocalVariableReadNode { .. } => true,
                Node::CallNode { .. } => node.as_call_node().is_some_and(|call| {
                    call.receiver().is_none()
                        && call.block().is_none()
                        && call.opening_loc().is_none()
                }),
                _ => false,
            };
            self.0 |= !worded;
        }
    }
    impl<'pr> ruby_prism::Visit<'pr> for Code {
        fn visit_branch_node_enter(&mut self, node: ruby_prism::Node<'pr>) {
            self.check(&node);
        }
        fn visit_leaf_node_enter(&mut self, node: ruby_prism::Node<'pr>) {
            self.check(&node);
        }
    }
    let parsed = ruby_prism::parse(raw);
    if parsed.errors().next().is_some() {
        return false;
    }
    let mut code = Code(false);
    ruby_prism::Visit::visit(&mut code, &parsed.node());
    code.0
}

/// A text's words, by line: each constant path (`Diffy::Diff`), method
/// name (`paginate`, `valid?`), and `require <path>` for a literal require.
#[expect(
    clippy::string_slice,
    reason = "a byte scan stops only at ASCII bytes, which are char boundaries"
)]
fn words(text: &str) -> Vec<(u32, String)> {
    let mut found = Vec::new();
    for (at, line) in text.lines().enumerate() {
        let line_no = at as u32 + 1;
        let bytes = line.as_bytes();
        let named = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            if !(b.is_ascii_alphabetic() || b == b'_') || (i > 0 && named(bytes[i - 1])) {
                i += 1;
                continue;
            }
            let start = i;
            while i < bytes.len() && named(bytes[i]) {
                i += 1;
            }
            if b.is_ascii_uppercase() {
                // `Diffy::Diff`: the whole path, as Ruby looks it up.
                while bytes[i..].starts_with(b"::")
                    && bytes.get(i + 2).is_some_and(|c| c.is_ascii_uppercase())
                {
                    i += 2;
                    while i < bytes.len() && named(bytes[i]) {
                        i += 1;
                    }
                }
            } else if matches!(bytes.get(i), Some(b'?' | b'!')) {
                i += 1;
            }
            // `Foo::Bar`'s `Bar` is no constant of its own. Bytes: a
            // multi-byte character may end just before `start`.
            if start >= 2 && bytes[start - 2..start] == *b"::" {
                continue;
            }
            let word = &line[start..i];
            found.push((line_no, word.to_string()));
            if word == "require" {
                let rest = line[i..].trim_start_matches([' ', '(']);
                if let Some(quote) = rest.chars().next().filter(|c| matches!(c, '"' | '\''))
                    && let Some((required, _)) = rest[1..].split_once(quote)
                {
                    found.push((line_no, format!("require {required}")));
                }
            }
        }
    }
    found
}

/// The files outside Ruby that load a gem by name: config, the commands a
/// process manager or CI runs, a linter's plugin list.
struct Texts {
    files: Vec<TextFile>,
}

/// One file's lines a name is looked for on, numbered: a Ruby file's string
/// and symbol literals (`:sidekiq`, `"opentelemetry/instrumentation/pg"`),
/// not its code, where `logger.debug?` is no mention of the debug gem; any
/// other file's lines but its comments.
struct TextFile {
    path: String,
    text: String,
    lines: Vec<(u32, String)>,
}

impl TextFile {
    fn new(path: String, text: String) -> TextFile {
        let ruby = super::built::RUBY.iter().any(|ext| path.ends_with(ext))
            || matches!(
                path.rsplit('/').next().unwrap_or(&path),
                "Rakefile" | "Guardfile" | "Capfile"
            )
            || super::views::ruby_script(&path, &text);
        let lines = match ruby {
            true => literals(text.as_bytes()),
            false => text
                .lines()
                .enumerate()
                .filter(|(_, line)| {
                    let code = line.trim_start();
                    !code.starts_with('#') && !code.starts_with("//")
                })
                .map(|(at, line)| (at as u32 + 1, line.to_ascii_lowercase()))
                .collect(),
        };
        TextFile { path, text, lines }
    }
}

/// Each string and symbol literal in Ruby source, lowercased, with its
/// line; a symbol's with its colon.
fn literals(source: &[u8]) -> Vec<(u32, String)> {
    struct Finder<'s> {
        source: &'s [u8],
        found: Vec<(u32, String)>,
    }
    impl Finder<'_> {
        fn push(&mut self, offset: usize, text: &[u8]) {
            let line = self.source[..offset.min(self.source.len())]
                .iter()
                .filter(|b| **b == b'\n')
                .count() as u32
                + 1;
            self.found
                .push((line, String::from_utf8_lossy(text).to_ascii_lowercase()));
        }
    }
    impl<'pr> ruby_prism::Visit<'pr> for Finder<'_> {
        fn visit_string_node(&mut self, node: &ruby_prism::StringNode<'pr>) {
            self.push(node.location().start_offset(), node.unescaped());
        }
        fn visit_symbol_node(&mut self, node: &ruby_prism::SymbolNode<'pr>) {
            let mut text = b":".to_vec();
            text.extend_from_slice(node.unescaped());
            self.push(node.location().start_offset(), &text);
        }
    }
    let parsed = ruby_prism::parse(source);
    let mut finder = Finder {
        source,
        found: Vec::new(),
    };
    ruby_prism::Visit::visit(&mut finder, &parsed.node());
    finder.found
}

/// At the checkout's root, by name or prefix.
const ROOT_FILES: [&str; 26] = [
    "Procfile",
    "Rakefile",
    "Makefile",
    "Dockerfile",
    "docker-compose",
    "compose.y",
    "package.json",
    ".rubocop",
    ".rspec",
    "Guardfile",
    "Capfile",
    "config.ru",
    "app.json",
    ".gitlab-ci.yml",
    ".travis.yml",
    "lefthook.yml",
    ".lefthook.yml",
    ".overcommit.yml",
    ".pre-commit-config.yaml",
    ".haml-lint.yml",
    ".erb-lint.yml",
    ".erb_lint.yml",
    ".slim-lint.yml",
    ".standard.yml",
    ".simplecov",
    ".foreman",
];

/// Directories read whole: scripts, CI, config (less locales), rake tasks.
const DIRS: [&str; 10] = [
    "bin/",
    "exe/",
    "script/",
    "scripts/",
    ".github/",
    ".circleci/",
    ".buildkite/",
    ".devcontainer/",
    "config/",
    "lib/tasks/",
];

impl Texts {
    fn read(root: &Path, manifests: &HashSet<String>) -> Texts {
        let wanted = |path: &str| {
            // Prose (an issue template naming a gem) loads nothing.
            if manifests.contains(path) || path.contains("/locales/") || path.ends_with(".md") {
                return false;
            }
            match path.contains('/') {
                false => ROOT_FILES.iter().any(|name| path.starts_with(name)),
                true => DIRS.iter().any(|dir| path.starts_with(dir)),
            }
        };
        let files = crate::scan::listed(root, &[])
            .into_iter()
            .filter(|path| wanted(path))
            .filter_map(|path| {
                let text = crate::scan::read_text(root.join(&path)).ok()?;
                Some(TextFile::new(path, text))
            })
            .collect();
        Texts { files }
    }

    /// The first file naming one of `tokens` — by its own name
    /// (`config/puma.rb`), or a word on a line that is not a comment.
    fn naming(&self, tokens: &[(String, &'static str)]) -> Option<Named> {
        for file in &self.files {
            let path = &file.path;
            let stem = path
                .rsplit('/')
                .next()
                .unwrap_or(path)
                .split('.')
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            if path.starts_with("config/")
                && let Some((_, by)) = tokens.iter().find(|(t, _)| *t == stem)
            {
                return Some(Named {
                    by,
                    path: path.clone(),
                    line: 1,
                    text: path.clone(),
                });
            }
            for (line, searched) in &file.lines {
                if let Some((_, by)) = tokens.iter().find(|(t, _)| has_word(searched, t)) {
                    let text: String = file
                        .text
                        .lines()
                        .nth(*line as usize - 1)
                        .unwrap_or_default()
                        .trim()
                        .chars()
                        .take(80)
                        .collect();
                    return Some(Named {
                        by,
                        path: path.clone(),
                        line: *line,
                        text,
                    });
                }
            }
        }
        None
    }
}

/// Whether `word` is in `line` with no name character either side; `-`
/// counts as one, so `rubocop` is not in `rubocop-rails`, and so does a
/// `.` before it: `.yardoc` is a directory, not yard.
#[expect(
    clippy::string_slice,
    reason = "offsets are where an ASCII token was found, or just past it"
)]
fn has_word(line: &str, word: &str) -> bool {
    let named = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
    line.match_indices(word).any(|(at, _)| {
        !line[..at]
            .chars()
            .next_back()
            .is_some_and(|c| named(c) || c == '.')
            && !line[at + word.len()..].chars().next().is_some_and(named)
    })
}

/// The words that name a gem outside Ruby, lowercased, each with what it
/// is: its name and spellings of it, what Bundler requires for it, the
/// adapter a database config names it by, and its commands.
fn tokens(declaration: &Declaration, executables: &[String]) -> Vec<(String, &'static str)> {
    let name = declaration.name.to_ascii_lowercase();
    let mut words: Vec<(String, &'static str)> = vec![
        (name.clone(), "config"),
        (name.replace('-', "_"), "config"),
        (name.replace('-', "/"), "config"),
        (name.replace(['-', '_'], ""), "config"),
    ];
    if let Require::Paths(paths) = &declaration.require {
        words.extend(paths.iter().map(|p| (p.to_ascii_lowercase(), "config")));
    }
    // The adapter names Active Record's database configs use.
    let adapters: &[&str] = match name.as_str() {
        "pg" => &["postgresql", "postgis", "postgres"],
        "activerecord-postgis-adapter" => &["postgis"],
        "mysql2" => &["mysql2"],
        "trilogy" => &["trilogy"],
        "sqlite3" => &["sqlite3"],
        _ => &[],
    };
    words.extend(adapters.iter().map(|a| (a.to_string(), "config")));
    words.extend(
        executables
            .iter()
            .map(|e| (e.to_ascii_lowercase(), "command")),
    );
    words.retain(|(w, _)| w.len() >= 2);
    words.dedup();
    words
}

/// What an installed gem's specification says: its commands and whether it
/// compiles. Read from rubygems' `specifications/` beside the gem, or the
/// gem's own gemspec (a git or path gem), picked as Bundler picks it; a list
/// it computes is looked for on disk.
fn installed_spec(dir: &str, gem: &str) -> InstalledSpec {
    let dir = Path::new(dir);
    let base = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let written = dir
        .parent()
        .and_then(Path::parent)
        .map(|root| root.join("specifications").join(format!("{base}.gemspec")))
        .and_then(|p| crate::scan::read_source(p).ok())
        .or_else(|| own_gemspec(dir, gem));
    let spec = written.map(|raw| gemfile::spec(&raw)).unwrap_or_default();
    let executables = spec.executables.unwrap_or_else(|| {
        let mut found: Vec<String> = ["exe", "bin"]
            .iter()
            .find_map(|bin| {
                let listed: Vec<String> = std::fs::read_dir(dir.join(bin))
                    .ok()?
                    .flatten()
                    .filter(|e| e.path().is_file())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| !matches!(n.as_str(), "console" | "setup"))
                    .collect();
                (!listed.is_empty()).then_some(listed)
            })
            .unwrap_or_default();
        found.sort();
        found
    });
    InstalledSpec {
        executables,
        compiled: spec.extensions || dir.join("ext").is_dir(),
    }
}

/// The gemspec in `dir` that is `gem`'s, of those sorted by name: the one
/// whose `name` is the gem's, else the file named for it when its name is
/// computed, else the only one. Two gems' gemspecs may share a directory
/// (`vendor/kits/a_kit.gemspec`, `b_kit.gemspec`).
fn own_gemspec(dir: &Path, gem: &str) -> Option<Vec<u8>> {
    let mut specs: Vec<(PathBuf, Vec<u8>)> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "gemspec"))
        .filter_map(|p| Some((p.clone(), crate::scan::read_source(&p).ok()?)))
        .collect();
    specs.sort_by(|a, b| a.0.cmp(&b.0));
    let named = format!("{gem}.gemspec");
    let names: Vec<Option<String>> = specs
        .iter()
        .map(|(_, raw)| gemfile::gemspec_name(raw))
        .collect();
    let at = names
        .iter()
        .position(|n| n.as_deref() == Some(gem))
        .or_else(|| {
            specs.iter().zip(&names).position(|((path, _), n)| {
                n.is_none() && path.file_name().is_some_and(|f| *f == *named)
            })
        })
        .or_else(|| (specs.len() == 1).then_some(0))?;
    Some(specs.swap_remove(at).1)
}

#[derive(Default)]
struct InstalledSpec {
    executables: Vec<String>,
    compiled: bool,
}

/// The version a gem's directory carries (`kaminari-1.2.2`), or none.
fn version_of(dir: &str, name: &str) -> Option<String> {
    let base = Path::new(dir).file_name()?.to_string_lossy().into_owned();
    base.strip_prefix(&format!("{name}-")).map(str::to_string)
}

struct Weigh<'a> {
    declaration: &'a Declaration,
    gem_dir: Option<&'a str>,
    spec: &'a InstalledSpec,
    convention: bool,
    owned: &'a Owned,
    layers: &'a Layers,
    store: &'a Store,
    tree: &'a Tree,
    lock: &'a Lock,
    installed: &'a HashMap<String, String>,
    shared: Option<&'a (String, Vec<String>)>,
    /// A path gem found in one directory with others: its directory, and them.
    same_dir: Option<&'a (String, Vec<String>)>,
}

/// What may still use a gem nothing was seen to: each a phrase naming the
/// way in and, where it can, the evidence that would settle it.
fn caveats(w: Weigh<'_>) -> anyhow::Result<Vec<String>> {
    let name = &w.declaration.name;
    let mut caveats = Vec::new();
    let loads = w.declaration.require != Require::Off;
    let Some(dir) = w.gem_dir else {
        if let Some((dir, others)) = w.same_dir {
            caveats.push(format!(
                "it is found in {} with {}, so its files are not told apart from theirs",
                shown(dir),
                others.join(", ")
            ));
            return Ok(caveats);
        }
        caveats.push(match w.declaration.local || w.lock.local.contains(name) {
            true => "its source is a path or git checkout trekr did not index as a gem, so \
                     its constants are not told apart from the app's own"
                .to_string(),
            false => "not installed here, so none of its files were read".to_string(),
        });
        return Ok(caveats);
    };
    if !w.declaration.platforms.is_empty() {
        caveats.push(format!(
            "declared for platforms {} only, where it may be used",
            w.declaration.platforms.join(", ")
        ));
    }
    if loads && let Some((fqn, path, line)) = w.owned.railties.get(name) {
        caveats.push(format!(
            "its Railtie {fqn} ({}:{line}) runs at boot through Bundler.require, \
             configuring the app with nothing naming it",
            shown(path)
        ));
    }
    if loads && let Some(patch) = patch(&w, dir)? {
        caveats.push(patch);
    }
    if !w.spec.executables.is_empty() && !w.convention {
        caveats.push(format!(
            "it installs {} (`{}`), which a person or a CI config trekr does not read may run",
            match w.spec.executables.len() {
                1 => "a command",
                _ => "commands",
            },
            w.spec.executables.join("`, `")
        ));
    }
    if w.spec.compiled {
        caveats.push(
            "it compiles an extension, whose constants and methods trekr does not read".to_string(),
        );
    }
    if let Some(found) = first_file(Path::new(dir), &|p: &Path| {
        p.extension().is_some_and(|e| e == "rake")
    }) {
        caveats.push(format!(
            "it defines rake tasks ({}), run by name",
            shown(&found)
        ));
    }
    if let Some(found) = first_file(&Path::new(dir).join("lib"), &|p: &Path| {
        p.components().any(|c| c.as_os_str() == "generators")
    }) {
        caveats.push(format!(
            "it provides generators ({}), run by hand",
            shown(&found)
        ));
    }
    if !w.convention
        && let Some(base) = plugin_of(name, w.lock, w.installed)
    {
        caveats.push(format!(
            "its name makes it a plugin of {base}, which may load it by name"
        ));
    }
    if w.declaration.groups.iter().any(|g| g == "development") {
        caveats.push(
            "it is in the development group, where a tool a person runs (a console, a \
             debugger, a generator) leaves no reference"
                .to_string(),
        );
    }
    if let Some((brought, holders)) = w.shared {
        caveats.push(format!(
            "it brings {brought}, which the app uses, as {} also bring{}",
            holders.join(", "),
            match holders.len() {
                1 => "s",
                _ => "",
            }
        ));
    }
    Ok(caveats)
}

/// Where a gem changes a class it does not own — a method defined on it, a
/// module mixed into it, an `on_load` hook — which acts once loaded.
fn patch(w: &Weigh<'_>, dir: &str) -> anyhow::Result<Option<String>> {
    let name = &w.declaration.name;
    let roots = crate::store::Roots::of(vec![w.layers.store_root(dir).0.to_string()]);
    // A path gem's root is the checkout's: only its own files are its.
    let its = |path: &str| w.layers.of(path) == Layer::Gem(name.clone());
    if let Some(hook) = w
        .store
        .load_hooks(&roots)?
        .into_iter()
        .find(|hook| its(&hook.path))
    {
        return Ok(Some(format!(
            "it hooks into {} as it loads ({}:{}), acting with nothing naming it",
            hook.target,
            shown(&hook.path),
            hook.line
        )));
    }
    let mixins = w
        .store
        .ancestry(&roots)?
        .into_iter()
        .filter(|edge| edge.relation != "superclass" && its(&edge.path));
    for edge in mixins {
        let Some((first, nesting)) = edge.owner.split_first() else {
            continue;
        };
        // `Base.include(self)` is sent to Base; a module's `included` hook
        // lands on whatever includes it, and an `on_load` hook is above.
        let owner = match crate::core::runtime::sent_to(first) {
            Some(receiver) => w.tree.resolve(receiver, nesting).fqn,
            None if first.starts_with('(') => None,
            None => w.tree.scope_fqn(&edge.owner),
        };
        let Some(owner) = owner else {
            continue;
        };
        if !w.owned.owns(name, &owner) && !w.owned.of_name(&owner).contains(name) {
            return Ok(Some(format!(
                "it mixes {} into {owner} ({}:{}), a class it does not own, which acts \
                 once loaded",
                edge.target,
                shown(&edge.path),
                edge.line
            )));
        }
    }
    let mut found = None;
    w.store.each_method(&roots, |row| {
        if found.is_some() || !its(&row.path) {
            return;
        }
        let owner = crate::tree::ruby_owner(&w.tree.owner(&row)).to_string();
        if !w.owned.owns(name, &owner)
            && !w.owned.of_name(&owner).contains(name)
            && !w.owned.owns(name, &placed(&row.nesting))
        {
            found = Some((owner, row.path.clone(), row.line, row.name.clone()));
        }
    })?;
    Ok(found.map(|(owner, path, line, method)| {
        format!(
            "it defines {owner}#{method} ({}:{line}) on a class it does not own, which \
             acts once loaded",
            shown(&path)
        )
    }))
}

/// The class a nesting's `class`/`module` keywords open, as written:
/// `class PReadFile` in `MiniMime::Db` opens `MiniMime::Db::PReadFile`, though
/// the lookup finds `PReadFile = ::File` there too.
fn placed(nesting: &[String]) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for name in nesting.iter().rev().filter(|n| !n.starts_with('(')) {
        match name.strip_prefix("::") {
            Some(rooted) => parts = vec![rooted],
            None => parts.push(name),
        }
    }
    parts.join("::")
}

/// The bundled gem a gem's name says it plugs into: `rubocop` for
/// `rubocop-rails`, `omniauth` for `omniauth-github`.
#[expect(clippy::string_slice, reason = "offsets come from char_indices")]
fn plugin_of(name: &str, lock: &Lock, installed: &HashMap<String, String>) -> Option<String> {
    let known = |n: &str| lock.needs.contains_key(n) || installed.contains_key(n);
    name.char_indices()
        .filter(|(_, c)| *c == '-' || *c == '_')
        .map(|(at, _)| &name[..at])
        .rfind(|base| known(base))
        .map(str::to_string)
}

/// What follows a plugin's base in its name, which its base may load it
/// by (`provider :saml` loads omniauth-saml) — unless another gem's name
/// holds it too (haml-rails's `rails`).
fn short_name(name: &str, lock: &Lock, installed: &HashMap<String, String>) -> Option<String> {
    let base = plugin_of(name, lock, installed)?;
    let short = name.get(base.len() + 1..)?;
    // One another gem's name holds too (`rails`, `rspec`) names no plugin.
    let generic = lock
        .needs
        .keys()
        .chain(installed.keys())
        .filter(|other| *other != name)
        .any(|other| other == short || other.split(['-', '_']).any(|part| part == short));
    (!generic && short.len() >= 3).then(|| short.to_string())
}

/// The first file under `dir` (depth-first, by name) that `keep` takes.
fn first_file(dir: &Path, keep: &dyn Fn(&Path) -> bool) -> Option<String> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            if path
                .file_name()
                .is_some_and(|n| n == "test" || n == "spec" || n == "ext")
            {
                continue;
            }
            if let Some(found) = first_file(&path, keep) {
                return Some(found);
            }
        } else if keep(&path) {
            return Some(path.to_string_lossy().into_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_word_is_whole_and_a_hyphen_joins_names() {
        assert!(has_word("web: bundle exec puma -c config/puma.rb", "puma"));
        assert!(has_word("require: rubocop-rails", "rubocop-rails"));
        assert!(!has_word("require: rubocop-rails", "rubocop"));
        assert!(!has_word("adapter: postgresql_x", "postgresql"));
        assert!(has_word("  adapter: postgresql", "postgresql"));
        assert!(!has_word("rm -rf .yardoc doc", "yardoc"));
        assert!(!has_word("cache: .yard", "yard"));
    }

    #[test]
    fn a_file_with_no_extension_is_ruby_when_it_holds_code() {
        for code in [
            "Vagrant.configure(\"2\") do |config|\nend\n",
            "lane :beta do\n  build_app\nend\n",
            "git_url \"https://example.com/certs\"\ntype \"development\"\n",
            "require \"widget\"\nWidget.run\n",
            "x = 1\n",
        ] {
            assert!(parses_as_code(code.as_bytes()), "{code}");
        }
        for prose in [
            "remove Tomlrb\nbump Rails\n",
            "Retriable\n",
            "  \n",
            "TODO: fix it.\n",
        ] {
            assert!(!parses_as_code(prose.as_bytes()), "{prose}");
        }
    }

    #[test]
    fn a_manifest_is_a_gemfile_or_a_gemspec() {
        assert!(is_manifest(Path::new("Gemfile")));
        assert!(is_manifest(Path::new("a/widget.gemspec")));
        assert!(is_manifest(Path::new("gems.rb")));
        assert!(!is_manifest(Path::new("app")));
        assert!(!is_manifest(Path::new("Gemfile.lock")));
    }
}
