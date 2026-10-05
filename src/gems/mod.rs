//! Which gems does this checkout use, and where are they on disk?
//!
//! Without running Ruby. `Gemfile.lock` is a plain text file with a documented
//! shape, every gem manager on earth unpacks into `.../gems/<name>-<version>/`,
//! and bundler checks a git source out into `.../bundler/gems/<repo>-<sha>/`,
//! so both halves are answerable by reading. Shelling out to `bundle` would
//! need the project's Ruby and its bundle to be installed and working — the
//! exact dependency PLAN §1 says is the product's first edge.
//!
//! A gem is keyed by its directory, which already encodes `(name, version)`
//! or the git revision, so
//! two projects on one machine that use `activesupport 7.1.0` share one index.
//! A gem's bytes never change, which makes this the best case the blob store
//! has.

use std::path::{Path, PathBuf};

mod declared;
pub(crate) mod stdlib;

/// Where a lockfile says a gem comes from. It decides where to look, and
/// whether to look at all.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Source {
    /// A packaged gem, unpacked into `.../gems/<name>-<version>/`.
    Registry,
    /// A git dependency. Bundler checks each revision out once, into
    /// `bundler/gems/<checkout>/`, and that one checkout may hold several
    /// gems — a monorepo's, each in its own subdirectory.
    Git {
        /// `<repo name>-<first 12 of the revision>`, as bundler names it.
        checkout: String,
    },
    /// A path dependency: `remote:` as written, relative to the Gemfile.
    Path { remote: String },
}

/// A gem the lockfile names.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Gem {
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) source: Source,
}

impl Gem {
    /// The directory name every packager unpacks into.
    fn dir_name(&self) -> String {
        format!("{}-{}", self.name, self.version)
    }
}

