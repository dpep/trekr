//! A checkout's gems when it has no `Gemfile.lock` (DEC-134).
//!
//! Most gems commit no lockfile, so their own specs had no rspec-core and no
//! runtime dependency to answer from. What they *declare* is still readable:
//! the gemspecs' `add_dependency` and `add_development_dependency`, and the
//! Gemfile's `gem` lines. Each is resolved to the highest installed version
//! that meets its requirements, and an installed gem's own runtime
//! dependencies follow from its installed gemspec — the closure `bundle
//! install` would have locked, approximated by what is on disk. Read with
//! Prism, never run.

use super::{Gem, Located, Source};
use ruby_prism::{Node, Visit};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// A dependency as a gemspec or Gemfile writes it.
#[derive(Debug, PartialEq)]
struct Dependency {
    name: String,
    /// Each as written (`"~> 3.0"`); empty for any version. A requirement
    /// that interpolates is dropped, which widens it to any.
    requirements: Vec<String>,
    development: bool,
}

/// The gems a checkout declares, resolved against what is installed, or
/// `None` when it declares nothing — no gemspec and no Gemfile.
pub(super) fn resolve(repo: &Path, roots: &[PathBuf]) -> Option<Vec<Located>> {
    let mut own: HashSet<String> = HashSet::new();
    let mut declared: Vec<Dependency> = Vec::new();
    let mut any = false;
    let mut entries: Vec<PathBuf> = std::fs::read_dir(repo)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .collect();
    entries.sort();
    for path in entries {
        let is_gemspec = path.extension().is_some_and(|e| e == "gemspec");
        let is_gemfile = path.file_name().is_some_and(|n| n == "Gemfile");
        if !is_gemspec && !is_gemfile {
            continue;
        }
        let Ok(source) = std::fs::read(&path) else {
            continue;
        };
        any = true;
        if is_gemspec && let Some(stem) = path.file_stem() {
            own.insert(stem.to_string_lossy().into_owned());
        }
        declared.extend(dependencies(&source));
    }
    if !any {
        return None;
    }

    let installed = Installed::scan(roots);
    // Every requirement written for a name, merged, before any is resolved:
    // the gemspec's `< 2` and the Gemfile's `~> 1.0` both bind.
    let mut wanted: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for dependency in declared {
        wanted
            .entry(dependency.name)
            .or_default()
            .extend(dependency.requirements);
    }
    let mut queue: Vec<(String, Vec<String>)> = wanted.into_iter().collect();
    queue.reverse();
    let mut seen: HashSet<String> = HashSet::new();
    let mut located = Vec::new();
    while let Some((name, requirements)) = queue.pop() {
        if own.contains(&name) || !seen.insert(name.clone()) {
            continue;
        }
        let Some(found) = installed.best(&name, &requirements) else {
            let version = match requirements.is_empty() {
                true => "*".to_string(),
                false => requirements.join(", "),
            };
            located.push(Located {
                gem: Gem {
                    name,
                    version,
                    source: Source::Registry,
                },
                root: None,
            });
            continue;
        };
        for dependency in runtime_dependencies(&found.path, &name, &found.written) {
            queue.push((dependency.name, dependency.requirements));
        }
        located.push(Located {
            gem: Gem {
                name,
                version: found.written.clone(),
                source: Source::Registry,
            },
            root: Some(found.path.clone()),
        });
    }
    Some(located)
}

/// An installed gem's runtime dependencies, from the gemspec rubygems wrote
/// beside it, or failing that the one it shipped.
fn runtime_dependencies(dir: &Path, name: &str, version: &str) -> Vec<Dependency> {
    let installed = dir.parent().and_then(Path::parent).map(|base| {
        base.join("specifications")
            .join(format!("{name}-{version}.gemspec"))
    });
    let shipped = dir.join(format!("{name}.gemspec"));
    let source = installed
        .and_then(|path| std::fs::read(path).ok())
        .or_else(|| std::fs::read(shipped).ok())
        .unwrap_or_default();
    dependencies(&source)
        .into_iter()
        .filter(|dependency| !dependency.development)
        .collect()
}

