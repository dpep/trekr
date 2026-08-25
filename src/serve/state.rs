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
//!
//! The unit is a **checkout**, not the client's workspace. An agent asks about
//! a file wherever that file lives, and the client's root is routinely another
//! repo — or, for Claude Code, whichever directory the session happened to
//! start in. So the session holds a tree per checkout and finds the one a file
//! belongs to (DEC-024).

use crate::core::Facts;
use crate::store::Store;
use crate::tree::Tree;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One LSP conversation: the store, a tree per checkout it has been asked
/// about, and the documents the editor has open.
pub(crate) struct Session {
    /// The client's own root, from `initialize`. Only a workspace-wide
    /// question consults it; a question about a file consults that file's
    /// checkout instead.
    pub(crate) root: PathBuf,
    store: Store,
    checkouts: HashMap<PathBuf, Checkout>,
    /// Which repository a directory belongs to. `repo_root` forks `git`, so a
    /// miss is expensive and a keystroke must not pay it twice. A directory
    /// outside any repository caches as `None`, so we do not re-ask.
    enclosing: HashMap<PathBuf, Option<PathBuf>>,
    /// Open documents by canonical absolute path — two checkouts can each have
    /// an `app.rb`, so a relative key is not a key.
    open: HashMap<PathBuf, Document>,
}

/// One checkout's assembled namespace, and what it was assembled from.
#[derive(Default)]
struct Checkout {
    /// `None` until first asked, so a client that only opens a file never pays
    /// for it.
    tree: Option<Tree>,
    /// What the tree was assembled from. Cheap to re-read, and it moves
    /// exactly when the assembled tree would differ.
    built_from: Option<Stamp>,
}

/// A file, placed in the checkout that owns it.
pub(crate) struct Located {
    pub(crate) root: PathBuf,
    /// The path as the index knows it: relative to `root`.
    pub(crate) relative: String,
    pub(crate) absolute: PathBuf,
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

/// A cheap fingerprint of what a tree was assembled from: the store's schema
/// version (which DEC-013 makes cover the extractor) and the checkout's
/// surface key.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    version: i64,
    surface: i64,
}

impl Session {
    pub(crate) fn open(root: PathBuf, store: Store) -> Session {
        Session {
            root,
            store,
            checkouts: HashMap::new(),
            enclosing: HashMap::new(),
            open: HashMap::new(),
        }
    }

    pub(crate) fn store(&self) -> &Store {
        &self.store
    }

    /// The checkout a file belongs to, and its path within it.
    ///
    /// Both sides are canonicalized before comparing: the store keys checkouts
    /// on git's real path, and an editor sends the one the user typed — which
    /// on macOS differ by `/var` being a symlink to `/private/var`. Comparing
    /// them textually silently matches nothing.
    pub(crate) fn locate(&mut self, path: &Path) -> Option<Located> {
        let absolute = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let directory = absolute.parent()?.to_path_buf();
        let root = match self.enclosing.get(&directory) {
            Some(cached) => cached.clone(),
            None => {
                let found = crate::scan::repo_root(&absolute)
                    .ok()
                    .map(|root| std::fs::canonicalize(&root).unwrap_or(root));
                self.enclosing.insert(directory, found.clone());
                found
            }
        }?;
        let relative = absolute.strip_prefix(&root).ok()?.to_string_lossy();
        Some(Located {
            relative: relative.into_owned(),
            absolute,
            root,
        })
    }

    /// A checkout's assembled namespace, rebuilt only when the index beneath it
    /// moved.
    ///
    /// DEC-007 chose whole rebuilds over incremental patching; what decides
    /// *whether* to rebuild is the checkout's surface key, which folds every
    /// file's path and tree-relevant facts into one number at index time. The
    /// file count it replaced could not see an edit at all.
    pub(crate) fn tree(&mut self, root: &Path) -> anyhow::Result<&Tree> {
        let key = root.to_string_lossy().into_owned();
        let stamp = Stamp {
            version: self.store.schema_version()?,
            surface: self.store.surface_key(&key)?,
        };
        let checkout = self.checkouts.entry(root.to_path_buf()).or_default();
        if checkout.built_from != Some(stamp) {
            checkout.tree = Some(Tree::build(&self.store, &key)?);
            checkout.built_from = Some(stamp);
        }
        Ok(checkout.tree.as_ref().expect("just built"))
    }

    pub(crate) fn did_open(&mut self, path: PathBuf, text: String) {
        self.open.insert(path, Document::new(text));
    }

    pub(crate) fn did_close(&mut self, path: &Path) {
        self.open.remove(path);
    }

    /// The editor's copy if it has one, else what is on disk. The editor's copy
    /// is the one the user is looking at.
    pub(crate) fn document(&mut self, path: &Path) -> Option<&mut Document> {
        if !self.open.contains_key(path) {
            let text = std::fs::read_to_string(path).ok()?;
            self.open.insert(path.to_path_buf(), Document::new(text));
        }
        self.open.get_mut(path)
    }
}
