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

use super::{Absence, Gem, Located, Place, Source};
use ruby_prism::{Node, Visit};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// A dependency as a gemspec or Gemfile writes it.
#[derive(Clone, Debug, PartialEq)]
struct Dependency {
    name: String,
    /// Each as written (`"~> 3.0"`), a literal constant's or local's value
    /// included; empty for any version.
    requirements: Vec<String>,
    /// The source of a requirement that is not a literal — `version`, an
    /// interpolation. It binds nothing, so the pick is the highest
    /// installed, and that is said rather than passed off as a reading.
    unread: Vec<String>,
    development: bool,
    /// Where each part of `requirements` was written, once merged: the file
    /// (`Gemfile`, `widget.gemspec`) and what it asks.
    from: Vec<(String, Vec<String>)>,
}

/// How many times the picks are revised as the picked gems' own
/// requirements join in. Two or three settle every checkout measured; the
/// cap only stops two picks that keep excluding each other.
const ROUNDS: usize = 8;

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
        let file = path.file_name().unwrap_or_default().to_string_lossy();
        declared.extend(dependencies(&source).into_iter().map(|mut dependency| {
            dependency.from = vec![(file.to_string(), dependency.requirements.clone())];
            dependency
        }));
    }
    if !any {
        return None;
    }

    let installed = Installed::scan(roots);
    // Every requirement written for a name, merged, before any is resolved:
    // the gemspec's `< 2` and the Gemfile's `~> 1.0` both bind.
    let mut direct: BTreeMap<String, Dependency> = BTreeMap::new();
    for dependency in declared {
        if own.contains(&dependency.name) {
            continue;
        }
        match direct.get_mut(&dependency.name) {
            Some(merged) => {
                merged.requirements.extend(dependency.requirements);
                merged.unread.extend(dependency.unread);
                merged.from.extend(dependency.from);
            }
            None => {
                direct.insert(dependency.name.clone(), dependency);
            }
        }
    }

    // Every requirement on a name binds at once — the checkout's own and each
    // picked gem's runtime dependencies — so a transitive `>= 0` cannot pick
    // past a direct `~> 5.25`. A pick can change what its dependents need,
    // so the picks are revised until they hold still.
    let needs = Needs::default();
    let mut picks: BTreeMap<String, Option<&Copy>> = BTreeMap::new();
    let mut wanted: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for _ in 0..ROUNDS {
        wanted = direct
            .iter()
            .map(|(name, d)| (name.clone(), d.requirements.clone()))
            .collect();
        for copy in picks.values().flatten() {
            for dependency in needs.of(copy).iter().filter(|d| !own.contains(&d.name)) {
                wanted
                    .entry(dependency.name.clone())
                    .or_default()
                    .extend(dependency.requirements.iter().cloned());
            }
        }
        // A pick whose own requirement on a name the checkout pins meets no
        // installed version would make that name a false "not installed":
        // rails 8.1 wanting activerecord 8.1 beside a gemspec's `< 8`.
        let fits = |copy: &Copy| {
            needs.of(copy).iter().all(|dependency| {
                let Some(pinned) = direct.get(&dependency.name) else {
                    return true;
                };
                let mut both = pinned.requirements.clone();
                both.extend(dependency.requirements.iter().cloned());
                installed.best(&dependency.name, &both, |_| true).is_some()
                    || installed
                        .best(&dependency.name, &pinned.requirements, |_| true)
                        .is_none()
            })
        };
        let next: BTreeMap<String, Option<&Copy>> = wanted
            .iter()
            .map(|(name, requirements)| (name.clone(), installed.best(name, requirements, fits)))
            .collect();
        if next.len() == picks.len()
            && next
                .iter()
                .zip(&picks)
                .all(|((a, x), (b, y))| a == b && x.map(|c| &c.path) == y.map(|c| &c.path))
        {
            break;
        }
        picks = next;
    }

    let picks_final: Vec<&Copy> = picks.values().flatten().copied().collect();
    let located = picks
        .into_iter()
        .map(|(name, copy)| {
            let unread = direct
                .get(&name)
                .map(|d| d.unread.clone())
                .unwrap_or_default();
            match copy {
                Some(copy) => Located {
                    gem: Gem {
                        name,
                        version: copy.written.clone(),
                        source: Source::Registry,
                    },
                    place: Place::Dir(copy.path.clone()),
                    unread,
                },
                None => {
                    let requirements = wanted.remove(&name).unwrap_or_default();
                    let (version, absence) =
                        match conflict(&name, &direct, &picks_final, &needs, &installed) {
                            Some(said) => (String::new(), Absence::Conflict(said)),
                            None if requirements.is_empty() => {
                                ("*".to_string(), Absence::NotInstalled)
                            }
                            None => (requirements.join(", "), Absence::NotInstalled),
                        };
                    Located {
                        gem: Gem {
                            name,
                            version,
                            source: Source::Registry,
                        },
                        place: Place::Missing(absence),
                        unread,
                    }
                }
            }
        })
        .collect();
    Some(located)
}

