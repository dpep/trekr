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

use super::complete::Members;
use super::require::{self, LoadPath, Require};
use super::vars::{self, Vars};
use crate::core::Facts;
use crate::store::Store;
use crate::tree::Tree;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

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
    /// Checkouts asked about that the store has never indexed. Drained by the
    /// serve loop into a background index; a question never waits for one.
    unindexed: Vec<PathBuf>,
    /// A checkout's members being listed on another thread (`list_members`).
    listing: Option<Listing>,
    /// Each checkout's load path, and the gem roots it was built from — it is
    /// rebuilt when the bundle moves, and otherwise never re-listed.
    load_paths: HashMap<PathBuf, (Vec<String>, LoadPath)>,
    /// The client takes `LocationLink`s from `definition`, which is what lets
    /// a whole `require` string be the thing clicked.
    pub(crate) definition_links: bool,
    /// How many references an answer keeps (`initializationOptions.referenceLimit`).
    pub(crate) reference_limit: usize,
    /// The checkout a background index is refilling after an upgrade
    /// dropped the store, and since when: until it ends, answers there are
    /// partial, and a hover says so.
    pub(crate) reindexing: Option<(PathBuf, std::time::Instant)>,
}

/// Members in the making, and the tree state they are being listed from.
struct Listing {
    root: PathBuf,
    stamp: Stamp,
    done: mpsc::Receiver<anyhow::Result<Members>>,
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
    /// The tree's namespaces and method table, listed — what completion
    /// reads. Built on first use and dropped with the tree it came from.
    members: Option<Members>,
}

/// A file, placed in the checkout that owns it.
pub(crate) struct Located {
    pub(crate) root: PathBuf,
    /// The path as the index knows it: relative to `root`, or absolute for a
    /// gem's file answered from an app (`locate_query`).
    pub(crate) relative: String,
    pub(crate) absolute: PathBuf,
}

/// A file's text and its parse — either the editor's copy or a read of disk.
pub(crate) struct Document {
    pub(crate) text: String,
    facts: Option<Facts>,
    requires: Option<Vec<Require>>,
    vars: Option<std::rc::Rc<Vars>>,
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
            requires: None,
            vars: None,
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

    /// The file's `require`s whose path is written literally — its own parse,
    /// since facts do not keep string arguments; also once per edit.
    pub(crate) fn requires(&mut self) -> &[Require] {
        self.requires
            .get_or_insert_with(|| require::requires_in(self.text.as_bytes()))
    }

    /// Its variables and what each read of a local can see — once per edit,
    /// and shared, since an ivar answer reads several files' at once.
    /// A string of code the file evaluates contributes its locals, where
    /// its bytes are the file's (DEC-167).
    pub(crate) fn vars(&mut self) -> std::rc::Rc<Vars> {
        if let Some(vars) = &self.vars {
            return vars.clone();
        }
        let mut vars = vars::analyze(self.text.as_bytes());
        let strings = self.facts().strings.clone();
        for string in strings {
            vars.absorb(vars::analyze(&string.src), |span| string.place(span));
        }
        let vars = std::rc::Rc::new(vars);
        self.vars = Some(vars.clone());
        vars
    }
}

/// A cheap fingerprint of what a tree was assembled from — the key its
/// snapshot is filed under: the store's schema version (which DEC-013 makes
/// cover the extractor), and the surface key of the checkout *and of every gem
/// its bundle names*. The checkout's alone missed a bundle moving to another
/// gem version, which changes no Ruby file in the checkout (DEC-065).
#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp([u8; 20]);

impl Session {
    pub(crate) fn open(root: PathBuf, store: Store) -> Session {
        Session {
            root,
            store,
            checkouts: HashMap::new(),
            enclosing: HashMap::new(),
            open: HashMap::new(),
            unindexed: Vec::new(),
            listing: None,
            load_paths: HashMap::new(),
            definition_links: false,
            reference_limit: super::gather::DEFAULT_LIMIT,
            reindexing: None,
        }
    }

    pub(crate) fn store(&self) -> &Store {
        &self.store
    }

    pub(crate) fn store_mut(&mut self) -> &mut Store {
        &mut self.store
    }

    /// Answer from `store` in place of one that was replaced underneath the
    /// session (DEC-300). Every tree was assembled from the old one.
    pub(crate) fn replace_store(&mut self, store: Store) {
        self.store = store;
        self.checkouts.clear();
        self.listing = None;
    }