/// The dependencies a gemspec or Gemfile writes: `add_dependency`,
/// `add_runtime_dependency`, `add_development_dependency`, and `gem`. A
/// `gem` from a path or git, or for another platform, is not an installed
/// release and is left out.
fn dependencies(source: &[u8]) -> Vec<Dependency> {
    let parsed = ruby_prism::parse(source);
    let mut found = Collector(Vec::new());
    found.visit(&parsed.node());
    found.0
}

struct Collector(Vec<Dependency>);

impl<'pr> Visit<'pr> for Collector {
    fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
        let method = String::from_utf8_lossy(node.name().as_slice()).into_owned();
        let development = match method.as_str() {
            "add_dependency" | "add_runtime_dependency" | "gem" => Some(false),
            "add_development_dependency" => Some(true),
            _ => None,
        };
        if let Some(development) = development
            && let Some(dependency) = dependency(node, development, method == "gem")
        {
            self.0.push(dependency);
        }
        ruby_prism::visit_call_node(self, node);
    }
}

fn dependency(
    call: &ruby_prism::CallNode<'_>,
    development: bool,
    gemfile: bool,
) -> Option<Dependency> {
    let args: Vec<Node<'_>> = call.arguments()?.arguments().iter().collect();
    let (first, rest) = args.split_first()?;
    let name = string(first)?;
    let mut requirements = Vec::new();
    for arg in rest {
        if let Some(options) = arg.as_keyword_hash_node() {
            let elsewhere = options.elements().iter().any(|element| {
                let key = element
                    .as_assoc_node()
                    .and_then(|assoc| symbol(&assoc.key()));
                matches!(
                    key.as_deref(),
                    Some("path" | "git" | "github" | "platforms" | "platform")
                )
            });
            if gemfile && elsewhere {
                return None;
            }
            continue;
        }
        requirements.extend(strings(arg));
    }
    Some(Dependency {
        name,
        requirements,
        development,
    })
}

/// `x.freeze` is `x`; `None` when it is neither.
fn frozen_receiver<'pr>(node: &Node<'pr>) -> Option<Node<'pr>> {
    let call = node.as_call_node()?;
    (call.name().as_slice() == b"freeze").then(|| call.receiver())?
}

fn string(node: &Node<'_>) -> Option<String> {
    if let Some(inner) = frozen_receiver(node) {
        return string(&inner);
    }
    let string = node.as_string_node()?;
    String::from_utf8(string.unescaped().to_vec()).ok()
}

fn strings(node: &Node<'_>) -> Vec<String> {
    if let Some(inner) = frozen_receiver(node) {
        return strings(&inner);
    }
    match node.as_array_node() {
        Some(array) => array.elements().iter().filter_map(|e| string(&e)).collect(),
        None => string(node).into_iter().collect(),
    }
}

fn symbol(node: &Node<'_>) -> Option<String> {
    let symbol = node.as_symbol_node()?;
    String::from_utf8(symbol.unescaped().to_vec()).ok()
}

/// A version as rubygems orders them: numeric segments by value, and a
/// segment with a letter — a prerelease — below any number.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Version(Vec<Segment>);

#[derive(Clone, Debug, PartialEq, Eq)]
enum Segment {
    Number(u64),
    Pre(String),
}

impl Version {
    fn parse(text: &str) -> Option<Version> {
        let text = text.trim();
        if !text.starts_with(|c: char| c.is_ascii_digit()) {
            return None;
        }
        let segments = text
            .split('.')
            .map(|segment| match segment.parse() {
                Ok(n) => Segment::Number(n),
                Err(_) => Segment::Pre(segment.to_string()),
            })
            .collect();
        Some(Version(segments))
    }

    fn is_prerelease(&self) -> bool {
        self.0.iter().any(|s| matches!(s, Segment::Pre(_)))
    }

