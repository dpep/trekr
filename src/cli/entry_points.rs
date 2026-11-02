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
    /// A public method, class or module under a gem's `lib/`.
    GemApi,
    /// A controller action a route reaches, or a controller only routes name.
    Route,
    /// A background job's class.
    Job,
}

impl EntryPoint {
    /// What the row is, how it is reached, and what would settle it.
    pub(super) fn caveat(self, method: bool) -> String {
        let (what, how, evidence) = match (self, method) {
            (Self::GemApi, _) => (
                "public API of a gem",
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
    /// A constant's whole name → whether it or a namespace holding it is internal.
    internal: HashMap<String, bool>,
}

impl Gems {
    /// Is `path` (absolute, or relative to `root`) in a published gem's
    /// `lib/`: the checkout is a gem (a `*.gemspec` at its root), and the
    /// file is in a `lib` directory beside a `*.gemspec`, the root's or a
    /// nested package's (Rails' `activerecord/`)? An app's own engines are
    /// called by the app, which is in the checkout; a test's files are no API.
    pub(super) fn in_gem_lib(&mut self, root: &Path, path: &str) -> bool {
        let relative = Path::new(path)
            .strip_prefix(root)
            .unwrap_or(Path::new(path));
        if !self.has_gemspec(root) || super::dead_consts::in_tests(&relative.to_string_lossy()) {
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

    /// Does the definition at `path:line` say it is no public API: `:nodoc:`
    /// on its line (RDoc), or in the comment directly above it `@api
    /// private` / `@private` (YARD) or `Private:` / `Internal:` (TomDoc)?
    pub(super) fn marked_internal(&mut self, path: &str, line: u32) -> bool {
        let lines = self.lines(path);
        let Some(at) = (line as usize)
            .checked_sub(1)
            .filter(|at| *at < lines.len())
        else {
            return false;
        };
        let comment = |text: &str| text.split_once('#').map(|(_, c)| c.to_string());
        if comment(&lines[at]).is_some_and(|c| c.contains(":nodoc:")) {
            return true;
        }
        lines[..at]
            .iter()
            .rev()
            .map(|text| text.trim_start())
            .take_while(|text| text.starts_with('#'))
            .map(|text| text.trim_start_matches('#').trim_start())
            .any(|text| {
                text.contains("@api private")
                    || text.contains("@private")
                    || text.starts_with("Private:")
                    || text.starts_with("Internal:")
            })
    }

    /// Is the constant `fqn`, or a namespace holding it, internal: marked so
    /// where it is declared, or made a `private_constant` in a file that
    /// declares it?
    pub(super) fn internal_constant(&mut self, tree: &Tree, root: &str, fqn: &str) -> bool {
        if let Some(known) = self.internal.get(fqn) {
            return *known;
        }
        let prefix = format!("{root}/");
        let tail = fqn.rsplit("::").next().unwrap_or(fqn).to_string();
        let sites: Vec<_> = tree
            .sites(fqn)
            .into_iter()
            .filter(|site| site.path.starts_with(&prefix))
            .collect();
        let own = sites.iter().any(|site| {
            self.marked_internal(&site.path, site.line)
                || self
                    .lines(&site.path)
                    .iter()
                    .any(|text| private_constant_names(text, &tail))
        });
        let internal = own
            || fqn
                .rsplit_once("::")
                .is_some_and(|(namespace, _)| self.internal_constant(tree, root, namespace));
        self.internal.insert(fqn.to_string(), internal);
        internal
    }
}

/// Does `line` make `name` a private constant: `private_constant :Name, …`?
fn private_constant_names(line: &str, name: &str) -> bool {
    let code = line.split_once('#').map_or(line, |(code, _)| code).trim();
    let Some(args) = code.strip_prefix("private_constant") else {
        return false;
    };
    args.split(|c: char| c == ',' || c == '(' || c == ')' || c.is_whitespace())
        .filter_map(|arg| arg.strip_prefix(':'))
        .any(|arg| arg == name)
}

/// Does a Ruby name say it is internal by convention: a leading underscore?
pub(super) fn underscored(name: &str) -> bool {
    name.starts_with('_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_private_constant_line_names_each_of_its_symbols() {
        assert!(private_constant_names(
            "  private_constant :A, :Beta",
            "Beta"
        ));
        assert!(private_constant_names("private_constant(:A)", "A"));
        assert!(!private_constant_names("private_constant :Alpha", "A"));
        assert!(!private_constant_names("# private_constant :A", "A"));
        assert!(!private_constant_names("FOO = :A", "A"));
    }
}
