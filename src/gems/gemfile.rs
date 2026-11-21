//! What a Gemfile or gemspec declares, line by line, and what a lockfile
//! says depends on what: the half of `--dead Gemfile` (DEC-670) that reads
//! no index. Read with Prism, never run.
//!
//! `declared.rs` reads the same files to *resolve* versions and drops what
//! it cannot install (`path:`, `platforms:`); this keeps every line, with
//! where it is and the options that decide how Bundler loads the gem.

use ruby_prism::{Node, Visit};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// How Bundler loads a gem at `Bundler.require`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Require {
    /// No `require:` option: its name is required.
    Auto,
    /// `require: false`: nothing is, until the app requires it.
    Off,
    /// `require: "x"` or a list: these paths instead of its name.
    Paths(Vec<String>),
}

/// One `gem` line, or one gemspec `add_*dependency`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Declaration {
    pub(crate) name: String,
    pub(crate) line: u32,
    pub(crate) col: u32,
    /// Bundler groups, as written (`development`, `test`); empty for the
    /// default group. A gemspec's development dependency is `development`.
    pub(crate) groups: Vec<String>,
    pub(crate) require: Require,
    /// `platforms:` or a `platforms` block; empty for every platform.
    pub(crate) platforms: Vec<String>,
    /// `path:`, `git:` or `github:`: not a release from a gem server.
    pub(crate) local: bool,
    /// `path:`'s directory, as written.
    pub(crate) path: Option<String>,
}

/// What a Gemfile or gemspec declares, in source order. Each `gemspec` (a
/// Gemfile pulling in a gemspec's dependencies) is the directory it reads,
/// `.` unless `path:` names another.
#[derive(Debug, Default)]
pub(crate) struct Declared {
    pub(crate) gems: Vec<Declaration>,
    pub(crate) gemspecs: Vec<String>,
}

pub(crate) fn declarations(source: &[u8]) -> Declared {
    let parsed = ruby_prism::parse(source);
    let mut reader = Reader {
        source,
        groups: Vec::new(),
        platforms: Vec::new(),
        found: Declared::default(),
    };
    reader.visit(&parsed.node());
    reader.found
}

struct Reader<'s> {
    source: &'s [u8],
    /// The `group` blocks the reader is inside, outermost first.
    groups: Vec<Vec<String>>,
    platforms: Vec<Vec<String>>,
    found: Declared,
}

impl Reader<'_> {
    fn place(&self, offset: usize) -> (u32, u32) {
        let before = &self.source[..offset.min(self.source.len())];
        let line = before.iter().filter(|b| **b == b'\n').count() as u32 + 1;
        let col = before.iter().rev().take_while(|b| **b != b'\n').count() as u32 + 1;
        (line, col)
    }

    fn declaration(
        &self,
        call: &ruby_prism::CallNode<'_>,
        development: bool,
    ) -> Option<Declaration> {
        let gemfile = call.name().as_slice() == b"gem";
        let args: Vec<Node<'_>> = call.arguments()?.arguments().iter().collect();
        let name = string(args.first()?)?;
        let mut groups: Vec<String> = self.groups.iter().flatten().cloned().collect();
        let mut platforms: Vec<String> = self.platforms.iter().flatten().cloned().collect();
        if development {
            groups.push("development".to_string());
        }
        let mut require = Require::Auto;
        let mut local = false;
        let mut path = None;
        for arg in &args[1..] {
            let Some(options) = arg.as_keyword_hash_node() else {
                continue;
            };
            if !gemfile {
                continue;
            }
            for assoc in options.elements().iter().filter_map(|e| e.as_assoc_node()) {
                let value = assoc.value();
                match symbol(&assoc.key()).as_deref() {
                    Some("group" | "groups") => groups.extend(names(&value)),
                    Some("platform" | "platforms") => platforms.extend(names(&value)),
                    Some("path") => {
                        local = true;
                        path = string(&value);
                    }
                    Some("git" | "github") => local = true,
                    Some("require") => {
                        require = if value.as_false_node().is_some() {
                            Require::Off
                        } else {
                            Require::Paths(names(&value))
                        }
                    }
                    _ => {}
                }
            }
        }
        let (line, col) = self.place(call.location().start_offset());
        groups.sort();
        groups.dedup();
        Some(Declaration {
            name,
            line,
            col,
            groups,
            require,
            platforms,
            local,
            path,
        })
    }
}