/// A gem that was named, and where it turned out to be.
#[derive(Debug)]
pub(crate) struct Located {
    pub(crate) gem: Gem,
    pub(crate) place: Place,
    /// Requirements as written that could not be read without running
    /// them (`version`, an interpolation): the pick ignored them.
    pub(crate) unread: Vec<String>,
    /// Found only in another Ruby's gem directories than the checkout's
    /// own: that Ruby has no copy that would do (DEC-291).
    pub(crate) elsewhere: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Place {
    /// Its own directory, indexed as a gem.
    Dir(PathBuf),
    /// A path gem whose source is inside the checkout, and so already
    /// indexed with it. Not a hole: rails' own lockfile names 12 of them.
    InCheckout,
    /// Not indexed. Reported, never silently dropped — a hole in every
    /// answer that would have come from it — and with the reason, because
    /// each has a different fix.
    Missing(Absence),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Absence {
    NotInstalled,
    /// A git gem whose revision is checked out in no `bundler/gems/`; the
    /// path is where bundler would have put it.
    NoCheckout(PathBuf),
    /// The checkout is there and holds no gemspec by this gem's name.
    NoGemspec(PathBuf),
    /// A path gem outside the checkout. Live source, often a repository of
    /// its own, so it is neither a pinned gem nor part of this checkout.
    OutsidePath(PathBuf),
    NoPath(PathBuf),
    /// A default gem at the version its Ruby ships: the directory is empty
    /// and the code is that Ruby's stdlib, here.
    DefaultGem(PathBuf),
    /// Without a lockfile, requirements from several places that no
    /// installed version meets together, in words naming each place.
    Conflict(String),
    /// Without a lockfile, a Gemfile's git gem with no one checkout to
    /// take, in words saying why (DEC-293).
    GitUnlocked(String),
}

impl Absence {
    /// Why, in words a person can act on.
    pub(crate) fn why(&self, name: &str) -> String {
        let at = |path: &Path| crate::core::paths::pretty(&path.to_string_lossy());
        match self {
            Absence::NotInstalled => "not installed".into(),
            Absence::NoCheckout(path) => format!("git source, checkout not found at {}", at(path)),
            Absence::NoGemspec(path) => format!("git source, no {name}.gemspec in {}", at(path)),
            Absence::OutsidePath(path) => {
                format!(
                    "path source outside this checkout, not indexed: {}",
                    at(path)
                )
            }
            Absence::NoPath(path) => format!("path source, not found at {}", at(path)),
            Absence::DefaultGem(stdlib) => {
                format!(
                    "default gem, its code is Ruby's stdlib in {}, not indexed",
                    at(stdlib)
                )
            }
            Absence::Conflict(said) | Absence::GitUnlocked(said) => said.clone(),
        }
    }
}

/// Parse the `specs:` blocks of a `Gemfile.lock`, each gem with its source.
///
/// A gem is a line indented exactly four spaces reading `name (version)`; its
/// own dependencies are indented six and are already listed elsewhere, so they
/// are skipped rather than deduplicated later. A section's header lines —
/// `remote:`, `revision:` — are indented two and come before its `specs:`.
pub(crate) fn parse_lockfile(text: &str) -> Vec<Gem> {
    let mut gems = Vec::new();
    let mut in_specs = false;
    let mut section = "";
    let mut remote = "";
    let mut revision = "";
    let mut source = Source::Registry;
    for line in text.lines() {
        let trimmed = line.trim_end();
        if trimmed.trim_start() == "specs:" {
            in_specs = true;
            source = match section {
                "GIT" => Source::Git {
                    checkout: git_checkout_name(remote, revision),
                },
                "PATH" => Source::Path {
                    remote: remote.to_string(),
                },
                _ => Source::Registry,
            };
            continue;
        }
        // A non-indented, non-empty line ends the section and names the next.
        if !trimmed.is_empty() && !trimmed.starts_with(' ') {
            in_specs = false;
            section = trimmed;
            remote = "";
            revision = "";
            continue;
        }
        if !in_specs {
            if let Some(value) = trimmed.strip_prefix("  remote: ") {
                remote = value;
            } else if let Some(value) = trimmed.strip_prefix("  revision: ") {
                revision = value;
            }
            continue;
        }
        let indent = trimmed.len() - trimmed.trim_start().len();
        if indent != 4 {
            continue;
        }
        let body = trimmed.trim_start();
        let Some((name, rest)) = body.split_once(" (") else {
            continue;
        };
        let Some(version) = rest.strip_suffix(')') else {
            continue;
        };
        // A platform-specific pin reads `nokogiri (1.16.0-arm64-darwin)`; the
        // directory on disk carries the platform too, so keep it whole.
        if name.is_empty() || version.is_empty() {
            continue;
        }
        gems.push(Gem {
            name: name.to_string(),
            version: version.to_string(),
            source: source.clone(),
        });
    }
    gems.sort();
    gems.dedup();
    gems
}

/// The directory bundler checks a git source out into: the repository's
/// name, then the revision's first 12 characters. Mirrors
/// `Bundler::Source::Git#base_name` and `#shortref_for_path`.
fn git_checkout_name(remote: &str, revision: &str) -> String {
    let short: String = revision.chars().take(12).collect();
    format!("{}-{short}", repo_name(remote))
}

/// A remote's repository name, as bundler names its checkout: the last
/// component, less `.git`. `github: "owner/repo"` reads the same way.
fn repo_name(remote: &str) -> &str {
    let base = remote
        .trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()
        .unwrap_or_default();
    base.strip_suffix(".git").unwrap_or(base)
}

/// Directory patterns gems are unpacked into, most specific first. A single
/// `*` in a component means "try every directory here".
fn search_roots(repo: &Path) -> Vec<PathBuf> {
    let mut roots = project_roots(repo);
    roots.extend(environment_roots());
    roots.extend(machine_roots().into_iter().map(|(pattern, _)| pattern));
    roots
}

/// Where the project itself has bundler install: always searched first.
fn project_roots(repo: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    // Bundler's configured path, when there is one, is where it installed.
    if let Some(path) = bundle_path(repo) {
        roots.push(path.join("ruby/*/gems"));
    }
    // Vendored into the project wins over the machine's: it is what the
    // project actually resolves.
    roots.push(repo.join("vendor/bundle/ruby/*/gems"));
    roots.push(repo.join(".bundle/ruby/*/gems"));
    roots
}

/// `$GEM_HOME` and `$GEM_PATH`: the Ruby a version manager made current.
fn environment_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(home) = std::env::var("GEM_HOME")
        && !home.is_empty()
    {
        roots.push(PathBuf::from(home).join("gems"));
    }
    if let Ok(paths) = std::env::var("GEM_PATH") {
        for entry in paths.split(':').filter(|p| !p.is_empty()) {
            roots.push(PathBuf::from(entry).join("gems"));
        }
    }
    roots
}

/// Whether a machine root names a Ruby by its installed version
/// (`versions/3.4.9`, `ruby-3.4.9`) or only by its ABI (`gems/3.4.0`).
#[derive(Clone, Copy, PartialEq)]
enum Named {
    Install,
    Abi,
}

/// Where each version manager installs a Ruby, as a pattern whose last
/// component is the install, named for its version: rvm, rbenv, asdf,
/// chruby's two, and mise's.
fn version_managers() -> Vec<PathBuf> {
    let mut patterns = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        let home = PathBuf::from(home);
        patterns.push(home.join(".rvm/rubies/*"));
        patterns.push(home.join(".rbenv/versions/*"));
        patterns.push(home.join(".asdf/installs/ruby/*"));
        patterns.push(home.join(".rubies/*"));
    }
    patterns.push(system("/opt/rubies/*"));
    if let Some(mise) = mise_data() {
        patterns.push(mise.join("installs/ruby/*"));
    }
    patterns
}

/// mise's data directory: `$MISE_DATA_DIR`, else under `$XDG_DATA_HOME`,
/// else `~/.local/share/mise`.
fn mise_data() -> Option<PathBuf> {
    let set = |var: &str| {
        std::env::var_os(var)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    set("MISE_DATA_DIR")
        .or_else(|| set("XDG_DATA_HOME").map(|data| data.join("mise")))
        .or_else(|| set("HOME").map(|home| home.join(".local/share/mise")))
}

/// Homebrew's Rubies, `Cellar/ruby/3.4.9` and `Cellar/ruby@3.3/3.3.11`: a
/// formula per minor version, which a `*` component cannot match by prefix.
fn homebrew_kegs() -> Vec<PathBuf> {
    ["/opt/homebrew/Cellar", "/usr/local/Cellar"]
        .iter()
        .flat_map(|cellar| {
            std::fs::read_dir(system(cellar))
                .into_iter()
                .flatten()
                .flatten()
        })
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name == "ruby" || name.starts_with("ruby@")
        })
        .flat_map(|entry| expand(&entry.path().join("*")))
        .collect()
}

/// A machine-wide path where Rubies are installed. `TREKR_TEST_SYSTEM`
/// stands in for `/`, so a test sees only the Rubies it stages, whatever
/// the machine running it has installed.
fn system(path: &str) -> PathBuf {
    match std::env::var_os("TREKR_TEST_SYSTEM") {
        Some(root) => PathBuf::from(root).join(path.trim_start_matches('/')),
        None => PathBuf::from(path),
    }
}

