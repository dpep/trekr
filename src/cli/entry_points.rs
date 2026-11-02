//! Rows reached from outside the repo (DEC-657): a gem's public API, a
//! routed controller action, a background job. Static analysis cannot see
//! those callers at all, so such a row keeps its tier, is graded `lower`,
//! and says what kind of entry point it is and what evidence would settle it.

use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::tree::Tree;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum EntryPoint {
    /// Public API: a public or protected method, class or module under a
    /// gem's `lib/`.
    Api,
    /// A controller action a route reaches, or a controller only routes name.
    Route,
    /// A background job's class.
    Job,
}

impl EntryPoint {
    /// What the row is, how it is reached, and what would settle it.
    pub(super) fn caveat(self, method: bool) -> String {
        let (what, how, evidence) = match (self, method) {
            (Self::Api, _) => (
                "public API",
                "its callers outside this repo are invisible",
                "its dependents or usage data",
            ),
            (Self::Route, true) => (
                "a routed controller action",
                "reached by HTTP requests",
                "request logs or APM (e.g. Datadog)",
            ),
            (Self::Route, false) => (
                "a routed controller",
                "reached by HTTP requests",
                "request logs or APM (e.g. Datadog)",
            ),
            (Self::Job, _) => (
                "a background job",
                "reached by enqueued and scheduled jobs",
                "job metrics or logs",
            ),
        };
        format!("{what}: {how} — confirm with {evidence}")
    }
}

/// The ancestors a background job has: Sidekiq's mixins, Active Job's base.
const JOB_ANCESTORS: &[&str] = &[
    "Sidekiq::Job",
    "Sidekiq::Worker",
    "ActiveJob::Base",
    "ApplicationJob",
];

/// Is `class` a background job? An unresolved ancestor counts, as a gem
/// not installed leaves Sidekiq's own unresolved.
pub(super) fn is_job(tree: &Tree, class: &str) -> bool {
    let ancestors = tree.ancestors(class);
    class != "ApplicationJob"
        && ancestors
            .chain
            .iter()
            .chain(&ancestors.unresolved)
            .filter(|name| name.as_str() != class)
            .any(|name| JOB_ANCESTORS.contains(&crate::tree::public_name(name)))
}

/// What `--dead` has read of the checkout's gems: which files are a gem's
/// `lib/`, and which constants and definitions their authors mark internal.
#[derive(Default)]
pub(super) struct Gems {
    /// A directory → whether it holds a `*.gemspec`.
    gemspecs: HashMap<PathBuf, bool>,
    /// A file's lines, read once.
    lines: HashMap<String, Vec<String>>,
    /// A constant's whole name → what its marks and its namespaces' make of it.
    scopes: HashMap<String, Scope>,
}

impl Gems {
    /// Is `path` (absolute, or relative to `root`) in a published gem's
    /// `lib/`: the checkout is a library (a `*.gemspec` at its root, or no
    /// app there), and the file is in its root's `lib` or in a `lib`
    /// directory beside a nested `*.gemspec` (Rails' `activerecord/`,
    /// protobuf's `ruby/`)? A Hoe gem (minitest) generates its gemspec at
    /// release, so a library's root `lib/` needs none. An app's own engines
    /// are called by the app, which is in the checkout; a test's files are no
    /// API.
    pub(super) fn in_gem_lib(&mut self, root: &Path, path: &str) -> bool {
        let relative = Path::new(path)
            .strip_prefix(root)
            .unwrap_or(Path::new(path));
        // A Rails app packaged as a gem is still an app; but an engine gem
        // keeps an `app/`, and a gem a demo `config.ru` (sdoc), beside its
        // root gemspec, so those markers yield to it.
        if root.join("config/application.rb").is_file()
            || (!self.has_gemspec(root)
                && (root.join("app").is_dir() || root.join("config.ru").is_file()))
            || super::dead_consts::in_tests(&relative.to_string_lossy())
        {
            return false;
        }
        let segments: Vec<_> = relative.iter().collect();
        let mut dir = root.to_path_buf();
        for (at, segment) in segments.iter().enumerate() {
            if at + 1 == segments.len() {
                break;
            }
            if *segment == "lib" && (at == 0 || self.has_gemspec(&dir)) {
                return true;
            }
            dir.push(segment);
        }
        false
    }

    fn has_gemspec(&mut self, dir: &Path) -> bool {
        *self.gemspecs.entry(dir.to_path_buf()).or_insert_with(|| {
            std::fs::read_dir(dir).is_ok_and(|entries| {
                entries.flatten().any(|entry| {
                    entry.path().extension().is_some_and(|ext| ext == "gemspec")
                        // Through a symlink, which `DirEntry::file_type` is not.
                        && entry.path().is_file()
                })
            })
        })
    }