impl<'pr> Visit<'pr> for Reader<'_> {
    fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
        let name = node.name();
        let scoped = match name.as_slice() {
            b"group" => Some(true),
            b"platforms" | b"platform" => Some(false),
            _ => None,
        };
        if let Some(group) = scoped
            && node.receiver().is_none()
            && let Some(block) = node.block().and_then(|b| b.as_block_node())
        {
            let named: Vec<String> = node
                .arguments()
                .map(|args| args.arguments().iter().flat_map(|a| names(&a)).collect())
                .unwrap_or_default();
            match group {
                true => self.groups.push(named),
                false => self.platforms.push(named),
            }
            if let Some(body) = block.body() {
                self.visit(&body);
            }
            match group {
                true => self.groups.pop(),
                false => self.platforms.pop(),
            };
            return;
        }
        let development = match name.as_slice() {
            b"gem" if node.receiver().is_none() => Some(false),
            b"add_dependency" | b"add_runtime_dependency" => Some(false),
            b"add_development_dependency" => Some(true),
            b"gemspec" if node.receiver().is_none() => {
                let path = node
                    .arguments()
                    .into_iter()
                    .flat_map(|args| args.arguments().iter())
                    .filter_map(|arg| arg.as_keyword_hash_node())
                    .flat_map(|options| options.elements().iter())
                    .filter_map(|e| e.as_assoc_node())
                    .find(|assoc| symbol(&assoc.key()).as_deref() == Some("path"))
                    .and_then(|assoc| string(&assoc.value()));
                self.found
                    .gemspecs
                    .push(path.unwrap_or_else(|| ".".to_string()));
                None
            }
            _ => None,
        };
        if let Some(development) = development
            && let Some(declaration) = self.declaration(node, development)
        {
            self.found.gems.push(declaration);
        }
        ruby_prism::visit_call_node(self, node);
    }
}

fn string(node: &Node<'_>) -> Option<String> {
    if let Some(call) = node.as_call_node()
        && call.name().as_slice() == b"freeze"
    {
        return string(&call.receiver()?);
    }
    let string = node.as_string_node()?;
    String::from_utf8(string.unescaped().to_vec()).ok()
}

fn symbol(node: &Node<'_>) -> Option<String> {
    let symbol = node.as_symbol_node()?;
    String::from_utf8(symbol.unescaped().to_vec()).ok()
}

/// The names a symbol, string, or a list of them writes.
fn names(node: &Node<'_>) -> Vec<String> {
    if let Some(array) = node.as_array_node() {
        return array.elements().iter().flat_map(|e| names(&e)).collect();
    }
    symbol(node).or_else(|| string(node)).into_iter().collect()
}

/// What a lockfile says: each gem's own dependencies, by name, and the
/// gems the Gemfile asked for (`DEPENDENCIES`).
#[derive(Debug, Default)]
pub(crate) struct Lock {
    pub(crate) needs: HashMap<String, BTreeSet<String>>,
    pub(crate) direct: BTreeSet<String>,
    /// Specs from a `GIT` or `PATH` source: no release on a gem server.
    pub(crate) local: BTreeSet<String>,
}