/// Every Ruby installed on the machine, by convention.
fn machine_roots() -> Vec<(PathBuf, Named)> {
    let mut roots = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        let home = PathBuf::from(home);
        roots.push((home.join(".gem/ruby/*/gems"), Named::Abi));
        roots.push((
            home.join(".rbenv/versions/*/lib/ruby/gems/*/gems"),
            Named::Install,
        ));
        roots.push((home.join(".rvm/gems/*/gems"), Named::Install));
        roots.push((
            home.join(".asdf/installs/ruby/*/lib/ruby/gems/*/gems"),
            Named::Install,
        ));
        roots.push((home.join(".rubies/*/lib/ruby/gems/*/gems"), Named::Install));
    }
    roots.push((system("/opt/rubies/*/lib/ruby/gems/*/gems"), Named::Install));
    if let Some(mise) = mise_data() {
        roots.push((
            mise.join("installs/ruby/*/lib/ruby/gems/*/gems"),
            Named::Install,
        ));
    }
    for keg in homebrew_kegs() {
        roots.push((keg.join("lib/ruby/gems/*/gems"), Named::Install));
    }
    for system in [
        "/opt/homebrew/lib/ruby/gems/*/gems",
        "/usr/local/lib/ruby/gems/*/gems",
        "/usr/lib/ruby/gems/*/gems",
        "/Library/Ruby/Gems/*/gems",
    ] {
        roots.push((PathBuf::from(system), Named::Abi));
    }
    roots
}

/// The gem directories of the one Ruby a checkout without a lockfile runs
/// on, and how it was chosen. A pick is "the highest installed", and
/// installed means installed for *that* Ruby: another's copy may be newer
/// and was never on this load path. In order: the version `.ruby-version`
/// or the Gemfile's `ruby` names, `$GEM_HOME`/`$GEM_PATH`, the `ruby` on
/// `$PATH`; with none of them, every Ruby, and that is said.
fn active_ruby_dirs(repo: &Path) -> (Vec<PathBuf>, String) {
    let mut dirs: Vec<PathBuf> = project_roots(repo).iter().flat_map(|p| expand(p)).collect();
    let machine = machine_dirs();
    let (found, how) =
        named_ruby_dirs(repo, &machine).unwrap_or_else(|| environment_ruby_dirs(machine));
    dirs.extend(found);
    (dirs, how)
}

fn machine_dirs() -> Vec<(PathBuf, Named)> {
    machine_roots()
        .into_iter()
        .flat_map(|(pattern, named)| expand(&pattern).into_iter().map(move |dir| (dir, named)))
        .collect()
}

/// The gem directories of the version `.ruby-version` or the Gemfile's
/// `ruby` names, when any is installed.
fn named_ruby_dirs(repo: &Path, machine: &[(PathBuf, Named)]) -> Option<(Vec<PathBuf>, String)> {
    let version = project_ruby(repo)?;
    let abi = abi_of(&version);
    let matching: Vec<PathBuf> = machine
        .iter()
        .filter(|(dir, named)| match named {
            Named::Install => dir.components().any(|c| {
                let c = c.as_os_str().to_string_lossy();
                let c = c.strip_prefix("ruby-").unwrap_or(&c);
                c == version
                    || c.starts_with(&format!("{version}."))
                    || c.starts_with(&format!("{version}@"))
            }),
            Named::Abi => abi.as_deref().is_some_and(|abi| has_component(dir, abi)),
        })
        .map(|(dir, _)| dir.clone())
        .collect();
    (!matching.is_empty()).then(|| {
        (
            matching,
            format!("Ruby {version}, which the checkout names"),
        )
    })
}

/// The gem directories of the Ruby the environment makes current —
/// `$GEM_HOME`/`$GEM_PATH`, else the `ruby` on `$PATH` — or, with neither,
/// every Ruby's, and how that was chosen.
fn environment_ruby_dirs(machine: Vec<(PathBuf, Named)>) -> (Vec<PathBuf>, String) {
    let mut dirs = Vec::new();
    let environment: Vec<PathBuf> = environment_roots().iter().flat_map(|p| expand(p)).collect();
    if !environment.is_empty() {
        dirs.extend(environment);
        let named = std::env::var("GEM_HOME").unwrap_or_default();
        return (
            dirs,
            format!(
                "the Ruby $GEM_HOME names ({})",
                crate::core::paths::pretty(&named)
            ),
        );
    }

    if let Some(ruby) = path_ruby() {
        // `<prefix>/bin/ruby`, and its gems in `<prefix>/lib/ruby/gems/<abi>`,
        // and the machine's other directories for the same ABI.
        let own: Vec<PathBuf> = ruby
            .parent()
            .and_then(Path::parent)
            .map(|prefix| expand(&prefix.join("lib/ruby/gems/*/gems")))
            .unwrap_or_default();
        let abis: Vec<String> = own
            .iter()
            .filter_map(|dir| dir.parent()?.file_name())
            .map(|abi| abi.to_string_lossy().into_owned())
            .collect();
        if !abis.is_empty() {
            dirs.extend(own);
            dirs.extend(
                machine
                    .iter()
                    .filter(|(dir, named)| {
                        *named == Named::Abi && abis.iter().any(|abi| has_component(dir, abi))
                    })
                    .map(|(dir, _)| dir.clone()),
            );
            let shown = crate::core::paths::pretty(&ruby.to_string_lossy());
            return (dirs, format!("the ruby on $PATH ({shown})"));
        }
    }

    dirs.extend(machine.into_iter().map(|(dir, _)| dir));
    (
        dirs,
        "every installed Ruby: none is named or current".into(),
    )
}

fn has_component(dir: &Path, name: &str) -> bool {
    dir.components().any(|c| c.as_os_str() == name)
}

