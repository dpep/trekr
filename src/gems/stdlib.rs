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
use std::collections::HashSet;
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
/// `$GEM_HOME` belongs to, the `ruby` on `$PATH`.
///
/// Only for a checkout that says it is a Ruby project — it resolves gems
/// (`bundled`), or names a Ruby. A directory of scripts with neither has no
/// Ruby to speak of, and a test's scratch repository stays hermetic.
pub(crate) fn for_checkout(repo: &Path, bundled: bool) -> Option<Stdlib> {
    let version = super::project_ruby(repo);
    if !bundled && version.is_none() {
        return None;
    }
    if let Some(version) = version
        && let Some(root) = named(&version)
    {
        return Some(Stdlib::at(
            root,
            format!("Ruby {version}, which the checkout names"),
        ));
    }
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
    let ruby = super::path_ruby()?;
    let root = stdlib_in(ruby.parent()?.parent()?)?;
    Some(Stdlib::at(
        root,
        format!(
            "the ruby on $PATH ({})",
            crate::core::paths::pretty(&ruby.to_string_lossy())
        ),
    ))
}

/// Every Ruby installed by a version manager or Homebrew, by its prefix.
fn installs() -> Vec<PathBuf> {
    let mut patterns = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        let home = PathBuf::from(home);
        patterns.push(home.join(".rvm/rubies/*"));
        patterns.push(home.join(".rbenv/versions/*"));
        patterns.push(home.join(".asdf/installs/ruby/*"));
    }
    patterns.push(PathBuf::from("/opt/homebrew/Cellar/ruby/*"));
    patterns.push(PathBuf::from("/usr/local/Cellar/ruby/*"));
    patterns.iter().flat_map(|p| super::expand(p)).collect()
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
    installs()
        .into_iter()
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
