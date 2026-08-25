//! Which gems does this checkout use, and where are they on disk?
//!
//! Without running Ruby. `Gemfile.lock` is a plain text file with a documented
//! shape, and every gem manager on earth unpacks into `.../gems/<name>-<version>/`,
//! so both halves are answerable by reading. Shelling out to `bundle` would
//! need the project's Ruby and its bundle to be installed and working — the
//! exact dependency PLAN §1 says is the product's first edge.
//!
//! A gem is keyed by its directory, which already encodes `(name, version)`, so
//! two projects on one machine that use `activesupport 7.1.0` share one index.
//! A gem's bytes never change, which makes this the best case the blob store
//! has.

use std::path::{Path, PathBuf};

/// Where a lockfile says a gem comes from. It decides where to look, and
/// whether to look at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Source {
    /// A packaged gem, unpacked into `.../gems/<name>-<version>/`.
    Registry,
    /// A git dependency, checked out under `bundler/gems/<name>-<sha>/`.
    Git,
    /// A path dependency — its code lives inside the checkout, so it is
    /// already indexed and must not be reported as a hole.
    Path,
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

/// A gem that was named and found, or named and not.
#[derive(Debug)]
pub(crate) struct Located {
    pub(crate) gem: Gem,
    /// `None` when nothing on disk matches. Reported, never silently dropped —
    /// a missing gem is a hole in every answer that would have come from it.
    pub(crate) root: Option<PathBuf>,
}

impl Located {
    /// Is this a hole in the index worth telling someone about?
    ///
    /// A path gem is not: its source is inside the checkout, which is already
    /// indexed. Counting it as missing would make a healthy monorepo look
    /// broken — rails' own lockfile names 12 of them.
    pub(crate) fn is_hole(&self) -> bool {
        self.root.is_none() && self.gem.source != Source::Path
    }
}

/// Parse the `specs:` blocks of a `Gemfile.lock`.
///
/// A gem is a line indented exactly four spaces reading `name (version)`; its
/// own dependencies are indented six and are already listed elsewhere, so they
/// are skipped rather than deduplicated later. `GIT`, `PATH`, and `GEM`
/// sections all use the same shape, which is why the section header does not
/// need parsing at all.
pub(crate) fn parse_lockfile(text: &str) -> Vec<Gem> {
    let mut gems = Vec::new();
    let mut in_specs = false;
    let mut source = Source::Registry;
    for line in text.lines() {
        let trimmed = line.trim_end();
        if trimmed.trim_start() == "specs:" {
            in_specs = true;
            continue;
        }
        // A non-indented, non-empty line ends the section and names the next.
        if !trimmed.is_empty() && !trimmed.starts_with(' ') {
            in_specs = false;
            source = match trimmed {
                "GIT" => Source::Git,
                "PATH" => Source::Path,
                _ => Source::Registry,
            };
            continue;
        }
        if !in_specs {
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
            source,
        });
    }
    gems.sort();
    gems.dedup();
    gems
}