/// `3.4.9` → `3.4.0`, the directory rubygems installs a Ruby's gems under.
fn abi_of(version: &str) -> Option<String> {
    let mut parts = version.split('.');
    let (major, minor) = (parts.next()?, parts.next()?);
    let numeric = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    (numeric(major) && numeric(minor)).then(|| format!("{major}.{minor}.0"))
}

/// The Ruby version the checkout names: its version file (`.ruby-version`,
/// `.tool-versions`, mise's), else the Gemfile's literal `ruby "3.4.1"`.
fn project_ruby(repo: &Path) -> Option<String> {
    if let Some((version, _)) = version_file(repo) {
        return Some(version);
    }
    let gemfile = crate::scan::read_text(repo.join("Gemfile")).ok()?;
    gemfile.lines().find_map(|line| {
        let rest = line.trim_start().strip_prefix("ruby ")?;
        ruby_version_text(rest.split(',').next()?)
    })
}

/// `3.4.1` from `ruby-3.4.1`, `"3.4.1"` or rvm's `ruby-3.4.1@gemset`;
/// `None` for anything that is not a version — `system`, `jruby-9.4`,
/// `>= 3.3`.
fn ruby_version_text(text: &str) -> Option<String> {
    let text = text.trim().trim_matches(|c| c == '"' || c == '\'');
    let text = text.strip_prefix("ruby-").unwrap_or(text);
    let text = text.split('@').next().unwrap_or(text);
    text.starts_with(|c: char| c.is_ascii_digit())
        .then(|| text.to_string())
}

/// The Ruby version a version manager reads from `dir`, and the file it is
/// read from: `.ruby-version` (rbenv, chruby, rvm, asdf and mise all read
/// it), `.tool-versions`' `ruby` line (asdf, mise), or mise's `[tools]`.
fn version_file(dir: &Path) -> Option<(String, PathBuf)> {
    let read = |name: &str| {
        let path = dir.join(name);
        crate::scan::read_text(&path).ok().map(|text| (text, path))
    };
    // Its first word, as rbenv reads it; one that names no install
    // (`system`, a blank file) leaves the others to.
    if let Some((text, path)) = read(".ruby-version")
        && let Some(version) = text.split_whitespace().next().and_then(ruby_version_text)
    {
        return Some((version, path));
    }
    if let Some((text, path)) = read(".tool-versions") {
        let version = text.lines().find_map(|line| {
            let mut fields = line.split_whitespace();
            (fields.next()? == "ruby").then(|| ruby_version_text(fields.next()?))?
        });
        if let Some(version) = version {
            return Some((version, path));
        }
    }
    ["mise.toml", ".mise.toml"].iter().find_map(|name| {
        let (text, path) = read(name)?;
        Some((mise_ruby(&text)?, path))
    })
}

/// `[tools]`' `ruby = "3.4"`, or the first of `ruby = ["3.4", …]`.
fn mise_ruby(toml: &str) -> Option<String> {
    let mut in_tools = false;
    toml.lines().find_map(|line| {
        let line = line.trim();
        if line.starts_with('[') {
            in_tools = line == "[tools]";
            return None;
        }
        let value = line
            .strip_prefix("ruby")
            .or_else(|| line.strip_prefix("\"ruby\""))?
            .trim_start()
            .strip_prefix('=')?;
        let quoted = value.split('"').nth(1)?;
        in_tools.then(|| ruby_version_text(quoted))?
    })
}

/// The Ruby a checkout's `Gemfile.lock` was locked on: its `RUBY VERSION`,
/// which bundler writes when the Gemfile has a `ruby` line. `ruby 3.4.7p58`
/// is `3.4.7`.
fn lockfile_ruby(repo: &Path) -> Option<String> {
    let text = crate::scan::read_text(repo.join("Gemfile.lock")).ok()?;
    let mut lines = text.lines().skip_while(|line| *line != "RUBY VERSION");
    lines.next()?;
    let written = lines.next()?.trim().strip_prefix("ruby ")?;
    let written = written.split_whitespace().next()?;
    // A development Ruby's patchlevel is `p-1`.
    let version = match written.rsplit_once('p') {
        Some((version, patch))
            if patch
                .trim_start_matches('-')
                .chars()
                .all(|c| c.is_ascii_digit()) =>
        {
            version
        }
        _ => written,
    };
    ruby_version_text(version)
}

/// The `ruby` executable `$PATH` finds, resolved through symlinks.
fn path_ruby() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("ruby"))
        .find(|candidate| candidate.is_file())
        .and_then(|ruby| std::fs::canonicalize(ruby).ok())
}

/// Expand `*` components by reading the directory, depth-first.
///
/// A tiny glob rather than a crate: the only pattern needed is a whole
/// component of `*`, and the version directories it matches are few.
fn expand(pattern: &Path) -> Vec<PathBuf> {
    let mut current: Vec<PathBuf> = vec![PathBuf::new()];
    for component in pattern.components() {
        let part = component.as_os_str();
        if part != "*" {
            for path in &mut current {
                path.push(part);
            }
            continue;
        }
        let mut next = Vec::new();
        for path in &current {
            let Ok(entries) = std::fs::read_dir(path) else {
                continue;
            };
            for entry in entries.flatten() {
                // A version manager's install may be a link to one elsewhere.
                let dir = entry
                    .file_type()
                    .is_ok_and(|t| t.is_dir() || (t.is_symlink() && entry.path().is_dir()));
                if dir {
                    next.push(entry.path());
                }
            }
        }
        current = next;
        if current.is_empty() {
            return current;
        }
    }
    current
}

