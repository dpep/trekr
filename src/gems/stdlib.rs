//! A Ruby's standard library, indexed as a checkout of its own (DEC-180).
//!
//! `Set`, `Pathname`, `URI`, `Logger` and the rest are Ruby files in
//! `<prefix>/lib/ruby/<abi>/`, the same bytes for every app on that Ruby, so
//! the directory is one checkout per Ruby, shared as a gem is. Part of it
//! belongs to default gems — `json.rb` and `json/` are json 2.9.1's — and an
//! app that bundles its own json must not see that copy too: which files a
//! default gem owns is read from its gemspec here, and hidden per app when the
//! tree is built.

use super::declared::{Version, split_dir, string};
use crate::scan;
use ruby_prism::Visit;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// The stdlib an app runs on.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Stdlib {
    /// `<prefix>/lib/ruby/<abi>`, canonical.
    pub(crate) root: PathBuf,
    /// Which Ruby, and how it was chosen, in words.
    pub(crate) ruby: String,
    /// Each default gem it ships, `(name, version)`, from its specs' names.
    ships: HashSet<(String, String)>,
}

impl Stdlib {
    fn at(root: PathBuf, ruby: String) -> Stdlib {
        let ships = specifications(&root)
            .and_then(|dir| std::fs::read_dir(dir).ok())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let file = entry.file_name().to_string_lossy().into_owned();
                let (name, version) = split_dir(file.strip_suffix(".gemspec")?)?;
                Some((name.to_string(), version.to_string()))
            })
            .collect();
        Stdlib { root, ruby, ships }
    }

    /// Is this gem, at this version, one this stdlib ships? Then its code is
    /// here, whether or not rubygems left an empty directory to find it by.
    pub(crate) fn ships(&self, name: &str, version: &str) -> bool {
        self.ships
            .contains(&(name.to_string(), version.to_string()))
    }

    /// The rbs gem whose signatures describe this Ruby's core and stdlib
    /// (DEC-240, DEC-242): the one bundled with it, whose version is that
    /// Ruby's; else the highest installed for it; else the highest any
    /// installed Ruby has. Each must have a `core/` to read.
    pub(crate) fn rbs(&self) -> Option<RbsGem> {
        if let Some(gem) = bundled_rbs(&self.root) {
            return Some(gem);
        }
        if let Some(gem) = rbs_in(&rbs_dirs(&self.root)) {
            return Some(RbsGem {
                chosen: Chosen::Installed,
                ..gem
            });
        }
        let others: Vec<PathBuf> = installs()
            .iter()
            .filter_map(|prefix| stdlib_in(prefix))
            .filter(|root| *root != self.root)
            .filter_map(|root| bundled_dir(&root))
            .collect();
        rbs_in(&others).map(|gem| RbsGem {
            chosen: Chosen::Other,
            ..gem
        })
    }
}

/// An rbs gem on disk.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RbsGem {
    pub(crate) version: String,
    /// The gem's own directory, holding `core/` and `stdlib/`.
    pub(crate) dir: PathBuf,
    pub(crate) chosen: Chosen,
}

/// Why an rbs gem was the one read, worst first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Chosen {
    /// Another Ruby's, since this one has none: it may describe that Ruby.
    Other,
    /// The highest installed for this Ruby since.
    Installed,
    /// Installed with the Ruby: its version is that Ruby's.
    Bundled,
}

/// Two gem versions in rubygems' order; one that does not parse is lowest.
pub(crate) fn version_order(a: &str, b: &str) -> std::cmp::Ordering {
    Version::parse(a).cmp(&Version::parse(b))
}

impl Chosen {
    /// As the store writes it: `bundled`, `installed`, `other`.
    pub(crate) fn named(name: &str) -> Option<Chosen> {
        [Chosen::Bundled, Chosen::Installed, Chosen::Other]
            .into_iter()
            .find(|chosen| chosen.name() == name)
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Chosen::Bundled => "bundled",
            Chosen::Installed => "installed",
            Chosen::Other => "other",
        }
    }

    /// In words, for `--index` and `--status`.
    pub(crate) fn why(self) -> &'static str {
        match self {
            Chosen::Bundled => "bundled with this Ruby",
            Chosen::Installed => "the highest installed for this Ruby; none was bundled with it",
            Chosen::Other => "another Ruby's, since this Ruby has none",
        }
    }
}