    fn lines(&mut self, path: &str) -> &[String] {
        self.lines.entry(path.to_string()).or_insert_with(|| {
            crate::scan::read_text(path)
                .map(|text| text.lines().map(str::to_string).collect())
                .unwrap_or_default()
        })
    }

    /// What the documentation of the definition at `path:line` marks it:
    /// `:nodoc:` on its line (RDoc), or in the comment directly above it a
    /// line opening with `@api private` / `@private` (YARD), or a first line
    /// opening with `Private:` / `Internal:` (TomDoc).
    fn marks(&mut self, path: &str, line: u32) -> Marks {
        let lines = self.lines(path);
        let mut marks = Marks::default();
        let Some(at) = (line as usize)
            .checked_sub(1)
            .filter(|at| *at < lines.len())
        else {
            return marks;
        };
        if let Some((_, comment)) = lines[at].split_once('#')
            && let Some((_, after)) = comment.split_once(":nodoc:")
        {
            marks.hidden = true;
            // RDoc reads `all` after the directive case-blind.
            marks.all = after.trim_start().to_ascii_lowercase().starts_with("all");
        }
        let end = doc_end(lines, at);
        let start = (0..end)
            .rev()
            .take_while(|&n| lines[n].trim_start().starts_with('#'))
            .last()
            .unwrap_or(end);
        for (n, text) in lines[start..end].iter().map(|l| docstring(l)).enumerate() {
            // Both YARD tags are transitive: a namespace's reaches its children.
            match yard_tag(text) {
                Some(("private", _)) => marks.all = true,
                Some(("api", text)) => {
                    marks.api_private = Some(text.split_whitespace().next() == Some("private"));
                }
                _ => {}
            }
            // RDoc's TomDoc reads the status at the comment's start (`tom_doc.rb`).
            if n == 0 && (text.starts_with("Private:") || text.starts_with("Internal:")) {
                marks.hidden = true;
            }
        }
        marks
    }

    /// Is the constant assigned at `path:line` a deprecated proxy for a
    /// class, kept for its old callers (Active Support's
    /// `DeprecatedConstantProxy.new`)?
    pub(super) fn deprecated_proxy(&mut self, path: &str, line: u32) -> bool {
        let lines = self.lines(path);
        (line as usize)
            .checked_sub(1)
            .and_then(|at| lines.get(at))
            .is_some_and(|text| code(text).contains("DeprecatedConstantProxy.new"))
    }

