//! A `require` string is a path: which file `require`, `require_relative`,
//! `load` and `autoload` name, found statically.
//!
//! Two halves, both pure. [`requires_in`] reads a file's bytes and returns
//! each call whose path is written literally, with the byte span of the
//! string. [`resolve`] follows one of them the way Ruby would — relative to
//! the requiring file, or along a load path — given the directories and a
//! file-exists test, so the rules are unit-tested without a disk. Building
//! the load path is the only part that reads one ([`LoadPath::for_checkout`]).
//!
//! Nothing here guesses. A path assembled at runtime is not followed; a
//! native extension is reported as one, not swapped for a `.rb` further down
//! the path; and several matches are all returned, in load-path order — but
//! the stdlib's copy only when nothing ahead of it matched, since it is last
//! on any load path.

use ruby_prism::{Node, Visit};
use std::collections::HashSet;
use std::ops::Range;
use std::path::{Component, Path, PathBuf};

/// The call, which decides where the search starts and whether `.rb` is
/// implied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verb {
    Require,
    RequireRelative,
    /// Takes the extension as written, and falls back to the working
    /// directory when the load path has nothing.
    Load,
    Autoload,
}

/// What a computed path is built on. Only the idioms that are literals in
/// disguise: `File.expand_path("x", __dir__)` names one file whatever runs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Anchor {
    /// `__FILE__` — the requiring file itself, so `"../x"` is a sibling.
    File,
    /// `__dir__`, `File.dirname(__FILE__)`.
    Dir,
    /// `Rails.root`, taken as the checkout root.
    Root,
}

/// A path-naming call, as written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Require {
    pub(crate) verb: Verb,
    /// `None` for a bare literal, whose meaning the verb decides.
    pub(crate) anchor: Option<Anchor>,
    pub(crate) path: String,
    /// Bytes of the string literal(s) naming the path, quotes included — the
    /// whole string is the link, wherever in it the cursor is.
    pub(crate) span: Range<usize>,
}

/// Every `require`-like call in a file whose path can be read without running
/// it, in source order.
pub(crate) fn requires_in(src: &[u8]) -> Vec<Require> {
    let parsed = ruby_prism::parse(src);
    let mut finder = Finder { found: Vec::new() };
    finder.visit(&parsed.node());
    finder.found
}

struct Finder {
    found: Vec<Require>,
}

impl<'pr> Visit<'pr> for Finder {
    fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
        self.found.extend(require_of(node));
        ruby_prism::visit_call_node(self, node);
    }
}

fn require_of(call: &ruby_prism::CallNode<'_>) -> Option<Require> {
    let (verb, at) = match call.name().as_slice() {
        b"require" => (Verb::Require, 0),
        b"require_relative" => (Verb::RequireRelative, 0),
        b"load" => (Verb::Load, 0),
        b"autoload" => (Verb::Autoload, 1),
        _ => return None,
    };
    // `Foo.autoload :Bar, "x"` is Module#autoload on any module; the others
    // are Kernel's, and `thing.load(…)` is somebody's own method.
    let kernel = call.receiver().is_none_or(|receiver| {
        receiver.as_self_node().is_some() || constant_named(&receiver, b"Kernel")
    });
    if !kernel && verb != Verb::Autoload {
        return None;
    }
    let argument = call.arguments()?.arguments().iter().nth(at)?;
    if let Some(string) = argument.as_string_node() {
        return Some(Require {
            verb,
            anchor: None,
            path: String::from_utf8(string.unescaped().to_vec()).ok()?,
            span: span_of(&argument),
        });
    }
    let built = anchored(&argument)?;
    Some(Require {
        verb,
        anchor: Some(built.anchor),
        path: built.path.trim_start_matches('/').to_string(),
        // An anchor alone (`require __dir__`) names no file.
        span: built.span?,
    })
}

/// A path built on an [`Anchor`]: the anchor, the literal text joined on to
/// it (`/`-separated, leading `/` meaning "directly below"), and the span of
/// that literal text.
struct Built {
    anchor: Anchor,
    path: String,
    span: Option<Range<usize>>,
}

impl Built {
    fn at(anchor: Anchor) -> Option<Built> {
        Some(Built {
            anchor,
            path: String::new(),
            span: None,
        })
    }