/// `<prefix>/lib/ruby/gems/<abi>/gems`: where a Ruby installs its bundled
/// gems, beside `<prefix>/lib/ruby/<abi>`.
fn bundled_dir(root: &Path) -> Option<PathBuf> {
    Some(
        root.parent()?
            .join("gems")
            .join(root.file_name()?)
            .join("gems"),
    )
}

/// The rbs bundled with the Ruby whose stdlib is at `root`
/// (`<prefix>/lib/ruby/<abi>`), with a `core/` to read.
///
/// Ruby records the version in `gems/bundled_gems`, which some installs
/// keep; that is taken when there is one. Otherwise rubygems keeps no mark
/// of it, and a later `gem install rbs` lands in the same directory, so the
/// bundled one is told by when it was written: within an hour of the Ruby's
/// install — its gemspec, or its cached `.gem`, which `gem pristine` reads
/// and never rewrites and which a Ruby's install may date from its tarball.
/// The install is dated by the median of its default gems' specs, which
/// `gem update --system` rewrites a few of, else by `bin/ruby` (DEC-272).
fn bundled_rbs(root: &Path) -> Option<RbsGem> {
    let gems = bundled_dir(root)?;
    let at = |version: &str| {
        let dir = gems.join(format!("rbs-{version}"));
        dir.join("core").is_dir().then(|| RbsGem {
            version: version.to_string(),
            dir,
            chosen: Chosen::Bundled,
        })
    };
    if let Some(version) = recorded_rbs(root) {
        return at(&version);
    }
    let installed = install_time(root)?;
    let base = gems.parent()?;
    let hour = std::time::Duration::from_secs(3600);
    std::fs::read_dir(&gems)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let (gem, version) = split_dir(&name)?;
            if gem != "rbs" {
                return None;
            }
            // How far from the install each was written; a cached `.gem`
            // older than the install came with it.
            let spec = written(&base.join(format!("specifications/{name}.gemspec")))
                .map(|at| distance(at, installed));
            let cached = written(&base.join(format!("cache/{name}.gem")))
                .map(|at| at.duration_since(installed).unwrap_or_default());
            let apart = spec.into_iter().chain(cached).min()?;
            (apart <= hour).then(|| (apart, version.to_string()))
        })
        .min()
        .and_then(|(_, version)| at(&version))
}

fn written(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn distance(a: std::time::SystemTime, b: std::time::SystemTime) -> std::time::Duration {
    a.duration_since(b)
        .or_else(|_| b.duration_since(a))
        .unwrap_or_default()
}

/// The rbs version a Ruby's own list of its bundled gems names:
/// `gems/bundled_gems`, lines of `name version [repository [revision]]`.
fn recorded_rbs(root: &Path) -> Option<String> {
    let lib = root.parent()?;
    let prefix = lib.parent()?.parent()?;
    let abi = root.file_name()?;
    let places = [
        prefix.join("gems/bundled_gems"),
        root.join("bundled_gems"),
        lib.join("gems").join(abi).join("bundled_gems"),
    ];
    places.iter().find_map(|path| {
        let text = std::fs::read_to_string(path).ok()?;
        text.lines().find_map(|line| {
            let mut fields = line.split_whitespace();
            (fields.next()? == "rbs").then(|| fields.next().map(str::to_string))?
        })
    })
}

/// When the Ruby at `root` was installed: the median of its default gems'
/// spec times, else its `bin/ruby`'s.
fn install_time(root: &Path) -> Option<std::time::SystemTime> {
    let mut times: Vec<std::time::SystemTime> = specifications(root)
        .and_then(|dir| std::fs::read_dir(dir).ok())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| written(&entry.path()))
        .collect();
    times.sort();
    if let Some(median) = times.get(times.len() / 2) {
        return Some(*median);
    }
    written(&root.ancestors().nth(3)?.join("bin/ruby"))
}