    /// The bare visibility toggle in force at the `def` at `path:line`: the
    /// nearest `private`, `protected`, `public` or `module_function` line
    /// above it at its own indentation, within its body. Ruby ends
    /// `module_function`'s mode at the next bare toggle, which the
    /// extractor does not, so its singleton copy may be one Ruby never made.
    pub(super) fn toggle(&mut self, path: &str, line: u32) -> Option<&'static str> {
        let lines = self.lines(path);
        let at = (line as usize)
            .checked_sub(1)
            .filter(|at| *at < lines.len())?;
        let indent = |text: &str| text.len() - text.trim_start().len();
        let width = indent(&lines[at]);
        for text in lines[..at].iter().rev() {
            if text.trim().is_empty() || text.trim_start().starts_with('#') {
                continue;
            }
            if indent(text) < width {
                return None;
            }
            if indent(text) == width {
                let toggle = ["private", "protected", "public", "module_function"]
                    .into_iter()
                    .find(|toggle| code(text) == *toggle);
                if toggle.is_some() {
                    return toggle;
                }
            }
        }
        None
    }

    /// Does RDoc's `:doc:` make the method at `path:line` documented API,
    /// private or not? RDoc 8.1 (`parser/ruby.rb`, `comment.rb`) reads a
    /// directive opening a line of the comment above the def (blank lines
    /// between allowed) or of a comment trailing the def's line.
    pub(super) fn doc_directive(&mut self, path: &str, line: u32) -> bool {
        let lines = self.lines(path);
        let Some(at) = (line as usize)
            .checked_sub(1)
            .filter(|at| *at < lines.len())
        else {
            return false;
        };
        let directive = |text: &str| {
            text.trim_start_matches('#')
                .trim_start()
                .starts_with(":doc:")
        };
        // Any `#`, as the first may open an interpolation (`"@#{name}"`).
        let def_line = &lines[at];
        if def_line
            .match_indices('#')
            .any(|(at, _)| directive(&def_line[at..]))
        {
            return true;
        }
        let mut end = doc_end(lines, at);
        while end > 0 && lines[end - 1].trim().is_empty() {
            end -= 1;
        }
        lines[..end]
            .iter()
            .rev()
            .map(|text| text.trim_start())
            .take_while(|text| text.starts_with('#'))
            .any(directive)
    }

    /// Is the method defined at `path:line` in `owner` internal: marked so
    /// itself, written in a body marked `:nodoc:` (RDoc leaves out the
    /// methods a `:nodoc:` body defines) and not `redocumented`, inside an
    /// opening whose mark
    /// reaches what is nested in it, or held by a `private_constant`?
    pub(super) fn internal_method(
        &mut self,
        tree: &Tree,
        root: &str,
        owner: &str,
        path: &str,
        line: u32,
    ) -> bool {
        let own = self.marks(path, line);
        let around = self.enclosing(path, line);
        // YARD: an `@api` tag written on the method overrides its class's.
        let api_private = own.api_private.or(around.api_private);
        if own.hidden
            || own.all
            || around.all
            || api_private == Some(true)
            || self.scope(tree, root, owner).private
        {
            return true;
        }
        // The body it is written in: its owner's nearest opening above it.
        let body = tree
            .sites(owner)
            .into_iter()
            .filter(|site| site.path == path && site.line <= line)
            .max_by_key(|site| site.line);
        body.is_some_and(|site| {
            self.marks(&site.path, site.line).hidden
                && !self.redocumented(site.line as usize, path, line)
        })
    }

    /// Does an own-line `# :doc:` turn a `:nodoc:` body's documentation back
    /// on for the def at `path:line`: is it the last own-line `:doc:` or
    /// `:nodoc:` at the def's indentation between the body's opening line
    /// `open` and the def? RDoc 8.1 applies every comment's directives to
    /// the body's class as it parses (`parser/ruby.rb`
    /// `handle_code_object_directives(@container, …)`), and
    /// `Context#add_method` keeps a method only while the class is
    /// documented. A trailing `# :doc:` is the method's own and does not.
    fn redocumented(&mut self, open: usize, path: &str, line: u32) -> bool {
        let lines = self.lines(path);
        let at = (line as usize).saturating_sub(1).min(lines.len());
        let indent = |text: &str| text.len() - text.trim_start().len();
        let Some(width) = lines.get(at).map(|text| indent(text)) else {
            return false;
        };
        lines[open.min(at)..at]
            .iter()
            .rev()
            .filter(|text| indent(text) == width && text.trim_start().starts_with('#'))
            .find_map(|text| {
                let text = text.trim_start().trim_start_matches('#').trim_start();
                if text.starts_with(":doc:") {
                    Some(true)
                } else if text.starts_with(":nodoc:") {
                    Some(false)
                } else {
                    None
                }
            })
            .unwrap_or(false)
    }

    /// Is the constant `fqn` internal: marked so on any of its openings
    /// (RDoc's `:nodoc:` holds for the class wherever it is reopened), held
    /// by a `private_constant`, its own or a namespace's, or written only
    /// inside openings whose mark reaches what is nested in them —
    /// `:nodoc: all`, YARD's `@api private` and `@private`? A plain
    /// `:nodoc:` reaches nothing nested: RDoc documents the classes in one.
    pub(super) fn internal_constant(&mut self, tree: &Tree, root: &str, fqn: &str) -> bool {
        self.scope(tree, root, fqn).internal
    }

    fn scope(&mut self, tree: &Tree, root: &str, fqn: &str) -> Scope {
        if let Some(known) = self.scopes.get(fqn) {
            return *known;
        }
        let prefix = format!("{root}/");
        let tail = fqn.rsplit("::").next().unwrap_or(fqn).to_string();
        let sites: Vec<_> = tree
            .sites(fqn)
            .into_iter()
            .filter(|site| site.path.starts_with(&prefix))
            .collect();
        let mut marked = false;
        let mut private = false;
        let mut api_private = None;
        let mut nested = !sites.is_empty();
        for site in &sites {
            let marks = self.marks(&site.path, site.line);
            marked |= marks.hidden || marks.all;
            private |= private_constant_names(self.lines(&site.path), &tail);
            // Openings that disagree: private wins.
            api_private = api_private.max(marks.api_private);
            let around = self.enclosing(&site.path, site.line);
            nested &= around.all || marks.api_private.or(around.api_private) == Some(true);
        }
        // Ruby: nothing outside reaches a name through a private constant.
        let private = private
            || fqn
                .rsplit_once("::")
                .is_some_and(|(namespace, _)| self.scope(tree, root, namespace).private);
        let scope = Scope {
            internal: marked || private || nested || api_private == Some(true),
            private,
        };
        self.scopes.insert(fqn.to_string(), scope);
        scope
    }

    /// What the openings lexically around `path:line` (`openings_around`)
    /// pass down to it: RDoc's `:nodoc: all` and YARD's `@private` reach
    /// what they hold, and the nearest `@api` tag decides. RDoc and YARD
    /// pass a mark down as they parse, so a reopening elsewhere gets it or
    /// not by file order; only the marked opening's own body is reached.
    fn enclosing(&mut self, path: &str, line: u32) -> Marks {
        let lines = self.lines(path);
        let openings = match (line as usize).checked_sub(1) {
            Some(at) if at < lines.len() => openings_around(lines, at),
            _ => Vec::new(),
        };
        let mut around = Marks::default();
        for at in openings {
            let marks = self.marks(path, at as u32 + 1);
            around.all |= marks.all;
            around.api_private = around.api_private.or(marks.api_private);
        }
        around
    }
}