    /// `File.join`/`Pathname#join`: each part below the last.
    fn join(mut self, parts: &[Node<'_>]) -> Option<Built> {
        for part in parts {
            let text = literal(part)?;
            if text.starts_with('/') {
                return None; // an absolute part discards the anchor
            }
            self.path = format!("{}/{text}", self.path);
            self.widen(span_of(part));
        }
        Some(self)
    }

    /// String `+`: only a text that starts a new component is a path below.
    fn concat(mut self, part: &Node<'_>) -> Option<Built> {
        let text = literal(part)?;
        if !text.starts_with('/') {
            return None;
        }
        self.path.push_str(&text);
        self.widen(span_of(part));
        Some(self)
    }

    fn widen(&mut self, span: Range<usize>) {
        self.span = Some(match self.span.take() {
            Some(held) => held.start.min(span.start)..held.end.max(span.end),
            None => span,
        });
    }
}

/// The path idioms that are literals in disguise. Anything else — a variable,
/// a method, an interpolation of something else — is a path decided at
/// runtime, and not followed.
fn anchored(node: &Node<'_>) -> Option<Built> {
    if node.as_source_file_node().is_some() {
        return Built::at(Anchor::File);
    }
    if let Some(string) = node.as_interpolated_string_node() {
        // `"#{__dir__}/x"`: one anchor, then text.
        let parts: Vec<Node<'_>> = string.parts().iter().collect();
        let [first, rest @ ..] = parts.as_slice() else {
            return None;
        };
        let mut statements: Vec<Node<'_>> = first
            .as_embedded_statements_node()?
            .statements()?
            .body()
            .iter()
            .collect();
        if statements.len() != 1 {
            return None;
        }
        let mut built = anchored(&statements.pop()?)?;
        let text: String = rest.iter().map(literal).collect::<Option<_>>()?;
        if !text.starts_with('/') {
            return None;
        }
        built.path.push_str(&text);
        built.span = Some(span_of(node));
        return Some(built);
    }
    let call = node.as_call_node()?;
    let args: Vec<Node<'_>> = call
        .arguments()
        .map(|a| a.arguments().iter().collect())
        .unwrap_or_default();
    let on_file = call.receiver().is_some_and(|r| constant_named(&r, b"File"));
    match (call.name().as_slice(), call.receiver(), args.as_slice()) {
        (b"__dir__", None, []) => Built::at(Anchor::Dir),
        (b"root", Some(receiver), []) if constant_named(&receiver, b"Rails") => {
            Built::at(Anchor::Root)
        }
        (b"dirname", _, [path]) if on_file => match anchored(path)? {
            Built {
                anchor: Anchor::File,
                path,
                ..
            } if path.is_empty() => Built::at(Anchor::Dir),
            _ => None,
        },
        (b"expand_path", _, [path]) if on_file => anchored(path),
        (b"expand_path", _, [path, base]) if on_file => {
            anchored(base)?.join(std::slice::from_ref(path))
        }
        (b"join", _, [base, parts @ ..]) if on_file => anchored(base)?.join(parts),
        // `Rails.root.join("x")` — a Pathname, joined the same way.
        (b"join", Some(receiver), parts) => anchored(&receiver)?.join(parts),
        (b"to_s", Some(receiver), []) => anchored(&receiver),
        (b"+", Some(receiver), [part]) => anchored(&receiver)?.concat(part),
        _ => None,
    }
}

fn literal(node: &Node<'_>) -> Option<String> {
    String::from_utf8(node.as_string_node()?.unescaped().to_vec()).ok()
}

fn constant_named(node: &Node<'_>, name: &[u8]) -> bool {
    node.as_constant_read_node()
        .is_some_and(|read| read.name().as_slice() == name)
}

fn span_of(node: &Node<'_>) -> Range<usize> {
    let location = node.location();
    location.start_offset()..location.end_offset()
}

/// Where a load-path directory came from — what a hover names it by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Origin {
    /// The checkout's own code, or a path relative to the requiring file.
    Checkout,
    /// A gem, by its root (`…/gems/name-version`).
    Gem(PathBuf),
    /// Ruby's standard library, by its directory (`…/lib/ruby/3.4.0`).
    Stdlib(PathBuf),
}