/// Where a Ruby's gems are installed, for the stdlib at `root`
/// (`<prefix>/lib/ruby/<abi>`): beside it, where Ruby bundles its own;
/// `~/.gem/ruby/<abi>`; rvm's gem directories for that install; Homebrew's
/// shared one for a Homebrew Ruby; and `$GEM_HOME` when it is this Ruby's.
fn rbs_dirs(root: &Path) -> Vec<PathBuf> {
    let Some(abi) = root.file_name() else {
        return Vec::new();
    };
    let Some(lib) = root.parent() else {
        return Vec::new();
    };
    let mut dirs = vec![lib.join("gems").join(abi).join("gems")];
    let prefix = lib.parent().and_then(Path::parent);
    if let Ok(home) = std::env::var("HOME") {
        let home = PathBuf::from(home);
        dirs.push(home.join(".gem/ruby").join(abi).join("gems"));
        if let Some(install) = prefix.and_then(Path::file_name) {
            let install = install.to_string_lossy();
            dirs.push(home.join(".rvm/gems").join(install.as_ref()).join("gems"));
            dirs.push(
                home.join(".rvm/gems")
                    .join(format!("{install}@global"))
                    .join("gems"),
            );
        }
    }
    for brew in ["/opt/homebrew", "/usr/local"] {
        if prefix.is_some_and(|p| p.starts_with(brew)) {
            dirs.push(Path::new(brew).join("lib/ruby/gems").join(abi).join("gems"));
        }
    }
    if let Ok(home) = std::env::var("GEM_HOME")
        && !home.is_empty()
        && of_gem_home(Path::new(&home)).as_deref() == Some(root)
    {
        dirs.push(Path::new(&home).join("gems"));
    }
    dirs
}

/// The highest `rbs-<version>` among these gem directories that has a
/// `core/` to read.
fn rbs_in(dirs: &[PathBuf]) -> Option<RbsGem> {
    dirs.iter()
        .flat_map(|dir| std::fs::read_dir(dir).into_iter().flatten().flatten())
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let (gem, version) = split_dir(&name)?;
            let dir = entry.path();
            (gem == "rbs" && dir.join("core").is_dir())
                .then(|| Some((Version::parse(version)?, version.to_string(), dir)))?
        })
        .max_by(|(a, _, _), (b, _, _)| a.cmp(b))
        .map(|(_, version, dir)| RbsGem {
            version,
            dir,
            chosen: Chosen::Installed,
        })
}

/// Dev tooling nobody navigates into from an app, internals an app reaches
/// only through a handful of names, opt-in extensions that reopen core for
/// every object once required, and files that are data rather than code.
/// A path is skipped when it is one of these or under one ending in `/`.
const SKIPPED: &[&str] = &[
    // Bundler and rubygems run the app rather than being called by it;
    // `bundler.rb` and the few rubygems files below stay, for
    // `Bundler.require` and `Gem::Version`.
    "bundler/",
    "rubygems/",
    // Consoles, documentation, and error-message helpers.
    "irb.rb",
    "irb/",
    "rdoc.rb",
    "rdoc/",
    "reline.rb",
    "reline/",
    "readline.rb",
    "syntax_suggest.rb",
    "syntax_suggest/",
    "error_highlight.rb",
    "error_highlight/",
    "did_you_mean.rb",
    "did_you_mean/",
    "psych/y.rb",
    "objspace/trace.rb",
    // Top-level `def`s — `cp`, `rm`, `have_header` — that would read as
    // private methods of every object.
    "un.rb",
    "mkmf.rb",
    // The VM's own compiler, the parser Ruby parses itself with, and the
    // loader's warnings: nothing an app calls.
    "ruby_vm/",
    "prism.rb",
    "prism/",
    "bundled_gems.rb",
    // Tables, not code.
    "unicode_normalize/tables.rb",
    // `require "json/add/core"` gives Time, Range, Struct … `to_json` and
    // `json_create`; an app that has not required it would be told they exist.
    "json/add/",
];

/// Under a skipped directory, and kept: what apps call.
const KEPT: &[&str] = &[
    "rubygems/version.rb",
    "rubygems/requirement.rb",
    "rubygems/specification.rb",
    "rubygems/basic_specification.rb",
];

/// Is this stdlib path, relative to its root, left out?
pub(crate) fn skipped(path: &str) -> bool {
    if KEPT.contains(&path) {
        return false;
    }
    SKIPPED.iter().any(|skip| match skip.strip_suffix('/') {
        Some(dir) => path.starts_with(skip) || path == dir,
        None => path == *skip,
    })
}