/// `BUNDLE_PATH`, as bundler reads it: the app's `.bundle/config` (or
/// `$BUNDLE_APP_CONFIG`'s), then the environment, then `~/.bundle/config`.
/// Relative to the app. Gems go under its `ruby/<version>/`.
fn bundle_path(repo: &Path) -> Option<PathBuf> {
    let read = |dir: PathBuf| {
        let text = crate::scan::read_text(dir.join("config")).ok()?;
        config_value(&text, "BUNDLE_PATH")
    };
    let local = std::env::var_os("BUNDLE_APP_CONFIG")
        .map_or_else(|| repo.join(".bundle"), |dir| repo.join(dir));
    let path = read(local)
        .or_else(|| std::env::var("BUNDLE_PATH").ok().filter(|p| !p.is_empty()))
        .or_else(|| read(PathBuf::from(std::env::var_os("HOME")?).join(".bundle")))?;
    Some(repo.join(path))
}

/// One key of a bundler config file — flat YAML, `KEY: "value"`.
fn config_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let value = line.strip_prefix(key)?.strip_prefix(':')?.trim();
        let value = value.trim_matches(|c| c == '"' || c == '\'');
        (!value.is_empty()).then(|| value.to_string())
    })
}

/// Every directory gems are unpacked into, in search-root order. Expanded
/// once, not once per gem: there are ~100 gems and a handful of roots.
fn gem_dirs(repo: &Path) -> Vec<PathBuf> {
    search_roots(repo).iter().flat_map(|p| expand(p)).collect()
}

/// Find each gem's source, in search-root order: the project's own, then
/// the checkout's Ruby's, then every other (DEC-291). A gem found only past
/// the Ruby's is `elsewhere` — unless the Ruby ships that version as a
/// default gem, whose code is its stdlib.
pub(crate) fn locate(repo: &Path, gems: Vec<Gem>, ruby: Option<&stdlib::Stdlib>) -> Vec<Located> {
    let mut roots: Vec<PathBuf> = project_roots(repo).iter().flat_map(|p| expand(p)).collect();
    let project = roots.len();
    roots.extend(ruby.map(|ruby| ruby.gem_dirs()).unwrap_or_default());
    // Past this index is another Ruby's; with no Ruby chosen, there is none.
    let own = match ruby {
        Some(_) => roots.len(),
        None => usize::MAX,
    };
    for dir in gem_dirs(repo) {
        if !roots.contains(&dir) {
            roots.push(dir);
        }
    }
    // Bundler checks git sources out beside `gems/`, in `bundler/gems/`.
    let git_roots: Vec<PathBuf> = roots
        .iter()
        .map(|root| root.parent().unwrap_or(root).join("bundler/gems"))
        .collect();
    // Where bundler on the checkout's Ruby would check one out.
    let rubys_git = match ruby {
        Some(_) => &git_roots[project..own],
        None => &[],
    };
    let checkout = std::fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf());
    // A source naming one gem owns its one gemspec, whatever the file is called.
    let sole = |source: &Source| gems.iter().filter(|g| g.source == *source).count() == 1;

    let places: Vec<(Place, bool)> = gems
        .iter()
        .map(|gem| match &gem.source {
            Source::Registry => {
                let dir = gem.dir_name();
                let Some((at, found)) = roots
                    .iter()
                    .map(|root| root.join(&dir))
                    .enumerate()
                    .find(|(_, candidate)| candidate.is_dir())
                else {
                    return (Place::Missing(Absence::NotInstalled), false);
                };
                match ruby {
                    Some(ruby) if at >= own && ruby.ships(&gem.name, &gem.version) => (
                        Place::Missing(Absence::DefaultGem(ruby.root.clone())),
                        false,
                    ),
                    _ => (Place::Dir(found), at >= own),
                }
            }
            Source::Git { checkout: name } => {
                let Some((at, found)) = git_roots
                    .iter()
                    .map(|r| r.join(name))
                    .enumerate()
                    .find(|(_, c)| c.is_dir())
                else {
                    // Where bundler would have put it: the first that exists,
                    // the checkout's Ruby's before any other's.
                    let expected = rubys_git
                        .iter()
                        .find(|r| r.is_dir())
                        .or(rubys_git.first())
                        .or_else(|| git_roots.iter().find(|r| r.is_dir()))
                        .or(git_roots.first())
                        .map_or_else(|| Path::new("bundler/gems").join(name), |r| r.join(name));
                    return (Place::Missing(Absence::NoCheckout(expected)), false);
                };
                match gemspec_dir(&found, &gem.name, sole(&gem.source)) {
                    Some(dir) => (Place::Dir(dir), at >= own),
                    None => (Place::Missing(Absence::NoGemspec(found)), false),
                }
            }
            Source::Path { remote } => {
                let Ok(dir) = std::fs::canonicalize(checkout.join(remote)) else {
                    return (
                        Place::Missing(Absence::NoPath(checkout.join(remote))),
                        false,
                    );
                };
                let place = match dir.starts_with(&checkout) {
                    true => Place::InCheckout,
                    false => Place::Missing(Absence::OutsidePath(
                        gemspec_dir(&dir, &gem.name, sole(&gem.source)).unwrap_or(dir),
                    )),
                };
                (place, false)
            }
        })
        .collect();
    gems.into_iter()
        .zip(places)
        .map(|(gem, (place, elsewhere))| Located {
            gem,
            place,
            unread: Vec::new(),
            elsewhere,
        })
        .collect()
}