/// One directory on the load path.
#[derive(Debug)]
pub(crate) struct Dir {
    pub(crate) path: PathBuf,
    pub(crate) origin: Origin,
    /// The directory's entries, for one that does not change — a gem's or the
    /// stdlib's. Listed once, so that a lookup visits only the few directories
    /// whose first component matches instead of stat-ing all three hundred.
    /// `None` for the checkout's own directories, which do change.
    pub(crate) names: Option<HashSet<String>>,
}

impl Dir {
    /// Could `relative` be in here? Exact for a listed directory, always yes
    /// for an unlisted one.
    fn may_hold(&self, relative: &str) -> bool {
        let head = relative.split('/').next().unwrap_or(relative);
        self.names.as_ref().is_none_or(|names| names.contains(head))
    }
}

/// `$LOAD_PATH`, as far as it can be known without running the app: the
/// checkout's conventional directories, its path gems, each bundled gem's
/// `lib/`, then the standard library the bundle was installed beside.
#[derive(Debug, Default)]
pub(crate) struct LoadPath {
    pub(crate) dirs: Vec<Dir>,
}

/// A file a require names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Found {
    pub(crate) file: PathBuf,
    pub(crate) origin: Origin,
    /// A compiled extension (`.so`, `.bundle`). There is nothing to open, and
    /// it shadows any `.rb` later on the path — which is why it is reported
    /// rather than skipped.
    pub(crate) native: bool,
}

/// Where the requiring file sits.
pub(crate) struct Context<'a> {
    pub(crate) file: &'a Path,
    /// The checkout root: Ruby's working directory, statically, and what
    /// `Rails.root` is taken to be. `None` outside a checkout.
    pub(crate) root: Option<&'a Path>,
    pub(crate) load_path: &'a LoadPath,
}

/// Extensions Ruby loads as compiled code. Ruby maps `.so` to the platform's
/// own, so both are native wherever they are written.
const NATIVE: [&str; 4] = ["so", "bundle", "dll", "o"];

/// The files a require names, in the order Ruby would try them.
///
/// Empty when nothing matches — never a nearby file instead. When a native
/// extension is the first match it is the only one returned, since that is
/// what Ruby loads.
pub(crate) fn resolve(
    require: &Require,
    cx: &Context<'_>,
    is_file: impl Fn(&Path) -> bool,
) -> Vec<Found> {
    let path = require.path.as_str();
    if path.is_empty() || path.starts_with('~') {
        return Vec::new();
    }
    let exact = require.verb == Verb::Load;
    match place(require, cx) {
        Place::One(target) => {
            return probe(&target, exact, Origin::Checkout, &is_file)
                .into_iter()
                .collect();
        }
        Place::Nowhere => return Vec::new(),
        Place::LoadPath => {}
    }

    let names = candidates(path, exact);
    let mut found = Vec::new();
    for dir in &cx.load_path.dirs {
        // The stdlib is last on `$LOAD_PATH` whoever sets the rest up, so a
        // gem's `json.rb` shadows its copy for certain.
        if !found.is_empty() && matches!(dir.origin, Origin::Stdlib(_)) {
            break;
        }
        let Some(hit) = probe_in(dir, &names, &is_file) else {
            continue;
        };
        if hit.native {
            if found.is_empty() {
                return vec![hit];
            }
            continue;
        }
        found.push(hit);
    }
    // `load` falls back to the working directory when the load path has
    // nothing.
    if found.is_empty()
        && exact
        && let Some(root) = cx.root
    {
        found.extend(probe(&root.join(path), true, Origin::Checkout, &is_file));
    }
    found
}

/// Where Ruby looks for a path.
enum Place {
    One(PathBuf),
    /// Relative to something this position does not know — a checkout root
    /// outside any checkout.
    Nowhere,
    LoadPath,
}

fn place(require: &Require, cx: &Context<'_>) -> Place {
    let path = require.path.as_str();
    let at = |base: Option<&Path>| {
        base.map_or(Place::Nowhere, |base| {
            Place::One(normalize(&base.join(path)))
        })
    };
    let explicit = path.starts_with("./") || path.starts_with("../");
    match (require.anchor, require.verb) {
        (Some(Anchor::File), _) => at(Some(cx.file)),
        (Some(Anchor::Dir), _) | (None, Verb::RequireRelative) => at(cx.file.parent()),
        (Some(Anchor::Root), _) => at(cx.root),
        _ if Path::new(path).is_absolute() => at(Some(Path::new("/"))),
        // Ruby skips the load path for these and reads the working directory.
        _ if explicit => at(cx.root),
        _ => Place::LoadPath,
    }
}