/// The stdlib's Ruby files that are indexed.
pub(crate) fn files(root: &Path) -> scan::Files {
    let mut files = scan::walk(root, "");
    files.retain(|path, _| !skipped(path));
    files
}

/// The stdlib of the Ruby this checkout runs on, chosen as its gems are
/// (DEC-152): the version `.ruby-version` or the Gemfile names, the Ruby
/// `$GEM_HOME` belongs to, the `ruby` on `$PATH`; failing those, the one
/// Ruby installed, when there is only one.
///
/// A directory of scripts that names no Ruby still runs on one, and its core
/// is that Ruby's (DEC-242). A test stays hermetic by what it puts on `PATH`
/// and in `HOME`.
///
/// `last` is the stdlib the checkout's last index chose. Only the checkout
/// naming an installed Ruby moves it: an editor launched from the Dock, or
/// the language server's background reindex, sees a poorer environment
/// than the shell that indexed, and must not take core away (DEC-271).
pub(crate) fn for_checkout(repo: &Path, last: Option<&Path>) -> Option<Stdlib> {
    if let Some(version) = super::project_ruby(repo)
        && let Some(root) = named(&version)
    {
        let mut ruby = format!("Ruby {version}, which the checkout names");
        if let Some(last) = last.filter(|last| *last != root) {
            ruby.push_str(&format!(
                ", in place of the {} the last index ran on",
                install_name(last)
            ));
        }
        return Some(Stdlib::at(root, ruby));
    }
    let found = from_environment();
    let Some(kept) = last.filter(|root| has_default_gems(root)) else {
        return found;
    };
    let why = match &found {
        Some(found) if found.root == kept => return Some(found.clone()),
        Some(found) => format!("this environment would pick {}", found.ruby),
        None => "this environment finds no Ruby".to_string(),
    };
    Some(Stdlib::at(
        kept.to_path_buf(),
        format!(
            "the {}, kept from the last index ({why})",
            install_name(kept)
        ),
    ))
}

/// The Ruby the environment names, when the checkout names none installed.
fn from_environment() -> Option<Stdlib> {
    if let Ok(home) = std::env::var("GEM_HOME")
        && !home.is_empty()
        && let Some(root) = of_gem_home(Path::new(&home))
    {
        return Some(Stdlib::at(
            root,
            format!(
                "the Ruby $GEM_HOME names ({})",
                crate::core::paths::pretty(&home)
            ),
        ));
    }
    if let Some(ruby) = super::path_ruby()
        && let Some(root) = ruby.parent().and_then(Path::parent).and_then(stdlib_in)
    {
        return Some(Stdlib::at(
            root,
            format!(
                "the ruby on $PATH ({})",
                crate::core::paths::pretty(&ruby.to_string_lossy())
            ),
        ));
    }
    let mut roots: Vec<PathBuf> = installs().iter().filter_map(|p| stdlib_in(p)).collect();
    roots.sort();
    roots.dedup();
    match roots.as_slice() {
        [root] => Some(Stdlib::at(
            root.clone(),
            format!(
                "the only Ruby installed ({})",
                crate::core::paths::pretty(&root.to_string_lossy())
            ),
        )),
        _ => None,
    }
}

/// `Ruby at ~/.rvm/rubies/ruby-3.4.9`, for the stdlib at `root`
/// (`<prefix>/lib/ruby/<abi>`).
fn install_name(root: &Path) -> String {
    let prefix = root.ancestors().nth(3).unwrap_or(root);
    format!(
        "Ruby at {}",
        crate::core::paths::pretty(&prefix.to_string_lossy())
    )
}

/// The Ruby version the checkout names when no install of it is found: the
/// checkout then runs on another Ruby, and `--index` and `--status` say so.
pub(crate) fn named_missing(repo: &Path) -> Option<String> {
    let version = super::project_ruby(repo)?;
    named(&version).is_none().then_some(version)
}

/// Every Ruby installed by a version manager or Homebrew, by its prefix.
fn installs() -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = super::version_managers()
        .iter()
        .flat_map(|p| super::expand(p))
        .collect();
    found.extend(super::homebrew_kegs());
    found
}