    /// Checkouts found unindexed since the last call.
    pub(crate) fn take_unindexed(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.unindexed)
    }

    /// This checkout's first index, while it is still filling the store
    /// (DEC-320): answers from it are partial, and say so.
    pub(crate) fn warming(&self, root: &Path) -> Option<crate::store::Warming> {
        self.store.warming(&root.to_string_lossy()).ok().flatten()
    }

    /// Is this checkout in the store at all?
    pub(crate) fn indexed(&self, root: &Path) -> bool {
        self.store
            .has_checkout(&root.to_string_lossy())
            .unwrap_or(false)
    }

    /// The checkout a file belongs to, and its path within it. `None` for a
    /// file outside it — a gem's, which is answered from an app but is not
    /// that app's to write (see `locate_query`).
    ///
    /// Both sides are canonicalized before comparing: the store keys checkouts
    /// on git's real path, and an editor sends the one the user typed — which
    /// on macOS differ by `/var` being a symlink to `/private/var`. Comparing
    /// them textually silently matches nothing.
    pub(crate) fn locate(&mut self, path: &Path) -> Option<Located> {
        let (root, absolute) = self.answering(path)?;
        let relative = absolute.strip_prefix(&root).ok()?.to_string_lossy();
        Some(Located {
            relative: relative.into_owned(),
            absolute,
            root,
        })
    }

    /// Where a question about a file is answered: `locate`, and for a gem's
    /// file the app whose bundle holds it, with the path absolute — as the
    /// tree names a gem's sites.
    pub(crate) fn locate_query(&mut self, path: &Path) -> Option<Located> {
        let (root, absolute) = self.answering(path)?;
        let relative = match absolute.strip_prefix(&root) {
            Ok(inside) => inside.to_string_lossy().into_owned(),
            Err(_) => absolute.to_string_lossy().into_owned(),
        };
        Some(Located {
            relative,
            absolute,
            root,
        })
    }

    /// The checkout that answers for a file, and the file's canonical path.
    fn answering(&mut self, path: &Path) -> Option<(PathBuf, PathBuf)> {
        let absolute = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let directory = absolute.parent()?.to_path_buf();
        let root = match self.enclosing.get(&directory) {
            Some(cached) => cached.clone(),
            None => {
                let gem = self
                    .store
                    .gem_containing(&absolute.to_string_lossy())
                    .ok()
                    .flatten();
                let found = match gem {
                    // A gem on its own is a tree of one gem plus core, so a
                    // position inside it is answered from an app whose bundle
                    // has the rest (DEC-029). Asked before git: a git gem's
                    // checkout is a clone of its own (DEC-150).
                    Some(gem) => Some(PathBuf::from(self.app_for_gem(gem))),
                    None => match crate::scan::repo_root(&absolute) {
                        Ok(root) => Some(std::fs::canonicalize(&root).unwrap_or(root)),
                        // Indexed, not a git repository, and not a gem: its
                        // own context.
                        Err(_) => self
                            .store
                            .checkout_containing(&absolute.to_string_lossy())
                            .ok()
                            .flatten()
                            .map(PathBuf::from),
                    },
                };
                self.enclosing.insert(directory, found.clone());
                found
            }
        }?;
        Some((root, absolute))
    }

    /// The app a gem's file is answered from: the workspace's own when its
    /// bundle holds the gem, since that is the app the person is working in,
    /// and otherwise the store's pick (DEC-029). The gem itself when no app has
    /// it.
    fn app_for_gem(&self, gem: String) -> String {
        let root = std::fs::canonicalize(&self.root).unwrap_or_else(|_| self.root.clone());
        let root = root.to_string_lossy().into_owned();
        // The workspace is usually the checkout itself, which `containing`
        // does not count as containing.
        let workspace = match self.store.has_checkout(&root) {
            Ok(true) => Some(root),
            _ => self.store.checkout_containing(&root).ok().flatten(),
        };
        if let Some(app) = workspace
            && self
                .store
                .gems_used(&app)
                .is_ok_and(|gems| gems.contains(&gem))
        {
            return app;
        }
        self.store.app_for_gem(&gem).ok().flatten().unwrap_or(gem)
    }

    /// A checkout's assembled namespace, rebuilt only when the index beneath it
    /// moved.
    ///
    /// DEC-007 chose whole rebuilds over incremental patching; what decides
    /// *whether* to rebuild is the tree's stamp, which folds every root's
    /// surface key — each file's path and tree-relevant facts, as one number
    /// per checkout at index time — with its snapshot's key. A rebuild after
    /// a method edit maps the same snapshot again (DEC-194). The file count
    /// it replaced could not see an edit at all.
    pub(crate) fn tree(&mut self, root: &Path) -> anyhow::Result<&Tree> {
        let key = root.to_string_lossy().into_owned();
        let stamp = Stamp(Tree::stamp(&self.store, &key)?);
        let checkout = self.checkouts.entry(root.to_path_buf()).or_default();
        if checkout.built_from != Some(stamp) {
            // Partial is normal: answer from core and gems alone, and ask for
            // the index rather than wait for it.
            if !self.store.has_checkout(&key)? && !self.unindexed.iter().any(|r| r == root) {
                self.unindexed.push(root.to_path_buf());
            }
            checkout.tree = Some(Tree::build(&self.store, &key)?);
            checkout.built_from = Some(stamp);
            checkout.members = None;
        }
        Ok(checkout.tree.as_ref().expect("just built"))
    }

    /// The tree and the store together, for a scan that reads the index as
    /// it goes while consulting the tree.
    pub(crate) fn tree_and_store(&mut self, root: &Path) -> anyhow::Result<(&Tree, &Store)> {
        self.tree(root)?;
        let tree = self.checkouts[root].tree.as_ref().expect("just built");
        Ok((tree, &self.store))
    }

    /// A checkout's load path, as `require` searches it — built once and
    /// kept until the checkout's bundle moves. Outside a checkout it is empty,
    /// and only a path relative to the requiring file resolves.
    pub(crate) fn load_path(&mut self, root: Option<&Path>) -> &LoadPath {
        static NONE: LoadPath = LoadPath { dirs: Vec::new() };
        let Some(root) = root else {
            return &NONE;
        };
        let gems = self
            .store
            .gems_used(&root.to_string_lossy())
            .unwrap_or_default();
        let current = self
            .load_paths
            .get(root)
            .is_some_and(|(from, _)| *from == gems);
        if !current {
            let stdlib = self
                .store
                .tree_roots(&root.to_string_lossy())
                .ok()
                .and_then(|roots| roots.stdlib);
            let built = LoadPath::for_checkout(root, &gems, stdlib.as_deref());
            self.load_paths.insert(root.to_path_buf(), (gems, built));
        }
        &self.load_paths[root].1
    }

    /// A checkout's tree together with its listed members, for completion.
    pub(crate) fn members(&mut self, root: &Path) -> anyhow::Result<(&Tree, &Members)> {
        self.tree(root)?;
        // Being listed already: waiting for it beats starting over.
        self.collect_members(Some(root));
        let checkout = self.checkouts.get_mut(root).expect("tree() just placed it");
        let tree = checkout.tree.as_ref().expect("tree() just built it");
        if checkout.members.is_none() {
            checkout.members = Some(Members::of(tree));
        }
        Ok((tree, checkout.members.as_ref().expect("just built")))
    }

    /// Start listing a checkout's members on another thread, so the idle
    /// moment that prepares completion does not hold up the next request.
    ///
    /// Listing reads every method in the checkout — a third of a second on
    /// discourse — which on the serve loop's thread was time that any request
    /// arriving meanwhile waited. The worker assembles its own tree from its
    /// own connection to list from, and hands back only the listing: its tree
    /// is dropped there, off the thread that answers requests.
    pub(crate) fn list_members(&mut self, root: &Path) -> anyhow::Result<()> {
        self.tree(root)?;
        let checkout = &self.checkouts[root];
        let stamp = checkout.built_from.expect("tree() just stamped it");
        let listing = self
            .listing
            .as_ref()
            .is_some_and(|l| l.root == root && l.stamp == stamp);
        if checkout.members.is_some() || listing {
            return Ok(());
        }
        // An in-memory store has no second connection; it lists on demand.
        let Some(store) = self.store.reopen()? else {
            return Ok(());
        };
        let key = root.to_string_lossy().into_owned();
        let (send, done) = mpsc::channel();
        std::thread::spawn(move || {
            let listed = Tree::build(&store, &key).map(|tree| Members::of(&tree));
            let _ = send.send(listed);
        });
        self.listing = Some(Listing {
            root: root.to_path_buf(),
            stamp,
            done,
        });
        Ok(())
    }

    /// Take a finished listing, if there is one — or wait for it, when it is
    /// the checkout asked about. A listing made from a tree that has since
    /// moved is dropped.
    pub(crate) fn collect_members(&mut self, waiting_for: Option<&Path>) {
        let Some(listing) = &self.listing else {
            return;
        };
        let result = if waiting_for == Some(listing.root.as_path()) {
            listing.done.recv().ok()
        } else {
            match listing.done.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => None,
            }
        };
        let listing = self.listing.take().expect("checked above");
        if let (Some(Ok(members)), Some(checkout)) = (result, self.checkouts.get_mut(&listing.root))
            && checkout.built_from == Some(listing.stamp)
        {
            checkout.members = Some(members);
        }
    }

    /// The editor's copy of a file, replacing whatever was held for it. Used
    /// for both open and change: sync is FULL, so each carries the whole text.
    pub(crate) fn did_open(&mut self, path: PathBuf, text: String, version: i32) {
        self.open
            .insert(path, Document::new(text, Origin::Editor { version }));
    }

    /// The files the editor has open.
    pub(crate) fn open_paths(&self) -> impl Iterator<Item = &Path> {
        self.open
            .iter()
            .filter(|(_, document)| document.version().is_some())
            .map(|(path, _)| path.as_path())
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

    /// The editor's copies, for a successor process to restore.
    pub(crate) fn editor_buffers(&self) -> Vec<super::reload::Buffer> {
        self.open
            .iter()
            .filter_map(|(path, d)| match d.origin {
                Origin::Editor { version } => Some(super::reload::Buffer {
                    path: path.clone(),
                    version,
                    text: d.text.clone(),
                }),
                Origin::Disk { .. } => None,
            })
            .collect()
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