    /// `~> 1.2.3` allows below `1.3`; `~> 1.2` below `2`.
    fn pessimistic_bound(&self) -> Version {
        let mut numbers: Vec<u64> = self
            .0
            .iter()
            .map_while(|s| match s {
                Segment::Number(n) => Some(*n),
                Segment::Pre(_) => None,
            })
            .collect();
        if numbers.len() > 1 {
            numbers.pop();
        }
        if let Some(last) = numbers.last_mut() {
            *last += 1;
        }
        Version(numbers.into_iter().map(Segment::Number).collect())
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Version) -> Ordering {
        let zero = Segment::Number(0);
        let len = self.0.len().max(other.0.len());
        for at in 0..len {
            let a = self.0.get(at).unwrap_or(&zero);
            let b = other.0.get(at).unwrap_or(&zero);
            let order = match (a, b) {
                (Segment::Number(a), Segment::Number(b)) => a.cmp(b),
                (Segment::Number(_), Segment::Pre(_)) => Ordering::Greater,
                (Segment::Pre(_), Segment::Number(_)) => Ordering::Less,
                (Segment::Pre(a), Segment::Pre(b)) => a.cmp(b),
            };
            if order != Ordering::Equal {
                return order;
            }
        }
        Ordering::Equal
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Version) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Does `version` meet one requirement as written? One that cannot be read
/// is met, which widens rather than guesses.
fn meets(version: &Version, requirement: &str) -> bool {
    let requirement = requirement.trim();
    let (op, rest) = ["~>", ">=", "<=", "!=", "=", ">", "<"]
        .iter()
        .find_map(|op| requirement.strip_prefix(op).map(|rest| (*op, rest)))
        .unwrap_or(("=", requirement));
    let Some(bound) = Version::parse(rest) else {
        return true;
    };
    match op {
        "~>" => *version >= bound && *version < bound.pessimistic_bound(),
        ">=" => *version >= bound,
        "<=" => *version <= bound,
        "!=" => *version != bound,
        ">" => *version > bound,
        "<" => *version < bound,
        _ => *version == bound,
    }
}

/// One installed copy of a gem.
struct Copy {
    version: Version,
    /// The version as its directory writes it, platform and all.
    written: String,
    path: PathBuf,
}

/// Every gem installed under the search roots, by name. The first root to
/// hold a version keeps it, as search-root order means.
struct Installed(HashMap<String, Vec<Copy>>);

impl Installed {
    fn scan(roots: &[PathBuf]) -> Installed {
        let mut by_name: HashMap<String, Vec<Copy>> = HashMap::new();
        for root in roots {
            let Ok(entries) = std::fs::read_dir(root) else {
                continue;
            };
            for entry in entries.flatten() {
                let dir = entry.file_name().to_string_lossy().into_owned();
                let Some((name, written)) = split_dir(&dir) else {
                    continue;
                };
                let number = written.split('-').next().unwrap_or(written);
                let Some(version) = Version::parse(number) else {
                    continue;
                };
                let copies = by_name.entry(name.to_string()).or_default();
                if copies.iter().any(|copy| copy.written == written) {
                    continue;
                }
                copies.push(Copy {
                    version,
                    written: written.to_string(),
                    path: entry.path(),
                });
            }
        }
        Installed(by_name)
    }