/// `ruby-3.4.9` → `3.4.9`; Homebrew's `3.4.9_1` keeps its revision out.
fn install_version(prefix: &Path) -> Option<Version> {
    let name = prefix.file_name()?.to_string_lossy();
    let name = name.strip_prefix("ruby-").unwrap_or(&name);
    Version::parse(name.split(['_', '@']).next()?)
}

/// The stdlib of the highest installed Ruby `version` names: `3.4.9`
/// exactly, `3.4` as any 3.4.
fn named(version: &str) -> Option<PathBuf> {
    let wanted = Version::parse(version)?;
    let depth = version.split('.').count();
    // Reversed, so of two installs of one version the version manager's,
    // listed before Homebrew's, wins.
    installs()
        .into_iter()
        .rev()
        .filter_map(|prefix| Some((install_version(&prefix)?, prefix)))
        .filter(|(have, _)| have.truncated(depth) == wanted)
        .max_by(|(a, _), (b, _)| a.cmp(b))
        .and_then(|(_, prefix)| stdlib_in(&prefix))
}

/// `$GEM_HOME` is `<prefix>/lib/ruby/gems/<abi>` for most installs, and
/// `~/.rvm/gems/ruby-3.4.9[@gemset]` for rvm, named for the Ruby.
fn of_gem_home(home: &Path) -> Option<PathBuf> {
    let abi = home.file_name()?.to_string_lossy();
    if let Some(ruby) = home.parent().filter(|gems| gems.ends_with("lib/ruby/gems")) {
        let root = ruby.parent()?.join(abi.as_ref());
        return has_default_gems(&root).then(|| std::fs::canonicalize(&root).ok())?;
    }
    named(abi.strip_prefix("ruby-")?.split('@').next()?)
}

/// `<prefix>/lib/ruby/<abi>`, when rubygems recorded that Ruby's default gems.
fn stdlib_in(prefix: &Path) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = super::expand(&prefix.join("lib/ruby/*"))
        .into_iter()
        .filter(|dir| has_default_gems(dir))
        .collect();
    found.sort();
    std::fs::canonicalize(found.pop()?).ok()
}

fn has_default_gems(root: &Path) -> bool {
    specifications(root).is_some_and(|dir| dir.is_dir())
}

/// Where rubygems keeps this stdlib's default gems' specs:
/// `lib/ruby/gems/<abi>/specifications/default`, beside `lib/ruby/<abi>`.
fn specifications(root: &Path) -> Option<PathBuf> {
    let abi = root.file_name()?;
    Some(
        root.parent()?
            .join("gems")
            .join(abi)
            .join("specifications/default"),
    )
}

/// The stdlib files whose classes are partly compiled, each with the
/// compiled extension it answers to (DEC-181): `monitor.rb` loads
/// `monitor.so`, so `Monitor#synchronize` is real though no Ruby defines it.
///
/// A file is when it requires a compiled feature of its own family — the
/// feature's first part begins with the file's (`date.rb` loads `date_core`,
/// `erb/util.rb` loads `erb/escape`) — when it is under a feature's
/// directory (`openssl/`, `json/ext/generator/`), or when it requires a
/// loader that only does the first (`digest.rb` through `digest/loader`).
/// Loading another family's extension is using it: `pp.rb` requires
/// `io/console` for the terminal's width, and `PP` is all Ruby.
pub(crate) fn compiled(root: &Path, files: &scan::Files) -> Vec<(String, String)> {
    let features = compiled_features(root);
    if features.is_empty() {
        return Vec::new();
    }
    let read: BTreeMap<&String, Loads> = files
        .keys()
        .filter_map(|path| Some((path, loads(&std::fs::read(root.join(path)).ok()?))))
        .collect();
    let mut backed: BTreeMap<String, String> = BTreeMap::new();
    for (path, loads) in &read {
        let direct = loads.requires.iter().find_map(|feature| {
            let (bare, explicit) = match feature
                .strip_suffix(".so")
                .or_else(|| feature.strip_suffix(".bundle"))
            {
                Some(bare) => (bare, true),
                None => (feature.as_str(), false),
            };
            let compiled = explicit || !files.contains_key(&format!("{bare}.rb"));
            let family = |feature: &str| feature.split('/').next().unwrap_or_default().to_string();
            let own = family(bare).starts_with(&family(path.trim_end_matches(".rb")));
            (compiled && own && features.contains(bare)).then(|| bare.to_string())
        });
        let under = features
            .iter()
            .filter(|feature| path.starts_with(&format!("{feature}/")))
            .max_by_key(|feature| feature.len())
            .cloned();
        if let Some(feature) = direct.or(under) {
            backed.insert(path.to_string(), feature);
        }
    }
    let through_loaders: Vec<(String, String)> = read
        .iter()
        .filter(|(path, _)| !backed.contains_key(path.as_str()))
        .filter_map(|(path, loads)| {
            let feature = loads.requires.iter().find_map(|required| {
                let file = format!("{required}.rb");
                let loader = read.get(&file)?;
                (!loader.declares).then(|| backed.get(&file).cloned())?
            })?;
            Some((path.to_string(), feature))
        })
        .collect();
    backed.extend(through_loaders);
    backed.into_iter().collect()
}