/// What a definition's documentation marks it.
#[derive(Clone, Copy, Default)]
struct Marks {
    /// No API, nor the methods its body defines: `:nodoc:`, TomDoc's.
    hidden: bool,
    /// No API, nor anything its opening holds: `:nodoc: all`, YARD's
    /// `@private`.
    all: bool,
    /// YARD's `@api` tag, if written: whether it says `private`.
    api_private: Option<bool>,
}

/// What a constant's marks and its namespaces' make of it.
#[derive(Clone, Copy, Default)]
struct Scope {
    internal: bool,
    /// A `private_constant`, its own or a namespace's.
    private: bool,
}

/// The `class`, `module` and constant lines lexically around line `at`, innermost
/// first, by indentation: each is the nearest code line above indented
/// less than the last. A line out of step (a heredoc's) ends the walk
/// early, which reaches less.
fn openings_around(lines: &[String], at: usize) -> Vec<usize> {
    let indent = |text: &str| text.len() - text.trim_start().len();
    let mut width = indent(&lines[at]);
    let mut found = Vec::new();
    for n in (0..at).rev() {
        if width == 0 {
            break;
        }
        let text = lines[n].trim_start();
        // An outdented `private` sits at its class's own indentation.
        let modifier = matches!(
            code(text),
            "private" | "protected" | "public" | "module_function"
        );
        if text.is_empty() || text.starts_with('#') || modifier || indent(&lines[n]) >= width {
            continue;
        }
        width = indent(&lines[n]);
        // A constant's block opens a class too: `Item = Struct.new(:a) do`.
        let constant =
            text.starts_with(|c: char| c.is_ascii_uppercase()) && code(text).contains(" = ");
        if constant
            || text.starts_with("module ")
            || (text.starts_with("class ") && !text.starts_with("class <<"))
        {
            found.push(n);
        }
    }
    found
}

/// Where the doc comment of the definition at `at` ends: directly above it,
/// or above a Sorbet `sig` written between them, on one line or a block.
fn doc_end(lines: &[String], at: usize) -> usize {
    let indent = |text: &str| text.len() - text.trim_start().len();
    let sig = |text: &str| {
        let text = text.trim_start();
        text.strip_prefix("sig")
            .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '{', '(']))
    };
    let Some(above) = at.checked_sub(1) else {
        return at;
    };
    if sig(&lines[above]) {
        return above;
    }
    let close = lines[above].trim_start();
    if close == "end" || close.starts_with('}') {
        // The block's opening line is the nearest one indented no deeper.
        let width = indent(&lines[above]);
        let open = (0..above)
            .rev()
            .find(|&n| !lines[n].trim().is_empty() && indent(&lines[n]) <= width);
        if let Some(open) = open.filter(|&n| sig(&lines[n])) {
            return open;
        }
    }
    at
}

/// A comment line as YARD's docstring holds it: its `#`s and at most one
/// space after them dropped (`parser/ruby/ruby_parser.rb`), so a line
/// indented past that is a tag's body or prose, never a tag.
fn docstring(line: &str) -> &str {
    let text = line.trim_start().trim_start_matches('#');
    text.strip_prefix([' ', '\t']).unwrap_or(text)
}

/// The YARD tag a docstring line opens, and its text: YARD's `META_MATCH`
/// wants `@` first, then the whole name (`@private_key` is no `@private`).
fn yard_tag(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix('@')?;
    let end = rest
        .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.'))
        .unwrap_or(rest.len());
    let (name, text) = rest.split_at(end);
    (!name.is_empty() && (text.is_empty() || text.starts_with(char::is_whitespace)))
        .then_some((name, text))
}