/// Why nothing installed meets a name's requirements, when they come from
/// more than one place and some installed copy meets one place's: the places
/// conflict, which "not installed" would misstate — `rubocop ~> 0.90.0, >=
/// 1.89.0, < 2.0` reads as a version to install that cannot exist.
fn conflict(
    name: &str,
    direct: &BTreeMap<String, Dependency>,
    picks: &[&Copy],
    needs: &Needs,
    installed: &Installed,
) -> Option<String> {
    let mut places: Vec<(String, Vec<String>)> =
        direct.get(name).map(|d| d.from.clone()).unwrap_or_default();
    for copy in picks {
        for dependency in needs.of(copy).iter().filter(|d| d.name == name) {
            places.push((
                format!("{} {}", copy.name, copy.written),
                dependency.requirements.clone(),
            ));
        }
    }
    places.retain(|(_, requirements)| !requirements.is_empty());
    let met = places
        .iter()
        .any(|(_, requirements)| installed.best(name, requirements, |_| true).is_some());
    (places.len() > 1 && met).then(|| {
        let said: Vec<String> = places
            .iter()
            .map(|(place, requirements)| format!("{} ({place})", requirements.join(", ")))
            .collect();
        format!(
            "requirements that conflict, which no installed version meets together: {}",
            said.join("; ")
        )
    })
}

/// Each installed copy's runtime dependencies, read once however often a
/// round asks.
#[derive(Default)]
struct Needs(std::cell::RefCell<HashMap<PathBuf, std::rc::Rc<Vec<Dependency>>>>);

impl Needs {
    fn of(&self, copy: &Copy) -> std::rc::Rc<Vec<Dependency>> {
        self.0
            .borrow_mut()
            .entry(copy.path.clone())
            .or_insert_with(|| runtime_dependencies(&copy.path, &copy.name, &copy.written).into())
            .clone()
    }
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
    let mut found = Collector::default();
    found.visit(&parsed.node());
    found.found
}

/// Reads dependencies in source order, with what the file binds by then: a
/// constant or local holding a literal, and a block parameter over a
/// literal list.
#[derive(Default)]
struct Collector {
    found: Vec<Dependency>,
    bound: HashMap<String, Vec<String>>,
}

impl Collector {
    /// A literal's strings, or what a name bound to one holds; `None` for
    /// anything else.
    fn values(&self, node: &Node<'_>) -> Option<Vec<String>> {
        if let Some(inner) = frozen_receiver(node) {
            return self.values(&inner);
        }
        if let Some(array) = node.as_array_node() {
            let mut out = Vec::new();
            for element in array.elements().iter() {
                out.extend(self.values(&element)?);
            }
            return Some(out);
        }
        if let Some(default) = env_default(node) {
            return Some(vec![default]);
        }
        if let Some(interpolated) = node.as_interpolated_string_node() {
            let mut out = String::new();
            for part in interpolated.parts().iter() {
                out.push_str(&match part.as_embedded_statements_node() {
                    Some(embedded) => {
                        let body: Vec<Node<'_>> = embedded.statements()?.body().iter().collect();
                        let [only] = body.try_into().ok()?;
                        let [value] = self.values(&only)?.try_into().ok()?;
                        value
                    }
                    None => string(&part)?,
                });
            }
            return Some(vec![out]);
        }
        let bound = |name: &[u8]| self.bound.get(&*String::from_utf8_lossy(name)).cloned();
        if let Some(constant) = node.as_constant_read_node() {
            return bound(constant.name().as_slice());
        }
        if let Some(local) = node.as_local_variable_read_node() {
            return bound(local.name().as_slice());
        }
        string(node).map(|s| vec![s])
    }