/// Every compiled extension in the stdlib's architecture directory, the one
/// holding `rbconfig.rb`, by the feature `require` names it with.
fn compiled_features(root: &Path) -> HashSet<String> {
    let Some(arch) = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .find(|dir| dir.join("rbconfig.rb").is_file())
    else {
        return HashSet::new();
    };
    let mut features = HashSet::new();
    let mut stack = vec![arch.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                // A `.dSYM` is debug symbols, named like the extension.
                if !name.ends_with(".dSYM") {
                    stack.push(path);
                }
                continue;
            }
            let Some(stem) = [".so", ".bundle", ".dll"]
                .iter()
                .find_map(|ext| name.strip_suffix(ext))
            else {
                continue;
            };
            if let Ok(dir) = dir.strip_prefix(&arch) {
                features.insert(dir.join(stem).to_string_lossy().into_owned());
            }
        }
    }
    features
}

/// What a file requires by a literal name, and whether it opens a class or
/// module at all — a loader that only requires opens none.
#[derive(Default)]
struct Loads {
    requires: Vec<String>,
    declares: bool,
}

impl<'pr> Visit<'pr> for Loads {
    fn visit_call_node(&mut self, call: &ruby_prism::CallNode<'pr>) {
        if call.name().as_slice() == b"require"
            && call.receiver().is_none()
            && let Some(args) = call.arguments()
            && let Some(feature) = args.arguments().iter().next()
            && let Some(feature) = string(&feature)
        {
            self.requires.push(feature);
        }
        ruby_prism::visit_call_node(self, call);
    }

    fn visit_class_node(&mut self, node: &ruby_prism::ClassNode<'pr>) {
        self.declares = true;
        ruby_prism::visit_class_node(self, node);
    }

    fn visit_module_node(&mut self, node: &ruby_prism::ModuleNode<'pr>) {
        self.declares = true;
        ruby_prism::visit_module_node(self, node);
    }
}

fn loads(source: &[u8]) -> Loads {
    let parsed = ruby_prism::parse(source);
    let mut found = Loads::default();
    found.visit(&parsed.node());
    found
}

/// A gem Ruby ships inside its stdlib, and the stdlib files that are its.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DefaultGem {
    pub(crate) name: String,
    pub(crate) version: String,
    /// Relative to the stdlib root, Ruby files only.
    pub(crate) files: Vec<String>,
}

/// Every default gem of the stdlib at `root`, from the gemspecs rubygems
/// wrote for them — read, never run.
pub(crate) fn default_gems(root: &Path) -> Vec<DefaultGem> {
    let Some(dir) = specifications(root) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut gems: Vec<DefaultGem> = entries
        .flatten()
        .filter_map(|entry| {
            let file = entry.file_name().to_string_lossy().into_owned();
            let (name, version) = split_dir(file.strip_suffix(".gemspec")?)?;
            let source = std::fs::read(entry.path()).ok()?;
            Some(DefaultGem {
                name: name.to_string(),
                version: version.to_string(),
                files: spec_files(&source)
                    .into_iter()
                    .filter(|path| scan::is_ruby(path))
                    .collect(),
            })
        })
        .collect();
    gems.sort_by(|a, b| a.name.cmp(&b.name));
    gems
}

