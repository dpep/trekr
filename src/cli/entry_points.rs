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
    /// Public API: a public method, class or module under a gem's `lib/`.
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
    /// app there), and the file is in a `lib` directory beside a
    /// `*.gemspec`, the root's or a nested package's (Rails'
    /// `activerecord/`, protobuf's `ruby/`)? An app's own engines are called
    /// by the app, which is in the checkout; a test's files are no API.
    pub(super) fn in_gem_lib(&mut self, root: &Path, path: &str) -> bool {
        let relative = Path::new(path)
            .strip_prefix(root)
            .unwrap_or(Path::new(path));
        // A gem with an `app/` (an engine) has a root gemspec, checked first.
        let app = || {
            ["config/application.rb", "config.ru"]
                .iter()
                .any(|f| root.join(f).is_file())
                || root.join("app").is_dir()
        };
        if (!self.has_gemspec(root) && app())
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
            if *segment == "lib" && self.has_gemspec(&dir) {
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
                        && entry.file_type().is_ok_and(|t| t.is_file())
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
    /// `:nodoc:` on its line (RDoc), or in the comment directly above it
    /// `@api private` / `@private` (YARD) or `Private:` / `Internal:` (TomDoc).
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
        for text in lines[..doc_end(lines, at)]
            .iter()
            .rev()
            .map(|text| text.trim_start())
            .take_while(|text| text.starts_with('#'))
            .map(|text| text.trim_start_matches('#').trim_start())
        {
            // Both YARD tags are transitive: a namespace's reaches its children.
            if let Some((_, api)) = text.split_once("@api ") {
                marks.api_private = Some(api.split_whitespace().next() == Some("private"));
            }
            if text.contains("@private") {
                marks.all = true;
            }
            if text.starts_with("Private:") || text.starts_with("Internal:") {
                marks.hidden = true;
            }
        }
        marks
    }

    /// Is the method defined at `path:line` in `owner` internal: marked so
    /// itself, written in a body marked `:nodoc:` (RDoc leaves out the
    /// methods a `:nodoc:` body defines), or under a namespace whose mark
    /// reaches everything nested in it?
    pub(super) fn internal_method(
        &mut self,
        tree: &Tree,
        root: &str,
        owner: &str,
        path: &str,
        line: u32,
    ) -> bool {
        let own = self.marks(path, line);
        let scope = self.scope(tree, root, owner);
        // YARD: an `@api` tag written on the method overrides its class's.
        let api_private = own.api_private.or(scope.api_private);
        if own.hidden || own.all || scope.all || api_private == Some(true) {
            return true;
        }
        // The body it is written in: its owner's nearest opening above it.
        let body = tree
            .sites(owner)
            .into_iter()
            .filter(|site| site.path == path && site.line <= line)
            .max_by_key(|site| site.line);
        body.is_some_and(|site| self.marks(&site.path, site.line).hidden)
    }

    /// Is the constant `fqn` internal: marked so on any of its openings
    /// (RDoc's `:nodoc:` holds for the class wherever it is reopened), made
    /// a `private_constant`, or nested in a namespace whose mark reaches
    /// everything under it — `:nodoc: all`, YARD's `@api private` and
    /// `@private`, a `private_constant`? A plain `:nodoc:` does not: RDoc
    /// documents the classes nested in one.
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
        let mut own = Marks::default();
        for site in &sites {
            let marks = self.marks(&site.path, site.line);
            let private = private_constant_names(self.lines(&site.path), &tail);
            own.hidden |= marks.hidden;
            own.all |= marks.all || private;
            // Openings that disagree: private wins.
            own.api_private = own.api_private.max(marks.api_private);
        }
        let outer = fqn
            .rsplit_once("::")
            .map(|(namespace, _)| self.scope(tree, root, namespace))
            .unwrap_or_default();
        let all = own.all || outer.all;
        let api_private = own.api_private.or(outer.api_private);
        let scope = Scope {
            internal: own.hidden || all || api_private == Some(true),
            all,
            api_private,
        };
        self.scopes.insert(fqn.to_string(), scope);
        scope
    }
}

/// What a definition's documentation marks it.
#[derive(Clone, Copy, Default)]
struct Marks {
    /// No API, nor the methods its body defines: `:nodoc:`, TomDoc's.
    hidden: bool,
    /// No API, nor anything nested in it: `:nodoc: all`, YARD's `@private`,
    /// `private_constant`.
    all: bool,
    /// YARD's `@api` tag, if written: whether it says `private`.
    api_private: Option<bool>,
}

/// What a constant's marks and its namespaces' make of it.
#[derive(Clone, Copy, Default)]
struct Scope {
    internal: bool,
    /// Its marks, or a namespace's, reach everything nested in it.
    all: bool,
    /// The nearest `@api` tag, its own or a namespace's.
    api_private: Option<bool>,
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
    fn a_doc_comment_ends_above_a_sig() {
        let lines = |source: &str| source.lines().map(str::to_string).collect::<Vec<_>>();
        assert_eq!(doc_end(&lines("# x\nsig { void }\ndef a"), 2), 1);
        assert_eq!(doc_end(&lines("# x\nsig do\n  void\nend\ndef a"), 4), 1);
        assert_eq!(doc_end(&lines("# x\ndef a\nend\ndef b"), 3), 3);
        assert_eq!(doc_end(&lines("# x\ndef a"), 1), 1);
    }
}