    fn bind(&mut self, name: &[u8], value: &Node<'_>) {
        let name = String::from_utf8_lossy(name).into_owned();
        match self.values(value) {
            Some(values) => self.bound.insert(name, values),
            None => self.bound.remove(&name),
        };
    }

    fn dependency(&self, call: &ruby_prism::CallNode<'_>, development: bool) -> Option<Dependency> {
        let gemfile = call.name().as_slice() == b"gem";
        let args: Vec<Node<'_>> = call.arguments()?.arguments().iter().collect();
        let (first, rest) = args.split_first()?;
        let [name] = self.values(first)?.try_into().ok()?;
        let mut requirements = Vec::new();
        let mut unread = Vec::new();
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
            match self.values(arg) {
                Some(values) => requirements.extend(values),
                None => {
                    unread.push(String::from_utf8_lossy(arg.location().as_slice()).into_owned())
                }
            }
        }
        Some(Dependency {
            name,
            requirements,
            unread,
            development,
            from: Vec::new(),
        })
    }

    /// What one branch of a conditional declares, apart from the rest.
    fn branch(&mut self, node: Option<Node<'_>>) -> Vec<Dependency> {
        let outer = std::mem::take(&mut self.found);
        if let Some(node) = node {
            self.visit(&node);
        }
        std::mem::replace(&mut self.found, outer)
    }

    /// Two branches that may each declare a gem are alternatives, not one
    /// requirement: `if ENV[…]` / `else` pins `~> 1.19` or `~> 1.16.0`,
    /// never both. A name both declare takes the branch that runs when
    /// nothing is set — `else`, or an `unless` body.
    fn alternatives(&mut self, taken: Vec<Dependency>, other: Vec<Dependency>) {
        let names: HashSet<String> = taken.iter().map(|d| d.name.clone()).collect();
        self.found.extend(taken);
        self.found
            .extend(other.into_iter().filter(|d| !names.contains(&d.name)));
    }
}

impl<'pr> Visit<'pr> for Collector {
    fn visit_constant_write_node(&mut self, node: &ruby_prism::ConstantWriteNode<'pr>) {
        self.bind(node.name().as_slice(), &node.value());
        ruby_prism::visit_constant_write_node(self, node);
    }

    fn visit_local_variable_write_node(&mut self, node: &ruby_prism::LocalVariableWriteNode<'pr>) {
        self.bind(node.name().as_slice(), &node.value());
        ruby_prism::visit_local_variable_write_node(self, node);
    }

    fn visit_if_node(&mut self, node: &ruby_prism::IfNode<'pr>) {
        self.visit(&node.predicate());
        let then = self.branch(node.statements().map(|s| s.as_node()));
        match node.subsequent() {
            Some(otherwise) => {
                let otherwise = self.branch(Some(otherwise));
                self.alternatives(otherwise, then);
            }
            None => self.found.extend(then),
        }
    }

    fn visit_unless_node(&mut self, node: &ruby_prism::UnlessNode<'pr>) {
        self.visit(&node.predicate());
        let then = self.branch(node.statements().map(|s| s.as_node()));
        let otherwise = self.branch(node.else_clause().map(|e| e.as_node()));
        self.alternatives(then, otherwise);
    }

    fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
        // `%w[a b].each { |g| s.add_dependency g }`: the body once per name.
        if let Some((param, items, body)) = self.each_over_literals(node) {
            for item in items {
                self.bound.insert(param.clone(), vec![item]);
                if let Some(body) = &body {
                    self.visit(body);
                }
            }
            self.bound.remove(&param);
            return;
        }
        let development = match node.name().as_slice() {
            b"add_dependency" | b"add_runtime_dependency" | b"gem" => Some(false),
            b"add_development_dependency" => Some(true),
            _ => None,
        };
        if let Some(development) = development
            && let Some(dependency) = self.dependency(node, development)
        {
            self.found.push(dependency);
        }
        ruby_prism::visit_call_node(self, node);
    }
}

impl Collector {
    /// `<literal list>.each { |one| body }`: the parameter, the names, the body.
    fn each_over_literals<'pr>(
        &self,
        call: &ruby_prism::CallNode<'pr>,
    ) -> Option<(String, Vec<String>, Option<Node<'pr>>)> {
        if call.name().as_slice() != b"each" {
            return None;
        }
        let receiver = call.receiver()?;
        receiver.as_array_node()?;
        let items = self.values(&receiver)?;
        let block = call.block()?.as_block_node()?;
        let params = block
            .parameters()?
            .as_block_parameters_node()?
            .parameters()?;
        let [param] = params
            .requireds()
            .iter()
            .collect::<Vec<_>>()
            .try_into()
            .ok()?;
        let param = param.as_required_parameter_node()?;
        let name = String::from_utf8_lossy(param.name().as_slice()).into_owned();
        Some((name, items, block.body()))
    }
}

/// What `ENV["X"] || "1.4"` or `ENV.fetch("X", "1.4")` is with nothing set:
/// the default, as a conditional's `else` is (DEC-151).
fn env_default(node: &Node<'_>) -> Option<String> {
    let is_env = |receiver: Option<Node<'_>>| {
        receiver
            .and_then(|r| r.as_constant_read_node())
            .is_some_and(|c| c.name().as_slice() == b"ENV")
    };
    if let Some(or) = node.as_or_node() {
        let lookup = or.left().as_call_node()?;
        return (lookup.name().as_slice() == b"[]" && is_env(lookup.receiver()))
            .then(|| string(&or.right()))?;
    }
    let call = node.as_call_node()?;
    if call.name().as_slice() != b"fetch" || !is_env(call.receiver()) {
        return None;
    }
    let args: Vec<Node<'_>> = call.arguments()?.arguments().iter().collect();
    match args.as_slice() {
        [_, default] => string(default),
        _ => None,
    }
}

/// `x.freeze` is `x`; `None` when it is neither.
fn frozen_receiver<'pr>(node: &Node<'pr>) -> Option<Node<'pr>> {
    let call = node.as_call_node()?;
    (call.name().as_slice() == b"freeze").then(|| call.receiver())?
}

pub(super) fn string(node: &Node<'_>) -> Option<String> {
    if let Some(inner) = frozen_receiver(node) {
        return string(&inner);
    }
    let string = node.as_string_node()?;
    String::from_utf8(string.unescaped().to_vec()).ok()
}

fn symbol(node: &Node<'_>) -> Option<String> {
    let symbol = node.as_symbol_node()?;
    String::from_utf8(symbol.unescaped().to_vec()).ok()
}

/// A version as rubygems orders them: numeric segments by value, and a
/// segment with a letter — a prerelease — below any number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Version(Vec<Segment>);

#[derive(Clone, Debug, PartialEq, Eq)]
enum Segment {
    Number(u64),
    Pre(String),
}