/// The literal `s.files = [...]` of a gemspec.
fn spec_files(source: &[u8]) -> Vec<String> {
    #[derive(Default)]
    struct Files(Vec<String>);
    impl<'pr> Visit<'pr> for Files {
        fn visit_call_node(&mut self, call: &ruby_prism::CallNode<'pr>) {
            if call.name().as_slice() == b"files="
                && let Some(args) = call.arguments()
                && let Some(list) = args.arguments().iter().next()
                && let Some(list) = list.as_array_node()
            {
                self.0
                    .extend(list.elements().iter().filter_map(|node| string(&node)));
            }
            ruby_prism::visit_call_node(self, call);
        }
    }
    let parsed = ruby_prism::parse(source);
    let mut files = Files::default();
    files.visit(&parsed.node());
    files.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_tooling_and_opt_in_extensions_and_keeps_what_apps_call() {
        for path in [
            "bundler/cli.rb",
            "rubygems/core_ext/kernel_require.rb",
            "irb.rb",
            "json/add/time.rb",
            "un.rb",
        ] {
            assert!(skipped(path), "{path}");
        }
        for path in [
            "bundler.rb",
            "rubygems.rb",
            "rubygems/version.rb",
            "json.rb",
            "json/common.rb",
            "set.rb",
            "irbx.rb",
        ] {
            assert!(!skipped(path), "{path}");
        }
    }

    #[test]
    fn reads_a_default_gems_files_from_its_gemspec() {
        let spec = br#"Gem::Specification.new do |s|
  s.name = "widget".freeze
  s.files = ["README.md".freeze, "ext/widget/extconf.rb".freeze, "widget.rb".freeze, "widget/core.rb".freeze]
end
"#;
        assert_eq!(
            spec_files(spec),
            [
                "README.md",
                "ext/widget/extconf.rb",
                "widget.rb",
                "widget/core.rb"
            ]
        );
    }

    #[test]
    fn a_file_is_compiled_when_it_loads_an_extension_or_lives_under_one() {
        let root = std::env::temp_dir().join(format!("trekr-compiled-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let arch = root.join("arm64-darwin");
        std::fs::create_dir_all(arch.join("widget")).unwrap();
        std::fs::create_dir_all(arch.join("gadget.bundle.dSYM")).unwrap();
        std::fs::write(arch.join("rbconfig.rb"), "").unwrap();
        for ext in ["gadget.bundle", "widget/core.so"] {
            std::fs::write(arch.join(ext), "").unwrap();
        }
        let sources = [
            ("gadget.rb", "require 'gadget.so'\nclass Gadget\nend\n"),
            ("gadget/loader.rb", "require 'gadget.so'\n"),
            ("gizmo.rb", "require 'gadget/loader'\nmodule Gizmo\nend\n"),
            ("widget/core/extra.rb", "class Widget\nend\n"),
            ("plain.rb", "require 'set'\nclass Plain\nend\n"),
            ("printer.rb", "require 'gadget.so'\nclass Printer\nend\n"),
        ];
        let mut files = scan::Files::new();
        for (path, source) in sources {
            let at = root.join(path);
            std::fs::create_dir_all(at.parent().unwrap()).unwrap();
            std::fs::write(&at, source).unwrap();
            files.insert(path.to_string(), scan::hash_blob(source.as_bytes()));
        }
        assert_eq!(
            compiled(&root, &files),
            [
                ("gadget.rb".to_string(), "gadget".to_string()),
                ("gadget/loader.rb".to_string(), "gadget".to_string()),
                ("gizmo.rb".to_string(), "gadget".to_string()),
                (
                    "widget/core/extra.rb".to_string(),
                    "widget/core".to_string()
                ),
            ]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_highest_rbs_with_signatures_to_read_is_picked() {
        let root = std::env::temp_dir().join(format!("trekr-rbs-pick-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (bundled, installed) = (root.join("bundled"), root.join("installed"));
        for (dir, gem) in [
            (&bundled, "rbs-3.8.0"),
            (&installed, "rbs-3.10.1"),
            (&installed, "rbs-4.0.0"),
            (&installed, "rbs-inline-9.0.0"),
        ] {
            let core = dir.join(gem).join("core");
            // 4.0.0 is a directory left with nothing to read.
            if gem != "rbs-4.0.0" {
                std::fs::create_dir_all(&core).unwrap();
            } else {
                std::fs::create_dir_all(dir.join(gem)).unwrap();
            }
        }
        let picked = rbs_in(&[bundled.clone(), installed.clone()]).unwrap();
        assert_eq!(picked.version, "3.10.1");
        assert_eq!(picked.dir, installed.join("rbs-3.10.1"));
        assert_eq!(rbs_in(&[root.join("none")]), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_rbs_installed_with_the_ruby_is_its_own_not_a_later_higher_one() {
        let root = std::env::temp_dir().join(format!("trekr-rbs-bundled-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let lib = root.join("lib/ruby");
        let stdlib = lib.join("9.8.0");
        let gems = lib.join("gems/9.8.0");
        std::fs::create_dir_all(&stdlib).unwrap();
        std::fs::create_dir_all(gems.join("specifications/default")).unwrap();
        let when = |secs: u64| std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
        let touch = |path: &Path, secs: u64| {
            std::fs::write(path, "").unwrap();
            std::fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(when(secs))
                .unwrap();
        };
        for spec in ["json-2.9.1", "set-1.1.1", "uri-1.0.3"] {
            touch(
                &gems.join(format!("specifications/default/{spec}.gemspec")),
                1_000_000,
            );
        }
        std::fs::create_dir_all(gems.join("cache")).unwrap();
        for (gem, secs) in [("rbs-3.8.0", 1_000_005), ("rbs-4.2.0", 9_000_000)] {
            std::fs::create_dir_all(gems.join("gems").join(gem).join("core")).unwrap();
            touch(
                &gems.join("specifications").join(format!("{gem}.gemspec")),
                secs,
            );
            touch(&gems.join("cache").join(format!("{gem}.gem")), secs);
        }
        let stdlib_at = Stdlib {
            root: stdlib.clone(),
            ruby: String::new(),
            ships: HashSet::new(),
        };
        let picked = || {
            let gem = stdlib_at.rbs().unwrap();
            (gem.version, gem.chosen)
        };
        let bundled = ("3.8.0".to_string(), Chosen::Bundled);
        assert_eq!(picked(), bundled);
        // `gem update --system` writes a newer default spec or two.
        for spec in ["rubygems-update-4.0.0", "bundler-4.0.0"] {
            touch(
                &gems.join(format!("specifications/default/{spec}.gemspec")),
                9_500_000,
            );
        }
        assert_eq!(picked(), bundled, "after gem update --system");
        // `gem pristine rbs` rewrites its gemspec; its cached `.gem`, dated
        // from Ruby's tarball, is only read.
        touch(&gems.join("specifications/rbs-3.8.0.gemspec"), 9_600_000);
        touch(&gems.join("cache/rbs-3.8.0.gem"), 900_000);
        assert_eq!(picked(), bundled, "after gem pristine rbs");
        // Ruby's own list of what it bundled comes first.
        std::fs::create_dir_all(root.join("gems")).unwrap();
        std::fs::write(
            root.join("gems/bundled_gems"),
            "# gem-name version repository\nminitest 5.25.4 https://github.com/minitest/minitest\n\
             rbs 4.2.0 https://github.com/ruby/rbs\n",
        )
        .unwrap();
        assert_eq!(picked(), ("4.2.0".to_string(), Chosen::Bundled));
        std::fs::remove_file(root.join("gems/bundled_gems")).unwrap();
        // With nothing written near the Ruby's install, the highest installed.
        std::fs::remove_file(gems.join("specifications/rbs-3.8.0.gemspec")).unwrap();
        std::fs::remove_file(gems.join("cache/rbs-3.8.0.gem")).unwrap();
        assert_eq!(picked(), ("4.2.0".to_string(), Chosen::Installed));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_ruby_is_matched_by_as_much_of_its_version_as_is_named() {
        let v = |s: &str| Version::parse(s).unwrap();
        assert_eq!(v("3.4.9").truncated(2), v("3.4"));
        assert_eq!(v("3.4.9").truncated(3), v("3.4.9"));
        assert_ne!(v("3.4.9").truncated(3), v("3.4.8"));
        assert_eq!(
            install_version(Path::new("/x/ruby-3.4.9")),
            Some(v("3.4.9"))
        );
        assert_eq!(install_version(Path::new("/x/3.4.9_1")), Some(v("3.4.9")));
    }
}