/// The directory of `name`'s gemspec within a source, searched where bundler
/// searches (`{,*,*/*}.gemspec`), shallowest first. A monorepo keeps each gem
/// in its own subdirectory: rails' `actionpack/actionpack.gemspec`.
fn gemspec_dir(source: &Path, name: &str, sole: bool) -> Option<PathBuf> {
    let levels = [source.to_path_buf(), source.join("*"), source.join("*/*")];
    let named = format!("{name}.gemspec");
    let found = levels
        .iter()
        .flat_map(|level| expand(level))
        .find(|dir| dir.join(&named).is_file());
    if found.is_some() || !sole {
        return found;
    }
    // A gem whose gemspec is named for something else, alone in its source.
    let mut specs = levels.iter().flat_map(|level| expand(level)).filter(|dir| {
        std::fs::read_dir(dir).is_ok_and(|entries| {
            entries
                .flatten()
                .any(|e| e.path().extension().is_some_and(|x| x == "gemspec"))
        })
    });
    let only = specs.next()?;
    specs.next().is_none().then_some(only)
}

/// Where a checkout's gem list came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Resolved {
    /// `Gemfile.lock`, exactly as bundler locked it.
    Lockfile,
    /// No lockfile: what the gemspecs and Gemfile declare, each at the
    /// highest version installed for one Ruby — `ruby` says which, and how
    /// it was chosen (DEC-134, DEC-152).
    Declared { ruby: String },
}

impl Resolved {
    /// `gems.resolved_from`.
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Resolved::Lockfile => "lockfile",
            Resolved::Declared { .. } => "declared",
        }
    }
}

/// The gems a checkout depends on, located on disk, and where the list came
/// from; `None` when nothing names any. `ruby` is the Ruby the checkout runs
/// on (DEC-271), whose gems are looked for first (DEC-291).
///
/// An absent `Gemfile.lock` is not an error: most gems commit none, and their
/// gemspec says the same thing less exactly.
pub(crate) fn for_checkout(
    repo: &Path,
    ruby: Option<&stdlib::Stdlib>,
) -> (Vec<Located>, Option<Resolved>) {
    let (mut located, resolved) = match crate::scan::read_text(repo.join("Gemfile.lock")) {
        Ok(text) => (
            locate(repo, parse_lockfile(&text), ruby),
            Some(Resolved::Lockfile),
        ),
        Err(_) => {
            // The checkout's Ruby's gems, then — for a name that Ruby has
            // none of — the environment's Ruby's, whose `bundle install` it
            // would have been. With no Ruby chosen, DEC-152's choice alone.
            let (dirs, fallback, how) = match ruby {
                Some(ruby) => {
                    let mut dirs: Vec<PathBuf> =
                        project_roots(repo).iter().flat_map(|p| expand(p)).collect();
                    dirs.extend(ruby.gem_dirs());
                    let (shell, _) = environment_ruby_dirs(machine_dirs());
                    let fallback: Vec<PathBuf> =
                        shell.into_iter().filter(|d| !dirs.contains(d)).collect();
                    (dirs, fallback, ruby.ruby.clone())
                }
                None => {
                    let (dirs, how) = active_ruby_dirs(repo);
                    (dirs, Vec::new(), how)
                }
            };
            match declared::resolve(repo, &dirs, &fallback) {
                Some(located) => (located, Some(Resolved::Declared { ruby: how })),
                None => (Vec::new(), None),
            }
        }
    };
    for entry in &mut located {
        if let Place::Dir(dir) = &entry.place
            && let Some(stdlib) = default_gem_stdlib(dir)
        {
            entry.place = Place::Missing(Absence::DefaultGem(stdlib));
        }
    }
    (located, resolved)
}

