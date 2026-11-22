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
use crate::gems::bundle::{
    InstalledSpec, declared_in, installed_spec, is_engine_gemspec, path_gems, version_of,
};
use crate::gems::gemfile::{self, Declaration, Lock, Require};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

mod owners;
mod scripts;
mod texts;
use owners::*;
use scripts::*;
use texts::*;

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
    /// The bundle's `eval_gemfile`s not followed, whose gems are not graded
    /// (DEC-876).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unread_evals: Vec<crate::gems::bundle::Unfollowed>,
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
    let mut unread_evals: Vec<crate::gems::bundle::Unfollowed> = Vec::new();
    for one in graded {
        for eval in one.unread_evals {
            if !unread_evals.contains(&eval) {
                unread_evals.push(eval);
            }
        }
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
        unread_evals,
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
    for eval in &summary.unread_evals {
        println!(
            "not followed, so no gem it declares is graded: {}",
            eval.said()
        );
    }
    Ok(exit_on(found))
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
    unread_evals: Vec<crate::gems::bundle::Unfollowed>,
}

/// What each evidence channel reads: the checkout, its index, and which gem
/// owns what.
struct Ctx<'a> {
    store: &'a Store,
    root: &'a Path,
    root_str: &'a str,
    tree: &'a Tree,
    layers: &'a Layers,
    owned: &'a Owned,
    lock: &'a Lock,
    installed: &'a HashMap<String, String>,
    declared: &'a [(Declaration, String)],
    direct: &'a HashSet<&'a str>,
    /// The bundle's own files, whose names are no use of a gem.
    manifests: &'a HashSet<String>,
    scripts: &'a Scripts,
}

impl Ctx<'_> {
    /// A path relative to the checkout, as a row cites it.
    fn at(&self, path: &str) -> String {
        format!("{}/{path}", self.root_str)
    }
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
    let manifests: HashSet<String> = declared
        .iter()
        .map(|(_, file)| file.clone())
        .chain(["Gemfile", "gems.rb", "Gemfile.lock", "gems.locked"].map(String::from))
        .collect();
    let scripts = Scripts::read(root);
    let cx = Ctx {
        store,
        root,
        root_str: &root_str,
        tree: &tree,
        layers: &layers,
        owned: &owned,
        lock: &lock,
        installed: &installed,
        declared: &declared,
        direct: &direct,
        manifests: &manifests,
        scripts: &scripts,
    };
    let at = |path: &str| cx.at(path);

    let mut uses: HashMap<String, Uses> = HashMap::new();
    credit_constants(&cx, &mut uses)?;
    credit_reopened(&cx, &mut uses);
    let pending = credit_requires(&cx, &mut uses)?;
    let symbols = credit_methods(&cx, &mut uses)?;
    let shared = credit_brought(&cx, pending, &mut uses);

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
        unread_evals: crate::gems::bundle::unfollowed_evals(root, manifest),
        root: root_str,
    })
}

/// Constants: each reference resolved as Ruby looks it up, credited to the
/// gem that owns what it names or a namespace holding it.
fn credit_constants(cx: &Ctx, uses: &mut HashMap<String, Uses>) -> anyhow::Result<()> {
    let mut resolved: HashMap<(String, Vec<String>), Option<String>> = HashMap::new();
    let refs = cx
        .store
        .all_const_refs(cx.root_str)?
        .into_iter()
        .filter(|r| !cx.manifests.contains(&r.path))
        .chain(cx.scripts.const_refs());
    for row in refs {
        let written = row.name.trim_start_matches("::");
        if !written
            .split("::")
            .any(|segment| cx.owned.leaves.contains(segment))
        {
            continue;
        }
        let fqn = resolved
            .entry((row.name.clone(), row.nesting.clone()))
            .or_insert_with(|| cx.tree.resolve(&row.name, &row.nesting).fqn)
            .clone()
            .unwrap_or_else(|| written.to_string());
        for gem in cx.owned.of_name(&fqn) {
            credit(
                uses,
                cx.layers,
                Evidence {
                    name: gem.clone(),
                    by: "constant",
                    what: fqn.clone(),
                    path: cx.at(&row.path),
                    line: row.line,
                },
            );
        }
    }
    Ok(())
}