fn code(line: &str) -> &str {
    line.split_once('#').map_or(line, |(code, _)| code).trim()
}

/// Do these lines make `name` a private constant: `private_constant :Name,
/// …`, its arguments on as many lines as its parentheses or commas carry?
fn private_constant_names(lines: &[String], name: &str) -> bool {
    lines.iter().enumerate().any(|(at, line)| {
        let Some(first) = code(line).strip_prefix("private_constant") else {
            return false;
        };
        let mut args = first.to_string();
        let mut next = at + 1;
        while (args.matches('(').count() > args.matches(')').count()
            || args.trim_end().ends_with(','))
            && next < lines.len()
        {
            args.push(' ');
            args.push_str(code(&lines[next]));
            next += 1;
        }
        args.split(|c: char| c == ',' || c == '(' || c == ')' || c.is_whitespace())
            .filter_map(|arg| arg.strip_prefix(':'))
            .any(|arg| arg == name)
    })
}

/// Does a Ruby name say it is internal by convention: a leading underscore?
pub(super) fn underscored(name: &str) -> bool {
    name.starts_with('_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(source: &str, name: &str) -> bool {
        let lines: Vec<String> = source.lines().map(str::to_string).collect();
        private_constant_names(&lines, name)
    }

    #[test]
    fn a_private_constant_call_names_each_of_its_symbols() {
        assert!(names("  private_constant :A, :Beta", "Beta"));
        assert!(names("private_constant(:A)", "A"));
        assert!(names("private_constant(\n  :A\n)", "A"));
        assert!(names("private_constant :A,\n  :B", "B"));
        assert!(!names("private_constant :A\n:B", "B"));
        assert!(!names("private_constant :Alpha", "A"));
        assert!(!names("# private_constant :A", "A"));
        assert!(!names("FOO = :A", "A"));
    }

    #[test]
    fn a_symlinked_root_gemspec_makes_a_library() {
        let root = std::env::temp_dir().join(format!("trekr-gemspec-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("lib")).unwrap();
        std::fs::create_dir_all(root.join("app")).unwrap();
        std::fs::write(root.join("real.gemspec.in"), "").unwrap();
        std::os::unix::fs::symlink(root.join("real.gemspec.in"), root.join("widget.gemspec"))
            .unwrap();
        let in_lib = Gems::default().in_gem_lib(&root, "lib/widget.rb");
        std::fs::remove_dir_all(&root).unwrap();
        assert!(
            in_lib,
            "an engine's symlinked gemspec still makes a library"
        );
    }

    #[test]
    fn the_openings_around_a_line_are_its_lexical_class_and_modules() {
        let lines = |source: &str| source.lines().map(str::to_string).collect::<Vec<_>>();
        let source = "module A\n  module B # :nodoc: all\n  end\n  X = Struct.new do\n    class << self\n      def m\n";
        assert_eq!(openings_around(&lines(source), 5), vec![3, 0]);
        let source = "module A\n  class C\n    # note\n\n    def m\n";
        assert_eq!(openings_around(&lines(source), 4), vec![1, 0]);
        let source = "module A\n  class C\n    def a; end\n  private\n    def m\n";
        assert_eq!(openings_around(&lines(source), 4), vec![1, 0]);
        assert!(openings_around(&lines("module A; end\ndef m"), 1).is_empty());
    }

    #[test]
    fn a_yard_tag_opens_its_docstring_line() {
        fn tag(line: &str) -> Option<(&str, &str)> {
            yard_tag(docstring(line))
        }
        assert_eq!(tag("  # @api private"), Some(("api", " private")));
        assert_eq!(tag("#@private"), Some(("private", "")));
        assert_eq!(
            tag("# @private_key holds it"),
            Some(("private_key", " holds it"))
        );
        assert_eq!(tag("#   # @api private"), None);
        assert_eq!(tag("#  @api private"), None);
        assert_eq!(tag("# Signs with `@private`."), None);
        assert_eq!(tag("# @private: no"), None);
    }

    #[test]
    fn a_doc_comment_ends_above_a_sig() {
        let lines = |source: &str| source.lines().map(str::to_string).collect::<Vec<_>>();
        assert_eq!(doc_end(&lines("# x\nsig { void }\ndef a"), 2), 1);
        assert_eq!(doc_end(&lines("# x\nsig do\n  void\nend\ndef a"), 4), 1);
        assert_eq!(doc_end(&lines("# x\ndef a\nend\ndef b"), 3), 3);
        assert_eq!(doc_end(&lines("# x\ndef a"), 1), 1);
    }
}