/// Directory patterns gems are unpacked into, most specific first. A single
/// `*` in a component means "try every directory here".
fn search_roots(repo: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let mut push = |path: PathBuf| roots.push(path);

    // Vendored into the project wins: it is what the project actually resolves.
    push(repo.join("vendor/bundle/ruby/*/gems"));
    push(repo.join(".bundle/ruby/*/gems"));

    if let Ok(home) = std::env::var("GEM_HOME") {
        push(PathBuf::from(home).join("gems"));
    }
    if let Ok(paths) = std::env::var("GEM_PATH") {
        for entry in paths.split(':').filter(|p| !p.is_empty()) {
            push(PathBuf::from(entry).join("gems"));
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let home = PathBuf::from(home);
        push(home.join(".gem/ruby/*/gems"));
        push(home.join(".rbenv/versions/*/lib/ruby/gems/*/gems"));
        push(home.join(".rvm/gems/*/gems"));
        push(home.join(".asdf/installs/ruby/*/lib/ruby/gems/*/gems"));
    }
    push(PathBuf::from("/opt/homebrew/lib/ruby/gems/*/gems"));
    push(PathBuf::from("/usr/local/lib/ruby/gems/*/gems"));
    push(PathBuf::from("/usr/lib/ruby/gems/*/gems"));
    push(PathBuf::from("/Library/Ruby/Gems/*/gems"));
    roots
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
                if entry.file_type().is_ok_and(|t| t.is_dir()) {
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

/// Find each gem's unpacked source, in search-root order.
pub(crate) fn locate(repo: &Path, gems: Vec<Gem>) -> Vec<Located> {
    // Expand the globs once, not once per gem: there are ~100 gems and a
    // handful of roots.
    let roots: Vec<PathBuf> = search_roots(repo).iter().flat_map(|p| expand(p)).collect();
    // A git dependency is checked out under a sibling directory, named with
    // the revision rather than the version, so it needs a prefix search.
    let git_roots: Vec<PathBuf> = roots
        .iter()
        .filter_map(|root| root.parent().map(|p| p.join("bundler/gems")))
        .collect();

    gems.into_iter()
        .map(|gem| {
            let root = match gem.source {
                // Its code is in the checkout, which is indexed already.
                Source::Path => None,
                Source::Registry => {
                    let dir = gem.dir_name();
                    roots
                        .iter()
                        .map(|root| root.join(&dir))
                        .find(|candidate| candidate.is_dir())
                }
                Source::Git => git_roots
                    .iter()
                    .find_map(|root| find_git_checkout(root, &gem.name)),
            };
            Located { gem, root }
        })
        .collect()
}

/// `bundler/gems/<name>-<revision>` — the version is not in the name, so match
/// on the prefix and take the first.
fn find_git_checkout(root: &Path, name: &str) -> Option<PathBuf> {
    let prefix = format!("{name}-");
    std::fs::read_dir(root)
        .ok()?
        .flatten()
        .find(|entry| {
            entry.file_name().to_string_lossy().starts_with(&prefix)
                && entry.file_type().is_ok_and(|t| t.is_dir())
        })
        .map(|entry| entry.path())
}

/// The gems a checkout depends on, located on disk.
///
/// An absent `Gemfile.lock` is not an error: plenty of Ruby has no bundle.
pub(crate) fn for_checkout(repo: &Path) -> Vec<Located> {
    let Ok(text) = std::fs::read_to_string(repo.join("Gemfile.lock")) else {
        return Vec::new();
    };
    locate(repo, parse_lockfile(&text))
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
        let by = |name: &str| gems.iter().find(|g| g.name == name).unwrap().source;
        assert_eq!(by("widget"), Source::Git);
        assert_eq!(by("billing"), Source::Path);
        assert_eq!(by("activesupport"), Source::Registry);
    }

    #[test]
    fn a_path_gem_is_not_a_hole_because_its_code_is_in_the_checkout() {
        let temp = std::env::temp_dir().join(format!("trekr-path-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&temp);
        let located = locate(
            &temp,
            vec![
                Gem {
                    name: "billing".into(),
                    version: "1.0.0".into(),
                    source: Source::Path,
                },
                Gem {
                    name: "absent".into(),
                    version: "1.0.0".into(),
                    source: Source::Registry,
                },
            ],
        );
        assert!(!located[0].is_hole(), "a path gem is already indexed");
        assert!(located[1].is_hole());
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn a_lockfile_with_nothing_in_it_yields_nothing() {
        assert!(parse_lockfile("").is_empty());
        assert!(parse_lockfile("GEM\n  remote: x\n  specs:\n").is_empty());
    }

    #[test]
    fn a_gem_that_is_not_on_disk_is_reported_rather_than_dropped() {
        let temp = std::env::temp_dir().join(format!("trekr-gems-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&temp);
        let located = locate(
            &temp,
            vec![Gem {
                name: "definitely-not-installed".into(),
                version: "9.9.9".into(),
                source: Source::Registry,
            }],
        );
        assert_eq!(located.len(), 1);
        assert!(
            located[0].root.is_none(),
            "a hole in the index has to be visible"
        );
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn finds_a_gem_vendored_into_the_project() {
        let temp = std::env::temp_dir().join(format!("trekr-vendor-{}", std::process::id()));
        let gem_dir = temp.join("vendor/bundle/ruby/3.3.0/gems/widget-0.1.0");
        std::fs::create_dir_all(&gem_dir).unwrap();
        let located = locate(
            &temp,
            vec![Gem {
                name: "widget".into(),
                version: "0.1.0".into(),
                source: Source::Registry,
            }],
        );
        assert_eq!(located[0].root.as_deref(), Some(gem_dir.as_path()));
        let _ = std::fs::remove_dir_all(&temp);
    }
}