    /// The highest installed version meeting every requirement, a release
    /// before any prerelease.
    fn best(&self, name: &str, requirements: &[String]) -> Option<&Copy> {
        let meeting: Vec<&Copy> = self
            .0
            .get(name)?
            .iter()
            .filter(|copy| requirements.iter().all(|r| meets(&copy.version, r)))
            .collect();
        let release = meeting
            .iter()
            .filter(|copy| !copy.version.is_prerelease())
            .max_by(|a, b| a.version.cmp(&b.version));
        release
            .or_else(|| meeting.iter().max_by(|a, b| a.version.cmp(&b.version)))
            .copied()
    }
}

/// `rspec-core-3.13.0` → (`rspec-core`, `3.13.0`); a platform stays with the
/// version (`nokogiri-1.16.0-arm64-darwin`). The name ends at the first `-`
/// a digit follows.
fn split_dir(dir: &str) -> Option<(&str, &str)> {
    let at = dir
        .char_indices()
        .find(|(i, c)| *c == '-' && dir[i + 1..].starts_with(|d: char| d.is_ascii_digit()))?
        .0;
    Some((&dir[..at], &dir[at + 1..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn reads_what_a_gemspec_and_a_gemfile_declare() {
        let gemspec = br#"Gem::Specification.new do |s|
  s.add_runtime_dependency(%q<rspec-core>.freeze, ["~> 3.13.0".freeze])
  s.add_development_dependency "rake", ">= 1", "< 14"
end
"#;
        let found = dependencies(gemspec);
        assert_eq!(found[0].name, "rspec-core");
        assert_eq!(found[0].requirements, ["~> 3.13.0"]);
        assert!(found[1].development && found[1].requirements == [">= 1", "< 14"]);

        let gemfile = br#"gem 'rspec', '~> 3.0'
gem "sqlite3", "~> #{ENV['V'] || '1.4'}"
gem 'local', path: '../local'
gem 'jruby-openssl', platforms: :jruby
"#;
        let found = dependencies(gemfile);
        let names: Vec<&str> = found.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["rspec", "sqlite3"]);
        assert!(
            found[1].requirements.is_empty(),
            "an interpolated requirement is any"
        );
    }

    #[test]
    fn requirements_order_versions_as_rubygems_does() {
        assert!(meets(&version("3.13.2"), "~> 3.13.0"));
        assert!(!meets(&version("3.14.0"), "~> 3.13.0"));
        assert!(meets(&version("3.99"), "~> 3.0"));
        assert!(!meets(&version("4.0"), "~> 3.0"));
        assert!(meets(&version("1.9.9"), "< 2"));
        assert!(version("1.0.0.rc1") < version("1.0.0"));
        assert!(version("1.10") > version("1.9"));
        assert!(meets(&version("2.0"), "2.0"));
    }

    #[test]
    fn a_directory_splits_at_the_version() {
        assert_eq!(
            split_dir("rspec-core-3.13.0"),
            Some(("rspec-core", "3.13.0"))
        );
        assert_eq!(
            split_dir("nokogiri-1.16.0-arm64-darwin"),
            Some(("nokogiri", "1.16.0-arm64-darwin"))
        );
        assert_eq!(split_dir("bundler"), None);
    }

    /// The highest release that meets every requirement, with the gems it
    /// needs at run time, and the checkout's own gems left to the checkout.
    #[test]
    fn resolves_the_highest_installed_match_and_what_it_needs() {
        let base = std::env::temp_dir().join(format!("trekr-declared-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let gems = base.join("gems");
        for dir in [
            "alpha-1.2.0",
            "alpha-1.9.0",
            "alpha-2.0.0",
            "alpha-2.1.0.rc1",
            "beta-0.3.0",
        ] {
            std::fs::create_dir_all(gems.join(dir)).unwrap();
        }
        std::fs::create_dir_all(base.join("specifications")).unwrap();
        std::fs::write(
            base.join("specifications/alpha-1.9.0.gemspec"),
            "s.add_runtime_dependency(%q<beta>.freeze, [\">= 0\".freeze])\n\
             s.add_development_dependency(%q<gamma>.freeze, [\">= 0\".freeze])\n",
        )
        .unwrap();
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(
            repo.join("widget.gemspec"),
            "Gem::Specification.new do |s|\n  s.add_dependency 'alpha', '< 2'\nend\n",
        )
        .unwrap();
        std::fs::write(
            repo.join("Gemfile"),
            "gemspec\ngem 'widget'\ngem 'absent'\n",
        )
        .unwrap();

        let located = resolve(&repo, std::slice::from_ref(&gems)).unwrap();
        let got: Vec<(&str, &str, bool)> = located
            .iter()
            .map(|l| {
                (
                    l.gem.name.as_str(),
                    l.gem.version.as_str(),
                    l.root.is_some(),
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("absent", "*", false),
                ("alpha", "1.9.0", true),
                ("beta", "0.3.0", true)
            ]
        );
        assert!(resolve(&gems.join("beta-0.3.0"), std::slice::from_ref(&gems)).is_none());
        let _ = std::fs::remove_dir_all(&base);
    }
}
