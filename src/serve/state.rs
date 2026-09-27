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

/// A file's text and its parse — either the editor's copy or a read of disk.
pub(crate) struct Document {
    pub(crate) text: String,
    facts: Option<Facts>,
    /// Where the text came from. The editor's copy is authoritative until it
    /// closes the file; a disk read is only as good as the file it was read
    /// from, and is re-read the moment that file changes.
    origin: Origin,
}

#[derive(Clone, Copy, PartialEq)]
enum Origin {
    /// The editor sent it, at this version.
    Editor { version: i32 },
    /// Read from disk, when the file had this modification time and length.
    Disk {
        modified: std::time::SystemTime,
        len: u64,
    },
}

/// How many disk reads to keep. An agent that walks a codebase would
/// otherwise pin every file it ever asked about in memory for the life of the
/// session; past this, the whole disk cache is dropped and rebuilt on demand,
/// which costs one read per file and nothing else.
const DISK_CACHE: usize = 256;

impl Document {
    fn new(text: String, origin: Origin) -> Document {
        Document {
            text,
            facts: None,
            origin,
        }
    }

    /// The editor's version, when this is the editor's copy.
    pub(crate) fn version(&self) -> Option<i32> {
        match self.origin {
            Origin::Editor { version } => Some(version),
            Origin::Disk { .. } => None,
        }
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
                let found = match crate::scan::repo_root(&absolute) {
                    Ok(root) => Some(std::fs::canonicalize(&root).unwrap_or(root)),
                    // A gem is an indexed checkout but not a git repository,
                    // and following a definition into gem source and asking
                    // again is the next thing an agent does.
                    // A gem on its own is a tree of one gem plus core, so a
                    // position inside it is answered from an app whose bundle
                    // has the rest (DEC-029). Without such an app the gem is
                    // still its own context.
                    Err(_) => self
                        .store
                        .checkout_containing(&absolute.to_string_lossy())
                        .ok()
                        .flatten()
                        .map(|gem| self.store.app_for_gem(&gem).ok().flatten().unwrap_or(gem))
                        .map(PathBuf::from),
                };
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

    /// The editor's copy of a file, replacing whatever was held for it. Used
    /// for both open and change: sync is FULL, so each carries the whole text.
    pub(crate) fn did_open(&mut self, path: PathBuf, text: String, version: i32) {
        self.open
            .insert(path, Document::new(text, Origin::Editor { version }));
    }

    pub(crate) fn did_close(&mut self, path: &Path) {
        self.open.remove(path);
    }

    /// The editor's copy if it has one, else what is on disk. The editor's copy
    /// is the one the user is looking at.
    ///
    /// A disk read is kept only while the file is unchanged. It used to be kept
    /// forever, which served an agent the file as it was the first time it
    /// asked — after the agent itself had edited it.
    pub(crate) fn document(&mut self, path: &Path) -> Option<&mut Document> {
        let fresh = match self.open.get(path).map(|d| d.origin) {
            Some(Origin::Editor { .. }) => true,
            Some(Origin::Disk { modified, len }) => {
                disk_stamp(path).is_some_and(|now| now == (modified, len))
            }
            None => false,
        };
        if !fresh {
            let (modified, len) = disk_stamp(path)?;
            let text = std::fs::read_to_string(path).ok()?;
            let disk = self
                .open
                .values()
                .filter(|d| matches!(d.origin, Origin::Disk { .. }))
                .count();
            if disk >= DISK_CACHE {
                self.open
                    .retain(|_, d| matches!(d.origin, Origin::Editor { .. }));
            }
            self.open.insert(
                path.to_path_buf(),
                Document::new(text, Origin::Disk { modified, len }),
            );
        }
        self.open.get_mut(path)
    }

    /// The editor's text for a file, if the editor has it open — and only
    /// then. A question that reads many files from disk asks this first, so an
    /// unsaved edit is answered as the user sees it.
    pub(crate) fn editor_text(&self, path: &Path) -> Option<&str> {
        self.open
            .get(path)
            .filter(|d| matches!(d.origin, Origin::Editor { .. }))
            .map(|d| d.text.as_str())
    }

    /// Every file the editor has open under `root`, with its text.
    pub(crate) fn editor_documents_under(&self, root: &Path) -> Vec<(PathBuf, String)> {
        self.open
            .iter()
            .filter(|(path, d)| matches!(d.origin, Origin::Editor { .. }) && path.starts_with(root))
            .map(|(path, d)| (path.clone(), d.text.clone()))
            .collect()
    }
}

fn disk_stamp(path: &Path) -> Option<(std::time::SystemTime, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}