/// The file `target` names with Ruby's extension rules, if any.
fn probe(
    target: &Path,
    exact: bool,
    origin: Origin,
    is_file: &impl Fn(&Path) -> bool,
) -> Option<Found> {
    candidates(&target.to_string_lossy(), exact)
        .into_iter()
        .find(|(candidate, _)| is_file(Path::new(candidate)))
        .map(|(file, native)| Found {
            file: PathBuf::from(file),
            origin,
            native,
        })
}

/// [`probe`] under one load-path directory, skipping the stat when the
/// directory's listing already says no.
fn probe_in(
    dir: &Dir,
    names: &[(String, bool)],
    is_file: &impl Fn(&Path) -> bool,
) -> Option<Found> {
    names
        .iter()
        .filter(|(candidate, _)| dir.may_hold(candidate))
        .find(|(candidate, _)| is_file(&normalize(&dir.path.join(candidate))))
        .map(|(candidate, native)| Found {
            file: normalize(&dir.path.join(candidate)),
            origin: dir.origin.clone(),
            native: *native,
        })
}

/// The file names to try for `path`, in Ruby's order, each marked native or
/// not. `require "x"` tries `x.rb`, then the compiled `x`; a written
/// extension is the only one tried; `load` takes the name as written.
fn candidates(path: &str, exact: bool) -> Vec<(String, bool)> {
    if exact {
        return vec![(path.to_string(), false)];
    }
    let extension = Path::new(path).extension().and_then(|e| e.to_str());
    match extension {
        Some("rb") => vec![(path.to_string(), false)],
        Some(ext) if NATIVE.contains(&ext) => vec![(path.to_string(), true)],
        _ => std::iter::once((format!("{path}.rb"), false))
            .chain(
                NATIVE[..2]
                    .iter()
                    .map(|ext| (format!("{path}.{ext}"), true)),
            )
            .collect(),
    }
}

/// `a/b/../c` → `a/c`, without touching the disk — `File.expand_path`'s
/// arithmetic. A symlinked directory is followed textually, as Ruby does.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

impl LoadPath {
    /// The load path of a checkout whose bundle resolves `gem_roots`, on the
    /// stdlib its index chose (DEC-180) — or, when it indexed none, the one
    /// its gems were installed into.
    ///
    /// Reads the disk once per directory: the checkout's candidates are
    /// checked to exist, and each gem and stdlib directory is listed so later
    /// lookups can skip it without a stat.
    pub(crate) fn for_checkout(
        root: &Path,
        gem_roots: &[String],
        stdlib: Option<&str>,
    ) -> LoadPath {
        let mut dirs = Vec::new();
        let mut checkout = |path: PathBuf| {
            if path.is_dir() && !dirs.iter().any(|d: &Dir| d.path == path) {
                dirs.push(Dir {
                    path,
                    origin: Origin::Checkout,
                    names: None,
                });
            }
        };
        // `lib` by convention; `spec` and `test` because rspec-core and
        // `rails test` add them, which is what `require "rails_helper"` needs.
        for conventional in ["lib", "spec", "test"] {
            checkout(root.join(conventional));
        }
        // A path gem's code is in the checkout, and on the load path.
        if let Ok(lockfile) = crate::scan::read_text(root.join("Gemfile.lock")) {
            for dir in path_gem_libs(&lockfile) {
                checkout(normalize(&root.join(dir)));
            }
        }
        let gem_roots: Vec<&String> = gem_roots
            .iter()
            .filter(|gem| Some(gem.as_str()) != stdlib)
            .collect();
        for gem in &gem_roots {
            let gem = PathBuf::from(gem);
            dirs.extend(listed(gem.join("lib"), Origin::Gem(gem)));
        }
        let stdlib = stdlib.map(PathBuf::from).or_else(|| {
            gem_roots.iter().find_map(|gem| {
                let (lib_ruby, abi) = ruby_beside(Path::new(gem))?;
                let abi = abi.or_else(|| only_abi(&lib_ruby))?;
                Some(lib_ruby.join(abi))
            })
        });
        if let Some(stdlib) = stdlib {
            let arch = listed(stdlib.clone(), Origin::Stdlib(stdlib.clone())).map(|dir| {
                // The architecture directory holds `rbconfig.rb` and the
                // compiled half of the stdlib; it is searched after the rest.
                let arch = dir
                    .names
                    .iter()
                    .flatten()
                    .find(|name| stdlib.join(name).join("rbconfig.rb").is_file());
                let arch = arch.map(|name| stdlib.join(name));
                dirs.push(dir);
                arch
            });
            if let Some(arch) = arch.flatten() {
                dirs.extend(listed(arch, Origin::Stdlib(stdlib)));
            }
        }
        LoadPath { dirs }
    }
}