impl Lock {
    /// A spec is indented four spaces, `name (version)`; its dependencies
    /// six. `DEPENDENCIES` lists the asked-for gems at two, `name!` for one
    /// from a git or path source.
    pub(crate) fn parse(text: &str) -> Lock {
        let mut lock = Lock::default();
        let mut section = "";
        let mut spec: Option<String> = None;
        for line in text.lines() {
            let trimmed = line.trim_end();
            if trimmed.is_empty() {
                continue;
            }
            if !trimmed.starts_with(' ') {
                section = trimmed;
                spec = None;
                continue;
            }
            let indent = trimmed.len() - trimmed.trim_start().len();
            let name = trimmed
                .trim_start()
                .split([' ', '!'])
                .next()
                .unwrap_or_default()
                .to_string();
            if name.is_empty() || name.ends_with(':') {
                continue;
            }
            match (section, indent) {
                ("DEPENDENCIES", 2) => {
                    lock.direct.insert(name);
                }
                ("GEM" | "GIT" | "PATH", 4) => {
                    lock.needs.entry(name.clone()).or_default();
                    if section != "GEM" {
                        lock.local.insert(name.clone());
                    }
                    spec = Some(name);
                }
                ("GEM" | "GIT" | "PATH", 6) => {
                    if let Some(spec) = &spec {
                        lock.needs.entry(spec.clone()).or_default().insert(name);
                    }
                }
                _ => {}
            }
        }
        lock
    }

    /// With no lockfile, what each installed gem's specification says it
    /// needs: a gem without a lock still brings its dependencies.
    pub(crate) fn from_installed(installed: &HashMap<String, String>) -> Lock {
        let mut lock = Lock::default();
        for (name, dir) in installed {
            let dir = std::path::Path::new(dir);
            let version = dir
                .file_name()
                .and_then(|base| base.to_str()?.strip_prefix(&format!("{name}-")))
                .unwrap_or_default();
            let needs = super::declared::runtime_needs(dir, name, version);
            lock.needs.insert(name.clone(), needs.into_iter().collect());
        }
        lock
    }

    /// Every gem `name` brings into the bundle, itself excluded.
    pub(crate) fn closure(&self, name: &str) -> BTreeSet<String> {
        let mut seen = BTreeSet::new();
        let mut todo = vec![name.to_string()];
        while let Some(next) = todo.pop() {
            for dep in self.needs.get(&next).into_iter().flatten() {
                if dep != name && seen.insert(dep.clone()) {
                    todo.push(dep.clone());
                }
            }
        }
        seen
    }

    /// The gems that need `name`, directly.
    pub(crate) fn needed_by(&self, name: &str) -> Vec<String> {
        let by: BTreeMap<&String, ()> = self
            .needs
            .iter()
            .filter(|(_, deps)| deps.contains(name))
            .map(|(spec, _)| (spec, ()))
            .collect();
        by.into_keys().cloned().collect()
    }
}

/// An installed gem's specification, as rubygems writes it: the commands
/// it installs (`executables`) and whether it compiles (`extensions`). A
/// list that is not literal is `None`, for the caller to look on disk; one
/// not written is empty, as rubygems defaults it (mini_mime ships a
/// `bin/db_pull_request` it does not install).
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Spec {
    pub(crate) executables: Option<Vec<String>>,
    pub(crate) extensions: bool,
}

