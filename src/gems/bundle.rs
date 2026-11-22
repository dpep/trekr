//! The bundle one manifest declares, read from disk with no index: its
//! gems and the file declaring each, its path gems inside the checkout,
//! and what an installed gem's specification says. Facts `--dead Gemfile`
//! (DEC-670) grades against, with no grading in them.

use super::gemfile::{self, Declaration};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// A gemspec in the directory of a path gem of the root bundle (rails'
/// `activesupport/activesupport.gemspec`) — one its lockfile names, or with
/// none, its Gemfile: its gems are the root bundle's, which the index
/// located.
pub(crate) fn is_engine_gemspec(root: &Path, manifest: &Path) -> bool {
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

/// Each file a Gemfile evals (`eval_gemfile "Gemfile.tools"`), and each one
/// those eval, in the order Bundler reads them, with its source: they
/// declare the bundle's gems too (DEC-820). Which files a Gemfile evals is
/// `gemfile::declarations`' to say. Beside them, each eval not followed —
/// a computed path, or no such file — which is said, never skipped
/// silently (DEC-876); `root` makes their files relative.
pub(crate) fn evaled(
    root: &Path,
    gemfile: &Path,
    source: &[u8],
) -> (Vec<(PathBuf, Vec<u8>)>, Vec<Unfollowed>) {
    let mut found = Vec::new();
    let mut unfollowed = Vec::new();
    let mut read_once: HashSet<PathBuf> = HashSet::new();
    read_once.insert(std::fs::canonicalize(gemfile).unwrap_or_else(|_| gemfile.to_path_buf()));
    let relative = |path: &Path| {
        path.strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned()
    };
    let mut evals_of = |file: &Path, source: &[u8]| -> Vec<(PathBuf, String, String)> {
        let dir = file.parent().unwrap_or(Path::new("."));
        let declared = gemfile::declarations(source);
        unfollowed.extend(declared.unread_evals.into_iter().map(|eval| Unfollowed {
            file: relative(file),
            eval,
            why: "computed",
        }));
        declared
            .evals
            .into_iter()
            .rev()
            .map(|e| (dir.join(&e), relative(file), e))
            .collect()
    };
    let mut todo = evals_of(gemfile, source);
    let mut missing = Vec::new();
    while let Some((file, from, written)) = todo.pop() {
        if !read_once.insert(std::fs::canonicalize(&file).unwrap_or_else(|_| file.clone())) {
            continue;
        }
        let Ok(source) = crate::scan::read_source(&file) else {
            missing.push(Unfollowed {
                file: from,
                eval: written,
                why: "missing",
            });
            continue;
        };
        todo.extend(evals_of(&file, &source));
        found.push((file, source));
    }
    unfollowed.extend(missing);
    (found, unfollowed)
}

/// An `eval_gemfile` not followed: the file that writes it, what it evals
/// as written, and why — `computed`, a path trekr cannot read without
/// running Ruby, or `missing`, no such file.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct Unfollowed {
    pub(crate) file: String,
    pub(crate) eval: String,
    pub(crate) why: &'static str,
}

impl Unfollowed {
    /// In words: `` `eval_gemfile nope.rb` in Gemfile: no such file ``.
    pub(crate) fn said(&self) -> String {
        said(&self.file, &self.eval, self.why)
    }
}

/// [`Unfollowed::said`], from its fields as `--json` wrote them.
pub(crate) fn said(file: &str, eval: &str, why: &str) -> String {
    let why = match why {
        "missing" => "no such file",
        _ => "a computed path trekr does not run",
    };
    format!("`eval_gemfile {eval}` in {file}: {why}")
}

/// The evals of `manifest`'s bundle that are not followed.
pub(crate) fn unfollowed_evals(root: &Path, manifest: &Path) -> Vec<Unfollowed> {
    let source = crate::scan::read_source(manifest).unwrap_or_default();
    evaled(root, manifest, &source).1
}

/// The path gems inside the checkout (not the checkout itself), each with
/// its directory under `root`: the lockfile's `PATH` specs, and a `path:`
/// the lockfile does not name. Their files are indexed as the checkout's,
/// and are theirs — unless two gems are found in one directory, whose files
/// are told apart from neither's: those are in `shared`, each with the
/// directory and the others there.
pub(crate) struct PathGems {
    pub(crate) found: Vec<(String, String)>,
    pub(crate) shared: HashMap<String, (String, Vec<String>)>,
}

pub(crate) fn path_gems(
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

pub(crate) fn path_gem_dirs(
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
pub(crate) fn declared_in(root: &Path, manifest: &Path) -> Vec<(Declaration, String)> {
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
    let source = crate::scan::read_source(manifest).unwrap_or_default();
    let (evals, _) = evaled(root, manifest, &source);
    for (file, source) in std::iter::once((manifest.to_path_buf(), source)).chain(evals) {
        let found = gemfile::declarations(&source);
        let dir = file.parent().unwrap_or(root).to_path_buf();
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

/// What an installed gem's specification says: its commands and whether it
/// compiles. Read from rubygems' `specifications/` beside the gem, or the
/// gem's own gemspec (a git or path gem), picked as Bundler picks it; a list
/// it computes is looked for on disk.
pub(crate) fn installed_spec(dir: &str, gem: &str) -> InstalledSpec {
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
pub(crate) fn own_gemspec(dir: &Path, gem: &str) -> Option<Vec<u8>> {
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
pub(crate) struct InstalledSpec {
    pub(crate) executables: Vec<String>,
    pub(crate) compiled: bool,
}

/// The version a gem's directory carries (`kaminari-1.2.2`), or none.
pub(crate) fn version_of(dir: &str, name: &str) -> Option<String> {
    let base = Path::new(dir).file_name()?.to_string_lossy().into_owned();
    base.strip_prefix(&format!("{name}-")).map(str::to_string)
}