/// A directory with its entries listed, or `None` when it cannot be read.
fn listed(path: PathBuf, origin: Origin) -> Option<Dir> {
    let names = std::fs::read_dir(&path)
        .ok()?
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    Some(Dir {
        path,
        origin,
        names: Some(names),
    })
}

/// The `lib/ruby` of the Ruby a gem was installed into, and its ABI version
/// when the gem's path says it — the stdlib is `<lib/ruby>/<abi>`.
///
/// rbenv, asdf, Homebrew and system Rubies keep gems inside the install:
/// `<prefix>/lib/ruby/gems/<abi>/gems/<gem>`. rvm keeps them beside it,
/// `.rvm/gems/<ruby>[@gemset]/gems/<gem>` for `.rvm/rubies/<ruby>`, and the
/// ABI is not in that path. A `vendor/bundle` names no Ruby at all, and
/// yields nothing rather than one that might not be the one in use.
pub(crate) fn ruby_beside(gem_root: &Path) -> Option<(PathBuf, Option<String>)> {
    let named = |path: &Path, name: &str| path.file_name().is_some_and(|n| n == name);
    let gems = gem_root.parent()?;
    let home = gems.parent()?;
    let above = home.parent()?;
    if !named(gems, "gems") || !named(above, "gems") {
        return None;
    }
    let abi = home.file_name()?.to_string_lossy().into_owned();
    let lib_ruby = above.parent()?;
    if named(lib_ruby, "ruby") && lib_ruby.parent().is_some_and(|lib| named(lib, "lib")) {
        return Some((lib_ruby.to_path_buf(), Some(abi)));
    }
    if named(lib_ruby, ".rvm") {
        let ruby = abi.split('@').next()?;
        return Some((lib_ruby.join("rubies").join(ruby).join("lib/ruby"), None));
    }
    None
}

/// The one ABI directory (`3.4.0`) in a Ruby's `lib/ruby`, beside `gems`,
/// `site_ruby` and `vendor_ruby`. Two would be a guess, so it is none.
fn only_abi(lib_ruby: &Path) -> Option<String> {
    let mut versions = std::fs::read_dir(lib_ruby)
        .ok()?
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(|c: char| c.is_ascii_digit()));
    let only = versions.next()?;
    versions.next().is_none().then_some(only)
}