pub(crate) fn spec(source: &[u8]) -> Spec {
    struct Finder(Spec);
    impl<'pr> Visit<'pr> for Finder {
        fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
            let value = node
                .arguments()
                .and_then(|args| args.arguments().iter().next());
            match (node.name().as_slice(), value) {
                (b"executables=", Some(value)) => {
                    let literal = value.as_array_node().is_some()
                        && value
                            .as_array_node()
                            .is_some_and(|a| a.elements().iter().all(|e| string(&e).is_some()));
                    self.0.executables = literal.then(|| names(&value));
                }
                (b"extensions=", Some(value)) => {
                    self.0.extensions = value
                        .as_array_node()
                        .is_none_or(|a| a.elements().iter().next().is_some());
                }
                _ => {}
            }
            ruby_prism::visit_call_node(self, node);
        }
    }
    let parsed = ruby_prism::parse(source);
    let mut finder = Finder(Spec {
        executables: Some(Vec::new()),
        extensions: false,
    });
    finder.visit(&parsed.node());
    finder.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gem_line_keeps_its_place_its_groups_and_how_it_loads() {
        let found = declarations(
            br#"source "https://rubygems.org"
gem "rails", "~> 7.1"
gem "pg", require: false
group :development, :test do
  gem "rspec-rails"
  platforms :mri do
    gem "debug", require: "debug/prelude"
  end
end
gem "lograge", group: [:production]
gem "local", path: "vendor/local"
gemspec
gemspec path: "engines/widget"
"#,
        );
        assert_eq!(found.gemspecs, [".", "engines/widget"]);
        let by = |name: &str| found.gems.iter().find(|g| g.name == name).unwrap();
        assert_eq!((by("rails").line, by("rails").col), (2, 1));
        assert_eq!(by("rails").require, Require::Auto);
        assert!(by("rails").groups.is_empty());
        assert_eq!(by("pg").require, Require::Off);
        assert_eq!(by("rspec-rails").groups, ["development", "test"]);
        assert_eq!(by("rspec-rails").line, 5);
        assert_eq!(by("debug").platforms, ["mri"]);
        assert_eq!(
            by("debug").require,
            Require::Paths(vec!["debug/prelude".into()])
        );
        assert_eq!(by("lograge").groups, ["production"]);
        assert!(by("local").local);
        assert_eq!(by("local").path.as_deref(), Some("vendor/local"));
        assert!(!by("rails").local);
    }

    #[test]
    fn a_gemspec_development_dependency_is_in_the_development_group() {
        let found = declarations(
            br#"Gem::Specification.new do |s|
  s.add_dependency "rack", ">= 2"
  s.add_development_dependency "rspec"
end
"#,
        );
        let names: Vec<(&str, &[String])> = found
            .gems
            .iter()
            .map(|g| (g.name.as_str(), g.groups.as_slice()))
            .collect();
        assert_eq!(names[0], ("rack", &[][..]));
        assert_eq!(names[1].0, "rspec");
        assert_eq!(names[1].1, ["development"]);
    }

    #[test]
    fn a_lockfile_says_what_each_gem_brings_and_what_was_asked_for() {
        let lock = Lock::parse(
            "GEM\n  remote: https://rubygems.org/\n  specs:\n    a (1.0)\n      b (>= 1)\n    \
             b (1.0)\n      c\n    c (1.0)\n    d (1.0)\n\nPLATFORMS\n  ruby\n\n\
             DEPENDENCIES\n  a (~> 1)\n  d!\n\nBUNDLED WITH\n   2.5.0\n",
        );
        assert_eq!(lock.direct, ["a", "d"].map(String::from).into());
        assert_eq!(lock.closure("a"), ["b", "c"].map(String::from).into());
        assert_eq!(lock.needed_by("c"), ["b"]);
        assert!(lock.closure("d").is_empty());
    }

    #[test]
    fn a_specification_names_its_commands_when_it_lists_them() {
        let found = spec(
            br#"Gem::Specification.new do |s|
  s.executables = ["rubocop".freeze]
  s.extensions = ["ext/x/extconf.rb".freeze]
end
"#,
        );
        assert_eq!(found.executables, Some(vec!["rubocop".to_string()]));
        assert!(found.extensions);
        let computed =
            spec(b"Gem::Specification.new { |s| s.executables = s.files.grep(%r{^exe/}) }");
        assert_eq!(computed.executables, None);
        assert!(!computed.extensions);
        let unwritten = spec(b"Gem::Specification.new { |s| s.name = \"mini_mime\" }");
        assert_eq!(unwritten.executables, Some(Vec::new()));
    }
}