impl Version {
    pub(super) fn parse(text: &str) -> Option<Version> {
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

    /// The first `depth` segments: `3.4.9` to two is `3.4`.
    pub(super) fn truncated(&self, depth: usize) -> Version {
        Version(self.0.iter().take(depth).cloned().collect())
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
    name: String,
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
                    name: name.to_string(),
                    version,
                    written: written.to_string(),
                    path: entry.path(),
                });
            }
        }
        Installed(by_name)
    }

    /// The highest installed version meeting every requirement, a release
    /// before any prerelease, and among those one whose own runtime
    /// requirements `fits` accepts.
    fn best(
        &self,
        name: &str,
        requirements: &[String],
        fits: impl Fn(&Copy) -> bool,
    ) -> Option<&Copy> {
        let meeting: Vec<&Copy> = self
            .0
            .get(name)?
            .iter()
            .filter(|copy| requirements.iter().all(|r| meets(&copy.version, r)))
            .collect();
        // Look one step ahead, but never at the cost of the name itself.
        let fitting: Vec<&Copy> = meeting.iter().copied().filter(|c| fits(c)).collect();
        let meeting = if fitting.is_empty() { meeting } else { fitting };
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
pub(super) fn split_dir(dir: &str) -> Option<(&str, &str)> {
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
        assert_eq!(
            found[1].requirements,
            ["~> 1.4"],
            "an environment lookup reads as its default"
        );
    }

    #[test]
    fn a_requirement_held_in_a_constant_or_a_loop_is_read() {
        let gemspec = br#"version = File.read("VERSION").strip
REQ = "~> 2.9.0".freeze
Gem::Specification.new do |s|
  s.add_dependency "alpha", REQ
  s.add_dependency "beta", version
  %w[gamma delta].each { |g| s.add_dependency g, "< 3" }
end
"#;
        let found = dependencies(gemspec);
        let by = |name: &str| found.iter().find(|d| d.name == name).unwrap();
        assert_eq!(by("alpha").requirements, ["~> 2.9.0"]);
        assert_eq!(by("beta").unread, ["version"], "{found:?}");
        assert_eq!(by("delta").requirements, ["< 3"]);
    }

    #[test]
    fn a_gem_in_both_branches_of_a_conditional_takes_the_default_one() {
        let gemfile = br#"if ENV["MODERN"]
  gem "alpha", "~> 1.19"
  gem "beta"
else
  gem "alpha", "~> 1.16.0"
end
unless ENV["CI"]
  gem "gamma", "~> 2.0"
else
  gem "gamma", "~> 3.0"
end
"#;
        let found = dependencies(gemfile);
        let by = |name: &str| found.iter().find(|d| d.name == name).unwrap();
        assert_eq!(by("alpha").requirements, ["~> 1.16.0"], "{found:?}");
        assert_eq!(by("gamma").requirements, ["~> 2.0"], "{found:?}");
        assert!(
            found.iter().any(|d| d.name == "beta"),
            "only one branch names it"
        );
        assert_eq!(found.len(), 3, "{found:?}");
    }

    #[test]
    fn an_environment_lookup_reads_as_its_default() {
        let gemfile = br#"gem "alpha", "~> #{ENV['ALPHA_VERSION'] || '7.1'}"
gem "beta", ENV.fetch("BETA_VERSION", "~> 13.3.0")
gem "gamma", "~> #{ENV['GAMMA']}"
"#;
        let found = dependencies(gemfile);
        let by = |name: &str| found.iter().find(|d| d.name == name).unwrap();
        assert_eq!(by("alpha").requirements, ["~> 7.1"]);
        assert_eq!(by("beta").requirements, ["~> 13.3.0"]);
        assert_eq!(by("gamma").unread.len(), 1, "no default to read");
    }

    /// A pick whose own requirement leaves a name the checkout pins with no
    /// installed version gives way to one that does not.
    #[test]
    fn a_pick_gives_way_to_what_the_checkout_pins() {
        let base =
            std::env::temp_dir().join(format!("trekr-declared-ahead-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let gems = base.join("gems");
        for dir in ["frame-7.2.0", "frame-8.1.0", "model-7.2.0", "model-8.1.0"] {
            std::fs::create_dir_all(gems.join(dir)).unwrap();
        }
        std::fs::create_dir_all(base.join("specifications")).unwrap();
        for version in ["7.2.0", "8.1.0"] {
            std::fs::write(
                base.join(format!("specifications/frame-{version}.gemspec")),
                format!("s.add_runtime_dependency(%q<model>.freeze, [\"= {version}\".freeze])\n"),
            )
            .unwrap();
        }
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(
            repo.join("widget.gemspec"),
            "Gem::Specification.new do |s|\n  s.add_dependency 'model', '< 8'\nend\n",
        )
        .unwrap();
        std::fs::write(repo.join("Gemfile"), "gemspec\ngem 'frame'\n").unwrap();

        let located = resolve(&repo, std::slice::from_ref(&gems)).unwrap();
        let versions: Vec<(&str, &str)> = located
            .iter()
            .map(|l| (l.gem.name.as_str(), l.gem.version.as_str()))
            .collect();
        assert_eq!(versions, [("frame", "7.2.0"), ("model", "7.2.0")]);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Requirements from two places that no installed version meets together
    /// are said to conflict, each with its place; one place's alone is still
    /// "not installed".
    #[test]
    fn requirements_no_installed_version_meets_together_name_their_places() {
        let base =
            std::env::temp_dir().join(format!("trekr-declared-conflict-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let gems = base.join("gems");
        for dir in ["linter-1.89.1", "linter-ext-2.30.0"] {
            std::fs::create_dir_all(gems.join(dir)).unwrap();
        }
        std::fs::create_dir_all(base.join("specifications")).unwrap();
        std::fs::write(
            base.join("specifications/linter-ext-2.30.0.gemspec"),
            "s.add_runtime_dependency(%q<linter>.freeze, [\">= 1.89.0\".freeze, \"< 2.0\".freeze])\n",
        )
        .unwrap();
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(
            repo.join("Gemfile"),
            "gem 'linter', '~> 0.90.0'\ngem 'linter-ext'\ngem 'absent', '~> 1.0'\n",
        )
        .unwrap();

        let located = resolve(&repo, std::slice::from_ref(&gems)).unwrap();
        let place = |name: &str| &located.iter().find(|l| l.gem.name == name).unwrap().place;
        let Place::Missing(Absence::Conflict(said)) = place("linter") else {
            panic!("{:?}", place("linter"));
        };
        assert!(
            said.contains("~> 0.90.0 (Gemfile)")
                && said.contains(">= 1.89.0, < 2.0 (linter-ext 2.30.0)"),
            "{said}"
        );
        assert_eq!(place("absent"), &Place::Missing(Absence::NotInstalled));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// An installed gem's `>= 0` on a name the checkout pins itself must not
    /// pick past the pin, whichever is declared first.
    #[test]
    fn every_requirement_on_a_gem_binds_its_pick_direct_and_transitive() {
        for order in [["alpha", "beta"], ["beta", "alpha"]] {
            let base = std::env::temp_dir().join(format!(
                "trekr-declared-both-{}-{}",
                order[0],
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&base);
            let gems = base.join("gems");
            for dir in ["alpha-1.0.0", "beta-5.26.0", "beta-6.0.0"] {
                std::fs::create_dir_all(gems.join(dir)).unwrap();
            }
            std::fs::create_dir_all(base.join("specifications")).unwrap();
            std::fs::write(
                base.join("specifications/alpha-1.0.0.gemspec"),
                "s.add_runtime_dependency(%q<beta>.freeze, [\">= 5.1\".freeze])\n",
            )
            .unwrap();
            let repo = base.join("repo");
            std::fs::create_dir_all(&repo).unwrap();
            let requirement = |name: &str| match name {
                "beta" => "  s.add_dependency 'beta', '~> 5.25'\n",
                _ => "  s.add_dependency 'alpha'\n",
            };
            std::fs::write(
                repo.join("widget.gemspec"),
                format!(
                    "Gem::Specification.new do |s|\n{}{}end\n",
                    requirement(order[0]),
                    requirement(order[1])
                ),
            )
            .unwrap();

            let located = resolve(&repo, std::slice::from_ref(&gems)).unwrap();
            let beta = located.iter().find(|l| l.gem.name == "beta").unwrap();
            assert_eq!(beta.gem.version, "5.26.0", "declared {order:?}");
            let _ = std::fs::remove_dir_all(&base);
        }
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
                    matches!(l.place, Place::Dir(_)),
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