/// A class the app reopens is used: `module Devise` in an initializer. A
/// path gem inside the checkout reopening another gem's class counts.
fn credit_reopened(cx: &Ctx, uses: &mut HashMap<String, Uses>) {
    for (fqn, gems) in &cx.owned.by_fqn {
        let sites: Vec<_> = cx
            .tree
            .sites(fqn)
            .into_iter()
            .filter(|site| {
                cx.layers.is_checkouts(&site.path)
                    && site
                        .path
                        .strip_prefix(cx.layers.app.as_str())
                        .is_some_and(|relative| !cx.manifests.contains(relative))
            })
            .collect();
        for gem in gems {
            let own = Layer::Gem(gem.clone());
            let Some(site) = sites.iter().find(|site| cx.layers.of(&site.path) != own) else {
                continue;
            };
            credit(
                uses,
                cx.layers,
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
}

/// A require a path gem writes, held until whether that gem is loaded is
/// known: the requiring gem, the evidence, and whether it is credited
/// already as the tests' use.
type Pending = Vec<(String, Evidence, bool)>;

/// Requires: a literal path one of the gems' files answers to. A path gem's
/// require loads what it names only once that gem is loaded, so those come
/// back pending, weighed when the rows are known (DEC-770).
fn credit_requires(cx: &Ctx, uses: &mut HashMap<String, Uses>) -> anyhow::Result<Pending> {
    let mut features = Features::new(cx.store, cx.layers, cx.installed)?;
    features.names.extend(
        cx.direct
            .iter()
            .map(|n| n.to_string())
            .chain(cx.lock.needs.keys().cloned()),
    );
    let requiring = cx.store.files_calling_any(
        cx.root_str,
        &["require", "load", "autoload"].map(String::from),
    )?;
    let mut sources: Vec<(String, Vec<u8>)> = requiring
        .into_iter()
        .filter(|path| !cx.manifests.contains(path))
        .filter_map(|path| {
            let raw = crate::scan::read_source(cx.root.join(&path)).ok()?;
            Some((path, raw))
        })
        .collect();
    sources.extend(cx.scripts.sources());
    let mut pending: Pending = Vec::new();
    for (path, raw) in &sources {
        let requirer = match cx.layers.of(&cx.at(path)) {
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
                    path: cx.at(path),
                    line,
                };
                match &requirer {
                    Some(by) => pending.push((by.clone(), evidence, false)),
                    None => credit(uses, cx.layers, evidence),
                }
            }
        }
    }
    for (path, required, line) in cx.scripts.word_requires() {
        for gem in features.of(&required) {
            credit(
                uses,
                cx.layers,
                Evidence {
                    name: gem,
                    by: "require",
                    what: required.clone(),
                    path: cx.at(&path),
                    line,
                },
            );
        }
    }
    Ok(pending)
}

/// Methods: a name few gems define, in a module of their own or on a class
/// they reopen, which the checkout calls. A gem's own class's methods are
/// reached through its constant, which is counted above. Returns each
/// symbol the checkout writes outside its tests, with the first file: a
/// plugin's base may load it by one (`provider :saml`).
fn credit_methods(
    cx: &Ctx,
    uses: &mut HashMap<String, Uses>,
) -> anyhow::Result<HashMap<String, String>> {
    let defines = method_names(cx.store, cx.root_str, cx.layers)?;
    let never_loaded: HashSet<&str> = cx
        .declared
        .iter()
        .filter(|(d, _)| d.require == Require::Off)
        .map(|(d, _)| d.name.as_str())
        .collect();
    let names_called: Vec<(String, String, bool)> = cx
        .store
        .called_names(cx.root_str)?
        .into_iter()
        .filter(|(_, path, _)| !cx.manifests.contains(path))
        .collect();
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
        cx.scripts
            .calls()
            .filter(|(name, _)| defines.contains_key(name)),
    );
    let mut asked: HashSet<(String, bool)> = HashSet::new();
    let mut in_core: HashMap<String, bool> = HashMap::new();
    let rbs_core = cx
        .installed
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
                || cx
                    .tree
                    .named(&name)
                    .iter()
                    .any(|def| cx.layers.of(&def.site.path) == Layer::Other)
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
            let owner = crate::tree::ruby_owner(&cx.tree.owner(row)).to_string();
            let mixin = cx.tree.kind_of(&owner) == Some("module");
            if !mixin && cx.owned.owns(gem, &owner) {
                continue;
            }
            credit(
                uses,
                cx.layers,
                Evidence {
                    name: gem.clone(),
                    by: "method",
                    what: name.clone(),
                    path: cx.at(&path),
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
    Ok(symbols)
}

/// A gem the app uses through another: credited to the one declared gem
/// that brings it, when no gem already credited brings it too; and each
/// pending require, once whether its gem is loaded is known. Returns, for a
/// declared gem whose used dependency others bring too, that dependency and
/// those others.
fn credit_brought(
    cx: &Ctx,
    mut pending: Pending,
    uses: &mut HashMap<String, Uses>,
) -> HashMap<String, (String, Vec<String>)> {
    let direct_names: Vec<String> = cx.declared.iter().map(|(d, _)| d.name.clone()).collect();
    let closures: HashMap<&str, BTreeSet<String>> = direct_names
        .iter()
        .map(|name| (name.as_str(), cx.lock.closure(name)))
        .collect();
    let by_name: HashMap<&str, &Declaration> = cx
        .declared
        .iter()
        .map(|(d, _)| (d.name.as_str(), d))
        .collect();
    let mut shared: HashMap<String, (String, Vec<String>)> = HashMap::new();
    loop {
        let used_through: Vec<String> = uses
            .keys()
            .filter(|gem| !cx.direct.contains(gem.as_str()))
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
                if credited(name, uses) {
                    continue;
                }
                for brought in closures[name.as_str()]
                    .iter()
                    .filter(|b| used_through.contains(b))
                {
                    let kept = direct_names.iter().any(|other| {
                        other != name
                            && credited(other, uses)
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
                Some(d) if is_row(uses, d) => {
                    waiting.push((by, evidence, by_tests));
                    continue;
                }
                Some(_) => uses.get(&by).is_some_and(|u| u.live.is_some()),
            };
            if live {
                credit(uses, cx.layers, evidence);
                moved = true;
                continue;
            }
            if !by_tests {
                credit_as(uses, cx.layers, evidence.clone(), true);
                moved = true;
            }
            waiting.push((by, evidence, true));
        }
        pending = waiting;
        if !moved {
            break;
        }
    }
    shared
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
    // Every candidate's paths in one automaton: a file worth parsing
    // mentions one of another gem's.
    let (needles, of_needle): (Vec<&String>, Vec<&String>) = wanted.iter().unzip();
    let mentioned = aho_corasick::AhoCorasick::new(&needles)?;
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
            let mentions = mentioned
                .find_overlapping_iter(&raw)
                .any(|found| of_needle[found.pattern().as_usize()] != gem);
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
    fn a_manifest_is_a_gemfile_or_a_gemspec() {
        assert!(is_manifest(Path::new("Gemfile")));
        assert!(is_manifest(Path::new("a/widget.gemspec")));
        assert!(is_manifest(Path::new("gems.rb")));
        assert!(!is_manifest(Path::new("app")));
        assert!(!is_manifest(Path::new("Gemfile.lock")));
    }
}