/// Where a default gem's code is, when `dir` is one: rubygems gives it an
/// empty `gems/<name>-<version>/` and a spec in `specifications/default/`,
/// and its files live in the Ruby's stdlib, `lib/ruby/<abi>/`.
fn default_gem_stdlib(dir: &Path) -> Option<PathBuf> {
    if dir.join("lib").is_dir() {
        return None;
    }
    let dir = std::fs::canonicalize(dir).ok()?;
    let base = dir.parent()?.parent()?;
    let spec = base
        .join("specifications/default")
        .join(format!("{}.gemspec", dir.file_name()?.to_string_lossy()));
    if !spec.is_file() {
        return None;
    }
    Some(base.parent()?.parent()?.join(base.file_name()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCKFILE: &str = "\
GIT
  remote: https://github.com/example/widget.git
  revision: abc123
  specs:
    widget (0.1.0)
      activesupport

PATH
  remote: engines/billing
  specs:
    billing (1.0.0)

GEM
  remote: https://rubygems.org/
  specs:
    actionpack (7.1.0)
      activesupport (= 7.1.0)
      rack (>= 2.2.4)
    activesupport (7.1.0)
      concurrent-ruby (~> 1.0)
    nokogiri (1.16.0-arm64-darwin)
      racc (~> 1.4)

PLATFORMS
  arm64-darwin-23

DEPENDENCIES
  rails
  nokogiri

BUNDLED WITH
   2.5.3
";

    #[test]
    fn reads_every_specs_block_and_ignores_nested_dependencies() {
        let gems = parse_lockfile(LOCKFILE);
        let names: Vec<&str> = gems.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "actionpack",
                "activesupport",
                "billing",
                "nokogiri",
                "widget"
            ],
            "GIT, PATH and GEM sections all count; six-space dependency lines do not"
        );
        assert_eq!(gems[1].version, "7.1.0");
    }

    #[test]
    fn keeps_a_platform_suffix_because_the_directory_has_one_too() {
        let gems = parse_lockfile(LOCKFILE);
        let nokogiri = gems.iter().find(|g| g.name == "nokogiri").unwrap();
        assert_eq!(nokogiri.version, "1.16.0-arm64-darwin");
        assert_eq!(nokogiri.dir_name(), "nokogiri-1.16.0-arm64-darwin");
    }

    #[test]
    fn sections_that_are_not_specs_contribute_nothing() {
        // DEPENDENCIES and PLATFORMS are indented too, and must not be read as
        // gems just because they follow one.
        let gems = parse_lockfile(LOCKFILE);
        assert!(!gems.iter().any(|g| g.name == "rails"), "{gems:?}");
    }

    #[test]
    fn each_gem_remembers_which_section_named_it() {
        let gems = parse_lockfile(LOCKFILE);
        let by = |name: &str| gems.iter().find(|g| g.name == name).unwrap().source.clone();
        assert_eq!(
            by("widget"),
            Source::Git {
                checkout: "widget-abc123".into()
            }
        );
        assert_eq!(
            by("billing"),
            Source::Path {
                remote: "engines/billing".into()
            }
        );
        assert_eq!(by("activesupport"), Source::Registry);
    }

    #[test]
    fn a_git_checkout_is_named_as_bundler_names_it() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        for remote in [
            "https://github.com/example/widget.git",
            "https://github.com/example/widget",
            "git@github.com:example/widget.git",
            "git@host:widget.git",
            "/srv/repos/widget/",
        ] {
            assert_eq!(
                git_checkout_name(remote, sha),
                "widget-0123456789ab",
                "{remote}"
            );
        }
    }

    /// A scratch directory for one test, emptied first.
    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("trekr-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn gem(name: &str, source: Source) -> Gem {
        Gem {
            name: name.into(),
            version: "1.0.0".into(),
            source,
        }
    }

    #[test]
    fn a_path_gem_inside_the_checkout_is_not_a_hole() {
        let repo = scratch("path-in");
        std::fs::create_dir_all(repo.join("engines/billing")).unwrap();
        let located = locate(
            &repo,
            vec![gem(
                "billing",
                Source::Path {
                    remote: "engines/billing".into(),
                },
            )],
            None,
        );
        assert_eq!(located[0].place, Place::InCheckout);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn a_path_gem_outside_the_checkout_says_it_was_not_indexed() {
        let base = scratch("path-out");
        let repo = base.join("app");
        let shared = base.join("shared");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&shared).unwrap();
        let path = |remote: &str| {
            gem(
                "shared",
                Source::Path {
                    remote: remote.into(),
                },
            )
        };
        let located = locate(&repo, vec![path("../shared"), path("../gone")], None);
        let why: Vec<String> = located
            .iter()
            .map(|l| match &l.place {
                Place::Missing(absence) => absence.why("shared"),
                other => format!("{other:?}"),
            })
            .collect();
        assert!(why[0].contains("outside this checkout"), "{why:?}");
        assert!(why[1].contains("not found"), "{why:?}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_gem_that_is_not_on_disk_is_reported_rather_than_dropped() {
        let repo = scratch("gems");
        let located = locate(
            &repo,
            vec![gem("definitely-not-installed", Source::Registry)],
            None,
        );
        assert_eq!(located[0].place, Place::Missing(Absence::NotInstalled));
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn finds_a_gem_vendored_into_the_project() {
        let repo = scratch("vendor");
        let gem_dir = repo.join("vendor/bundle/ruby/3.3.0/gems/widget-1.0.0");
        std::fs::create_dir_all(&gem_dir).unwrap();
        let located = locate(&repo, vec![gem("widget", Source::Registry)], None);
        assert_eq!(located[0].place, Place::Dir(gem_dir));
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// A monorepo's git checkout holds each gem in a subdirectory; the gem
    /// is that subdirectory, and a revision nobody checked out is said so.
    #[test]
    fn finds_each_gem_of_a_git_monorepo_in_its_own_subdirectory() {
        let repo = scratch("git-mono");
        std::fs::create_dir_all(repo.join(".bundle")).unwrap();
        std::fs::write(repo.join(".bundle/config"), "---\nBUNDLE_PATH: \"store\"\n").unwrap();
        let checkout = repo.join("store/ruby/3.4.0/bundler/gems/kit-abc123def456");
        std::fs::create_dir_all(repo.join("store/ruby/3.4.0/gems")).unwrap();
        for dir in ["", "kit_core", "kit_web"] {
            std::fs::create_dir_all(checkout.join(dir)).unwrap();
            let name = if dir.is_empty() { "kit" } else { dir };
            std::fs::write(checkout.join(dir).join(format!("{name}.gemspec")), "").unwrap();
        }
        let git = |checkout: &str| Source::Git {
            checkout: checkout.into(),
        };
        let located = locate(
            &repo,
            vec![
                gem("kit", git("kit-abc123def456")),
                gem("kit_core", git("kit-abc123def456")),
                gem("kit_web", git("kit-abc123def456")),
                gem("kit_absent", git("kit-abc123def456")),
                gem("kit", git("kit-000000000000")),
            ],
            None,
        );
        let places: Vec<&Place> = located.iter().map(|l| &l.place).collect();
        assert_eq!(places[0], &Place::Dir(checkout.clone()));
        assert_eq!(places[1], &Place::Dir(checkout.join("kit_core")));
        assert_eq!(places[2], &Place::Dir(checkout.join("kit_web")));
        assert_eq!(places[3], &Place::Missing(Absence::NoGemspec(checkout)));
        assert!(
            matches!(places[4], Place::Missing(Absence::NoCheckout(path)) if path.ends_with("bundler/gems/kit-000000000000")),
            "another revision's checkout is not this one: {places:?}"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn a_lone_git_gem_owns_a_gemspec_named_otherwise() {
        let dir = scratch("git-sole");
        std::fs::write(dir.join("other.gemspec"), "").unwrap();
        assert_eq!(gemspec_dir(&dir, "widget", true), Some(dir.clone()));
        assert_eq!(gemspec_dir(&dir, "widget", false), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_default_gem_says_its_code_is_the_stdlib() {
        let prefix = scratch("default-gem");
        let base = prefix.join("lib/ruby/gems/3.4.0");
        let empty = base.join("gems/widget-2.9.1");
        let installed = base.join("gems/widget-2.10.0/lib");
        std::fs::create_dir_all(&empty).unwrap();
        std::fs::create_dir_all(&installed).unwrap();
        std::fs::create_dir_all(base.join("specifications/default")).unwrap();
        std::fs::write(base.join("specifications/default/widget-2.9.1.gemspec"), "").unwrap();
        let stdlib = std::fs::canonicalize(&prefix)
            .unwrap()
            .join("lib/ruby/3.4.0");
        assert_eq!(default_gem_stdlib(&empty), Some(stdlib));
        assert_eq!(default_gem_stdlib(installed.parent().unwrap()), None);
        let _ = std::fs::remove_dir_all(&prefix);
    }

    #[test]
    fn a_ruby_version_names_its_abi_directory() {
        assert_eq!(abi_of("3.4.9").as_deref(), Some("3.4.0"));
        assert_eq!(abi_of("3.4").as_deref(), Some("3.4.0"));
        assert_eq!(abi_of("jruby-9.4"), None);
        let repo = scratch("ruby-version");
        std::fs::write(repo.join("Gemfile"), "source 'x'\nruby \"3.3.1\"\n").unwrap();
        assert_eq!(project_ruby(&repo).as_deref(), Some("3.3.1"));
        std::fs::write(repo.join(".ruby-version"), "ruby-3.4.9\n").unwrap();
        assert_eq!(
            project_ruby(&repo).as_deref(),
            Some("3.4.9"),
            ".ruby-version first"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn reads_the_ruby_each_version_file_and_the_lockfile_name() {
        let repo = scratch("version-files");
        std::fs::write(
            repo.join(".tool-versions"),
            "nodejs 20.1.0\nruby 3.3.6 3.2.0\n",
        )
        .unwrap();
        assert_eq!(project_ruby(&repo).as_deref(), Some("3.3.6"));
        std::fs::write(repo.join(".tool-versions"), "ruby system\n").unwrap();
        assert_eq!(project_ruby(&repo), None, "system names no install");
        let mise = "[env]\nruby = \"x\"\n[tools]\nnode = \"20\"\nruby = [\"3.4\", \"3.3\"]\n";
        assert_eq!(mise_ruby(mise).as_deref(), Some("3.4"));
        assert_eq!(mise_ruby("[tools]\nruby-build = \"1\"\n"), None);
        std::fs::write(
            repo.join("Gemfile.lock"),
            "GEM\n  specs:\n\nRUBY VERSION\n   ruby 3.4.7p58\n\nBUNDLED WITH\n   2.6.9\n",
        )
        .unwrap();
        assert_eq!(lockfile_ruby(&repo).as_deref(), Some("3.4.7"));
        std::fs::write(
            repo.join("Gemfile.lock"),
            "RUBY VERSION\n   ruby 3.5.0p-1\n",
        )
        .unwrap();
        assert_eq!(lockfile_ruby(&repo).as_deref(), Some("3.5.0"), "a dev Ruby");
        std::fs::write(repo.join("Gemfile.lock"), "GEM\n  specs:\n").unwrap();
        assert_eq!(lockfile_ruby(&repo), None);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn a_checkouts_ruby_requirements_are_read_from_its_gemspecs_and_gemfile() {
        let repo = scratch("ruby-requirements");
        std::fs::write(
            repo.join("widget.gemspec"),
            "Gem::Specification.new do |s|\n  s.required_ruby_version = Gem::Requirement.new(\">= 3.1\")\nend\n",
        )
        .unwrap();
        std::fs::write(
            repo.join("Gemfile"),
            "source 'x'\nruby '>= 3.0', '< 4.1', engine: 'ruby'\ngem 'rake'\n",
        )
        .unwrap();
        let requirements = declared::ruby_requirements(&repo);
        assert_eq!(
            requirements,
            [
                ("Gemfile".to_string(), ">= 3.0".to_string()),
                ("Gemfile".to_string(), "< 4.1".to_string()),
                ("widget.gemspec".to_string(), ">= 3.1".to_string()),
            ]
        );
        assert!(declared::meets_all("3.4.9", &requirements));
        assert!(!declared::meets_all("3.0.7", &requirements));
        assert!(!declared::meets_all("4.1.0", &requirements));
        // A literal version is named, not required.
        std::fs::write(repo.join("Gemfile"), "ruby \"3.4.1\"\n").unwrap();
        std::fs::remove_file(repo.join("widget.gemspec")).unwrap();
        assert!(declared::ruby_requirements(&repo).is_empty());
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn reads_a_bundler_config_value_quoted_or_not() {
        let config = "---\nBUNDLE_JOBS: \"4\"\nBUNDLE_PATH: \"vendor/bundle\"\n";
        assert_eq!(
            config_value(config, "BUNDLE_PATH").as_deref(),
            Some("vendor/bundle")
        );
        assert_eq!(
            config_value("BUNDLE_PATH: x\n", "BUNDLE_PATH").as_deref(),
            Some("x")
        );
        assert_eq!(
            config_value("BUNDLE_PATH__SYSTEM: true\n", "BUNDLE_PATH"),
            None
        );
    }
}
