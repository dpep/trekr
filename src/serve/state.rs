//! What a resident process holds on to.
//!
//! The engine is daemon-free: state lives on disk and any process can answer
//! (PLAN §4). A resident front does not own that state — it caches it. Two
//! things are worth caching, and the measurements say so:
//!
//! * **the assembled tree**, 210 ms on rails and 314 ms on discourse, rebuilt
//!   from SQL on every CLI invocation today;
//! * **the parse of an open file**, which `--def` and `--refs` both redo.
//!
//! A `--refs` query pays both — 360–400 ms, of which the tree is over half —
//! which is the whole economic case for this module.

use crate::core::Facts;
use crate::store::Store;
use crate::tree::Tree;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One workspace's cached state.
pub(crate) struct Workspace {
    pub(crate) root: PathBuf,
    store: Store,
    /// Rebuilt when the checkout's blob set moves. `None` until first asked, so
    /// a client that only opens a file never pays for it.
    tree: Option<Tree>,
    /// What the tree was built against — the store's schema version and the
    /// checkout's file count. Cheap to re-read, and both change exactly when
    /// the facts do.
    built_from: Option<Stamp>,
    /// Open documents, by URI path. The editor's copy, which may be newer than
    /// disk, so it wins.
    open: HashMap<String, Document>,
}

/// A file the editor has open, and its parse.
pub(crate) struct Document {
    pub(crate) text: String,
    facts: Option<Facts>,
}

impl Document {
    pub(crate) fn new(text: String) -> Document {
        Document { text, facts: None }
    }

    /// Prism's syntax errors for this document.
    pub(crate) fn parse_errors(&self) -> Vec<(u32, u32, String)> {
        crate::extract::syntax_errors(self.text.as_bytes())
    }

    /// The parse, made once per edit rather than once per query.
    pub(crate) fn facts(&mut self) -> &Facts {
        self.facts
            .get_or_insert_with(|| crate::extract::extract(self.text.as_bytes()))
    }
}

/// A cheap fingerprint of what the tree was assembled from.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    version: i64,
    files: i64,
}

impl Workspace {
    pub(crate) fn open(root: PathBuf, store: Store) -> Workspace {
        Workspace {
            root,
            store,
            tree: None,
            built_from: None,
            open: HashMap::new(),
        }
    }

    pub(crate) fn store(&self) -> &Store {
        &self.store
    }

    /// The assembled namespace, rebuilt only when the index beneath it moved.
    ///
    /// DEC-007 chose whole rebuilds over incremental patching and DEC-013 made
    /// the store version cover the extractor; between them, "has anything
    /// changed" is two integers.
    pub(crate) fn tree(&mut self) -> anyhow::Result<&Tree> {
        let root = self.root.to_string_lossy().into_owned();
        let stamp = Stamp {
            version: self.store.schema_version()?,
            files: self.store.file_count(&root)?,
        };
        if self.built_from != Some(stamp) {
            self.tree = Some(Tree::build(&self.store, &root)?);
            self.built_from = Some(stamp);
        }
        Ok(self.tree.as_ref().expect("just built"))
    }

    pub(crate) fn did_open(&mut self, path: String, text: String) {
        self.open.insert(path, Document::new(text));
    }

    pub(crate) fn did_change(&mut self, path: String, text: String) {
        self.open.insert(path, Document::new(text));
    }

    pub(crate) fn did_close(&mut self, path: &str) {
        self.open.remove(path);
    }

    /// The editor's copy if it has one, else what is on disk. The editor's copy
    /// is the one the user is looking at.
    pub(crate) fn document(&mut self, path: &str) -> Option<&mut Document> {
        if !self.open.contains_key(path) {
            let text = std::fs::read_to_string(self.root.join(path)).ok()?;
            self.open.insert(path.to_string(), Document::new(text));
        }
        self.open.get_mut(path)
    }

    /// A workspace-relative path for a file URI.
    ///
    /// Both sides are canonicalized before comparing: the store keys checkouts
    /// on git's real path, and an editor sends the one the user typed — which
    /// on macOS differ by `/var` being a symlink to `/private/var`. Comparing
    /// them textually silently matches nothing.
    pub(crate) fn relative(&self, path: &Path) -> Option<String> {
        let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        real.strip_prefix(&self.root)
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
    }
}