/// The `lib/` directories of a lockfile's path gems, relative to the
/// checkout. A `PATH` section's `remote:` is one directory that may hold
/// several gems — rails' is `.` — so each named gem's own directory is
/// offered as well; the caller keeps the ones that exist.
pub(crate) fn path_gem_libs(lockfile: &str) -> Vec<String> {
    let mut libs = Vec::new();
    let mut remote: Option<String> = None;
    let mut in_path = false;
    for line in lockfile.lines() {
        if !line.starts_with(' ') && !line.trim().is_empty() {
            in_path = line.trim_end() == "PATH";
            remote = None;
            continue;
        }
        if !in_path {
            continue;
        }
        if let Some(dir) = line.trim().strip_prefix("remote: ") {
            libs.push(format!("{dir}/lib"));
            remote = Some(dir.to_string());
            continue;
        }
        // A gem is indented four; its own dependencies, six.
        let gem = line
            .strip_prefix("    ")
            .filter(|rest| !rest.starts_with(' '))
            .and_then(|rest| rest.split_once(" ("));
        if let (Some(dir), Some((name, _))) = (&remote, gem) {
            libs.push(format!("{dir}/{name}/lib"));
        }
    }
    libs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(src: &str) -> Vec<Require> {
        requires_in(src.as_bytes())
    }

    fn one(src: &str) -> Require {
        let mut all = found(src);
        assert_eq!(all.len(), 1, "{src}: {all:?}");
        all.pop().unwrap()
    }

    #[test]
    fn a_literal_spans_the_whole_string_quotes_included() {
        let src = "require_relative \"a/b\"\n";
        let require = one(src);
        assert_eq!(require.verb, Verb::RequireRelative);
        assert_eq!(require.anchor, None);
        assert_eq!(require.path, "a/b");
        assert_eq!(&src[require.span], "\"a/b\"");
    }

    #[test]
    fn autoload_names_its_path_second_on_any_receiver() {
        let require = one("Widget.autoload :Part, 'widget/part'\n");
        assert_eq!(require.verb, Verb::Autoload);
        assert_eq!(require.path, "widget/part");
    }

    #[test]
    fn a_load_method_on_something_else_is_not_kernels() {
        assert!(found("config.load \"x\"\nloader.require \"y\"\n").is_empty());
        assert_eq!(found("Kernel.require \"y\"\nself.load 'z.rb'\n").len(), 2);
    }

    #[test]
    fn a_path_decided_at_runtime_is_not_followed() {
        for src in [
            "require name\n",
            "require \"#{prefix}/x\"\n",
            "require File.join(base, \"x\")\n",
            "require File.expand_path(\"x\", root)\n",
            "require __dir__ + \"x\"\n",
            "require __dir__\n",
        ] {
            assert!(found(src).is_empty(), "{src}");
        }
    }

    #[test]
    fn literal_idioms_anchored_on_the_file_are_followed() {
        let cases = [
            ("require File.expand_path(\"x\", __dir__)", Anchor::Dir, "x"),
            (
                "require File.expand_path('../x', __FILE__)",
                Anchor::File,
                "../x",
            ),
            (
                "require File.expand_path(\"x\", File.dirname(__FILE__))",
                Anchor::Dir,
                "x",
            ),
            (
                "require File.join(__dir__, \"a\", \"b\")",
                Anchor::Dir,
                "a/b",
            ),
            (
                "require File.expand_path(File.dirname(__FILE__)) + '/../h'",
                Anchor::Dir,
                "../h",
            ),
            ("require \"#{__dir__}/a/b\"", Anchor::Dir, "a/b"),
            (
                "require Rails.root.join(\"script/x.rb\").to_s",
                Anchor::Root,
                "script/x.rb",
            ),
        ];
        for (src, anchor, path) in cases {
            let require = one(src);
            assert_eq!(
                (require.anchor, require.path.as_str()),
                (Some(anchor), path),
                "{src}"
            );
        }
    }

    #[test]
    fn a_computed_path_spans_its_literal_parts() {
        let src = "require File.join(__dir__, \"a\", \"b\")";
        assert_eq!(&src[one(src).span], "\"a\", \"b\"");
    }

    #[test]
    fn requires_are_found_anywhere_in_the_file() {
        let src = "module A\n  def self.x\n    require 'deep' if true\n  end\nend\n";
        assert_eq!(one(src).path, "deep");
    }

    // Resolution: a fake disk.

    fn disk(files: &[&str]) -> impl Fn(&Path) -> bool + use<> {
        let files: HashSet<PathBuf> = files.iter().map(PathBuf::from).collect();
        move |path: &Path| files.contains(path)
    }

    fn require(verb: Verb, path: &str) -> Require {
        Require {
            verb,
            anchor: None,
            path: path.to_string(),
            span: 0..0,
        }
    }

    fn files(found: Vec<Found>) -> Vec<String> {
        found
            .into_iter()
            .map(|f| f.file.to_string_lossy().into_owned())
            .collect()
    }

    fn resolve_in(req: &Require, load_path: &LoadPath, on_disk: &[&str]) -> Vec<Found> {
        let cx = Context {
            file: Path::new("/app/lib/widget/part.rb"),
            root: Some(Path::new("/app")),
            load_path,
        };
        resolve(req, &cx, disk(on_disk))
    }

    fn dir(path: &str, origin: Origin, names: Option<&[&str]>) -> Dir {
        Dir {
            path: PathBuf::from(path),
            origin,
            names: names.map(|n| n.iter().map(|s| s.to_string()).collect()),
        }
    }

    fn standard() -> LoadPath {
        LoadPath {
            dirs: vec![
                dir("/app/lib", Origin::Checkout, None),
                dir(
                    "/gems/shelf-1.0/lib",
                    Origin::Gem("/gems/shelf-1.0".into()),
                    Some(&["shelf", "shelf.rb", "json.rb"]),
                ),
                dir(
                    "/ruby/lib/ruby/3.4.0",
                    Origin::Stdlib("/ruby/lib/ruby/3.4.0".into()),
                    Some(&["json.rb", "json", "digest.bundle"]),
                ),
            ],
        }
    }

    #[test]
    fn require_relative_appends_rb_from_the_files_directory() {
        let on_disk = ["/app/lib/widget/gear.rb", "/app/lib/other.rb"];
        let empty = LoadPath::default();
        for (path, expected) in [
            ("gear", "/app/lib/widget/gear.rb"),
            ("gear.rb", "/app/lib/widget/gear.rb"),
            ("../other", "/app/lib/other.rb"),
        ] {
            let got = files(resolve_in(
                &require(Verb::RequireRelative, path),
                &empty,
                &on_disk,
            ));
            assert_eq!(got, vec![expected], "{path}");
        }
    }

    #[test]
    fn a_missing_file_resolves_to_nothing() {
        let empty = LoadPath::default();
        assert!(resolve_in(&require(Verb::RequireRelative, "gone"), &empty, &[]).is_empty());
        assert!(resolve_in(&require(Verb::Require, "gone"), &standard(), &[]).is_empty());
    }

    #[test]
    fn require_searches_the_load_path_in_order() {
        let on_disk = ["/app/lib/widget.rb", "/gems/shelf-1.0/lib/shelf/rack.rb"];
        let load_path = standard();
        let got = files(resolve_in(
            &require(Verb::Require, "widget"),
            &load_path,
            &on_disk,
        ));
        assert_eq!(got, vec!["/app/lib/widget.rb"]);
        let found = resolve_in(&require(Verb::Require, "shelf/rack"), &load_path, &on_disk);
        assert_eq!(found[0].origin, Origin::Gem("/gems/shelf-1.0".into()));
    }

    #[test]
    fn several_matches_are_all_returned_first_on_the_path_first() {
        let on_disk = ["/app/lib/shelf.rb", "/gems/shelf-1.0/lib/shelf.rb"];
        let got = files(resolve_in(
            &require(Verb::Require, "shelf"),
            &standard(),
            &on_disk,
        ));
        assert_eq!(
            got,
            vec!["/app/lib/shelf.rb", "/gems/shelf-1.0/lib/shelf.rb"],
            "which a runner puts first is not known statically"
        );
    }

    #[test]
    fn the_stdlib_is_read_only_when_nothing_before_it_has_the_file() {
        let on_disk = [
            "/gems/shelf-1.0/lib/json.rb",
            "/ruby/lib/ruby/3.4.0/json.rb",
        ];
        let got = files(resolve_in(
            &require(Verb::Require, "json"),
            &standard(),
            &on_disk,
        ));
        assert_eq!(got, vec!["/gems/shelf-1.0/lib/json.rb"], "the gem's copy");
        let got = files(resolve_in(
            &require(Verb::Require, "json"),
            &standard(),
            &on_disk[1..],
        ));
        assert_eq!(got, vec!["/ruby/lib/ruby/3.4.0/json.rb"]);
    }

    #[test]
    fn a_listed_directory_is_trusted_over_the_disk() {
        // On disk but absent from the listing: the listing is what is trusted.
        let on_disk = ["/gems/shelf-1.0/lib/zzz.rb"];
        assert!(resolve_in(&require(Verb::Require, "zzz"), &standard(), &on_disk).is_empty());
    }

    #[test]
    fn a_native_extension_first_on_the_path_is_the_answer() {
        let on_disk = ["/ruby/lib/ruby/3.4.0/digest.bundle"];
        let found = resolve_in(&require(Verb::Require, "digest"), &standard(), &on_disk);
        assert_eq!(found.len(), 1);
        assert!(found[0].native);
        let written = require(Verb::Require, "digest.so");
        assert!(resolve_in(&written, &standard(), &["/app/lib/digest.so"])[0].native);
    }

    #[test]
    fn load_takes_the_name_as_written_and_falls_back_to_the_root() {
        let on_disk = ["/app/lib/tasks/x.rake", "/app/script/y.rb"];
        let load_path = standard();
        let got = files(resolve_in(
            &require(Verb::Load, "tasks/x.rake"),
            &load_path,
            &on_disk,
        ));
        assert_eq!(got, vec!["/app/lib/tasks/x.rake"]);
        assert!(resolve_in(&require(Verb::Load, "script/y"), &load_path, &on_disk).is_empty());
        let got = files(resolve_in(
            &require(Verb::Load, "script/y.rb"),
            &load_path,
            &on_disk,
        ));
        assert_eq!(got, vec!["/app/script/y.rb"]);
    }

    #[test]
    fn an_explicitly_relative_require_reads_the_working_directory() {
        let on_disk = ["/app/config/boot.rb", "/app/lib/config/boot.rb"];
        let got = files(resolve_in(
            &require(Verb::Require, "./config/boot"),
            &standard(),
            &on_disk,
        ));
        assert_eq!(got, vec!["/app/config/boot.rb"]);
    }

    #[test]
    fn an_anchored_path_is_one_place() {
        let anchored = |anchor, path: &str| Require {
            anchor: Some(anchor),
            ..require(Verb::Require, path)
        };
        let on_disk = [
            "/app/lib/widget/gear.rb",
            "/app/lib/sibling.rb",
            "/app/script/x.rb",
        ];
        let load_path = standard();
        for (req, expected) in [
            (anchored(Anchor::Dir, "gear"), "/app/lib/widget/gear.rb"),
            (anchored(Anchor::File, "../gear"), "/app/lib/widget/gear.rb"),
            (anchored(Anchor::Dir, "../sibling"), "/app/lib/sibling.rb"),
            (anchored(Anchor::Root, "script/x.rb"), "/app/script/x.rb"),
        ] {
            assert_eq!(
                files(resolve_in(&req, &load_path, &on_disk)),
                vec![expected],
                "{req:?}"
            );
        }
    }

    #[test]
    fn the_stdlib_the_index_chose_is_on_the_load_path_whatever_the_gems_say() {
        let base = std::env::temp_dir().join(format!("trekr-loadpath-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let gem = base.join("app/vendor/bundle/ruby/3.4.0/gems/rack-3.0.0");
        let stdlib = base.join("ruby/lib/ruby/3.4.0");
        for dir in [gem.join("lib"), stdlib.clone()] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(stdlib.join("stash.rb"), "").unwrap();
        let gems = [gem.to_string_lossy().into_owned()];
        let stdlibs = |load_path: LoadPath| -> Vec<PathBuf> {
            load_path
                .dirs
                .into_iter()
                .filter(|dir| matches!(dir.origin, Origin::Stdlib(_)))
                .map(|dir| dir.path)
                .collect()
        };
        let app = base.join("app");
        assert_eq!(
            stdlibs(LoadPath::for_checkout(&app, &gems, None)),
            Vec::<PathBuf>::new(),
            "a vendored bundle names no Ruby"
        );
        assert_eq!(
            stdlibs(LoadPath::for_checkout(
                &app,
                &gems,
                Some(&stdlib.to_string_lossy())
            )),
            [stdlib]
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_ruby_is_found_beside_an_installed_gem_and_not_a_vendored_one() {
        let beside = |gem: &str| ruby_beside(Path::new(gem));
        assert_eq!(
            beside("/r/3.4.1/lib/ruby/gems/3.4.0/gems/rack-3.0.0"),
            Some((PathBuf::from("/r/3.4.1/lib/ruby"), Some("3.4.0".into())))
        );
        assert_eq!(
            beside("/h/.rvm/gems/ruby-3.4.1@app/gems/rack-3.0.0"),
            Some((PathBuf::from("/h/.rvm/rubies/ruby-3.4.1/lib/ruby"), None))
        );
        assert_eq!(
            beside("/app/vendor/bundle/ruby/3.4.0/gems/rack-3.0.0"),
            None
        );
    }

    #[test]
    fn path_gems_offer_their_remote_and_each_named_gems_lib() {
        let lockfile = "\
PATH
  remote: .
  specs:
    actionpack (8.0.0)
      rack (>= 2)
    rails (8.0.0)

PATH
  remote: engines/billing
  specs:
    billing (1.0.0)

GEM
  remote: https://rubygems.org/
  specs:
    rack (3.0.0)
";
        assert_eq!(
            path_gem_libs(lockfile),
            vec![
                "./lib",
                "./actionpack/lib",
                "./rails/lib",
                "engines/billing/lib",
                "engines/billing/billing/lib",
            ]
        );
    }
}
