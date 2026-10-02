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
    /// What a definition answers when the call is unresolved
    /// (`initializationOptions.unresolved`).
    pub(crate) unresolved: Unresolved,
    /// The checkout a background index is refilling after an upgrade
    /// dropped the store, and since when: until it ends, answers there are
    /// partial, and a hover says so.
    pub(crate) reindexing: Option<(PathBuf, std::time::Instant)>,
    /// Checkouts told their navigation answers are partial (DEC-331): once
    /// each, for the life of the session.
    pub(crate) told_warming: std::collections::HashSet<PathBuf>,
    /// A first index's early store is what `store` reads now (DEC-332).
    early: Option<Early>,
    /// Each checkout's stamp, and the store's `data_version` it was read at:
    /// a request re-reads the store's roots only once something has
    /// committed since (DEC-333).
    stamps: HashMap<PathBuf, (i64, Stamp)>,
    /// Checkouts whose first index died with its early store in use: to be
    /// indexed again, as one found cut short at start is (DEC-320).
    resume: Vec<PathBuf>,
}

/// The early store being read in place of the store, and the store, kept for
/// writes and for when the early store is gone.
struct Early {
    path: PathBuf,
    main: Store,
    /// The checkout it was found for, and its index as found.
    root: PathBuf,
    writer: crate::store::Warming,
}

/// A tree being built aside, from the store as it was at `stamp`.
struct Next {
    stamp: Stamp,
    done: mpsc::Receiver<anyhow::Result<(Tree, Option<crate::store::Warming>)>>,
}

/// How long a request waits for something built aside — a partial tree's
/// successor, completion's member listing — before it answers without it.
/// A request is answered within a second (DEC-323).
const ASIDE: std::time::Duration = std::time::Duration::from_millis(400);

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
    /// The index was still filling the checkout when the tree was built
    /// (DEC-320): its answers are partial, and its successor is built aside.
    partial: Option<crate::store::Warming>,
    /// That successor, being built on another thread (DEC-323).
    next: Option<Next>,
    /// The tree's namespaces and method table, listed — what completion
    /// reads. Built on first use and dropped with the tree it came from.
    members: Option<Members>,
    /// The listing of the tree this one replaced, until its own is listed.
    stale: Option<Members>,
}

impl Checkout {
    /// Answer from `tree`, assembled from the store at `stamp`. The old
    /// tree's listing still serves completion, said to be old, until this
    /// one's is listed: at scale that is seconds, and the two differ by an
    /// edit's worth.
    fn replace(&mut self, tree: Tree, stamp: Stamp, partial: Option<crate::store::Warming>) {
        self.tree = Some(tree);
        self.built_from = Some(stamp);
        self.partial = partial;
        if let Some(members) = self.members.take() {
            self.stale = Some(members);
        }
    }

    /// The tree's members, listed.
    fn listed(&mut self, members: Members) {
        self.members = Some(members);
        self.stale = None;
    }

    /// What completion lists from, and whether it is the tree's own listing.
    fn listing(&self) -> Option<(&Members, bool)> {
        match (&self.members, &self.stale) {
            (Some(members), _) => Some((members, true)),
            (None, Some(stale)) => Some((stale, false)),
            (None, None) => None,
        }
    }
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
            unresolved: Unresolved::default(),
            reindexing: None,
            told_warming: std::collections::HashSet::new(),
            early: None,
            stamps: HashMap::new(),
            resume: Vec::new(),
        }
    }

    pub(crate) fn store(&self) -> &Store {
        &self.store
    }

    /// The store itself, never an early store: the one that can be replaced
    /// underneath the session (DEC-300). An early store's removal is its
    /// index's cleanup, and `follow_early` notices it.
    pub(crate) fn main_store(&self) -> &Store {
        match &self.early {
            Some(early) => &early.main,
            None => &self.store,
        }
    }

    /// The store, to write to: never an early store. Its own writes do not
    /// move its `data_version`, so every stamp is read again after one.
    pub(crate) fn store_mut(&mut self) -> &mut Store {
        self.stamps.clear();
        match &mut self.early {
            Some(early) => &mut early.main,
            None => &mut self.store,
        }
    }

    /// Answer from `store` in place of one that was replaced underneath the
    /// session (DEC-300). Every tree was assembled from the old one.
    pub(crate) fn replace_store(&mut self, store: Store) {
        self.store = store;
        self.early = None;
        self.stamps.clear();
        self.checkouts.clear();
        self.listing = None;
    }

    /// Checkouts whose first index died mid-session, since the last call.
    pub(crate) fn take_resume(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.resume)
    }

    /// Checkouts found unindexed since the last call.
    pub(crate) fn take_unindexed(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.unindexed)
    }

    /// This checkout's first index, while it is still filling the store
    /// (DEC-320): answers from it are partial, and say so. Asked of the tree
    /// that answers when there is one, which may be older than the store.
    pub(crate) fn warming(&self, root: &Path) -> Option<crate::store::Warming> {
        let warming = match self.checkouts.get(root) {
            Some(checkout) if checkout.tree.is_some() => checkout.partial.clone(),
            _ => self.store.warming(&root.to_string_lossy()).ok().flatten(),
        };
        warming.map(crate::store::Warming::now)
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
    ///
    /// A tree built while the index was still filling the checkout keeps
    /// answering while its successor is built on another thread, and a
    /// request waits for that at most `ASIDE` (DEC-323): at scale a build is
    /// seconds, and a first index moves the store several times.
    pub(crate) fn tree(&mut self, root: &Path) -> anyhow::Result<&Tree> {
        self.follow_early(root);
        let key = root.to_string_lossy().into_owned();
        let stamp = self.stamp(root)?;
        self.collect_tree(root, None);
        let checkout = self.checkouts.entry(root.to_path_buf()).or_default();
        // An index can end without moving the stamp — a checkout small
        // enough to be read whole ahead of the rest — and its tree must stop
        // calling itself partial.
        let finished = checkout.partial.is_some() && self.store.warming(&key)?.is_none();
        if checkout.built_from != Some(stamp) || finished {
            // Partial is normal: answer from core and gems alone, and ask for
            // the index rather than wait for it.
            if !self.store.has_checkout(&key)? && !self.unindexed.iter().any(|r| r == root) {
                self.unindexed.push(root.to_path_buf());
            }
            let aside = checkout.tree.is_some() && checkout.partial.is_some();
            let building = checkout
                .next
                .as_ref()
                .is_some_and(|next| next.stamp == stamp);
            // No second connection — an in-memory store — builds it here.
            let second = match aside {
                true => self.store.reopen().ok().flatten(),
                false => None,
            };
            match second {
                Some(_) if building => self.collect_tree(root, Some(ASIDE)),
                Some(store) => {
                    let (send, done) = mpsc::channel();
                    std::thread::spawn(move || {
                        let built = Tree::build(&store, &key)
                            .and_then(|tree| Ok((tree, store.warming(&key)?)));
                        let _ = send.send(built);
                    });
                    let checkout = self.checkouts.get_mut(root).expect("placed above");
                    checkout.next = Some(Next { stamp, done });
                    self.collect_tree(root, Some(ASIDE));
                }
                None => {
                    let tree = Tree::build(&self.store, &key)?;
                    let partial = self.store.warming(&key)?;
                    let checkout = self.checkouts.get_mut(root).expect("placed above");
                    checkout.replace(tree, stamp, partial);
                    checkout.next = None;
                }
            }
        }
        Ok(self.checkouts[root].tree.as_ref().expect("built or kept"))
    }

    /// The checkout's stamp, read again only when the store has moved since
    /// it was last read (DEC-333): reading it is most of a warm request.
    fn stamp(&mut self, root: &Path) -> anyhow::Result<Stamp> {
        let version = self.store.data_version()?;
        if let Some((at, stamp)) = self.stamps.get(root)
            && *at == version
        {
            return Ok(*stamp);
        }
        let stamp = Stamp(Tree::stamp(&self.store, &root.to_string_lossy())?);
        self.stamps.insert(root.to_path_buf(), (version, stamp));
        Ok(stamp)
    }

    /// Read a first index's early store while it is there, and the store once
    /// the index has removed it (DEC-332) — or has died, when nothing will
    /// remove it and the store holds what was saved since. A tree built from
    /// either keeps answering until its successor is built aside, as for any
    /// partial tree.
    fn follow_early(&mut self, root: &Path) {
        if let Some(early) = &self.early {
            let running = !early.writer.clone().now().interrupted;
            if running && early.path.exists() {
                return;
            }
            let early = self.early.take().expect("checked above");
            self.store = early.main;
            self.stamps.clear();
            if running {
                return;
            }
            if let Some(main) = self.store.path() {
                crate::store::early::sweep(main);
            }
            // An index that ended has cleared its mark; one that died has not.
            let cut_short = self
                .store
                .warming(&early.root.to_string_lossy())
                .ok()
                .flatten()
                .is_some_and(|warming| warming.interrupted);
            if cut_short {
                self.resume.push(early.root);
            }
            return;
        }
        let whole = self
            .checkouts
            .get(root)
            .is_some_and(|checkout| checkout.tree.is_some() && checkout.partial.is_none());
        let Some(main) = self.store.path().filter(|_| !whole) else {
            return;
        };
        let Ok(Some(warming)) = self.store.warming(&root.to_string_lossy()) else {
            return;
        };
        let path = crate::store::early::path(main, warming.pid);
        if warming.interrupted || !path.exists() {
            return;
        }
        let Ok(early) = Store::open_existing(&path) else {
            return;
        };
        let main = std::mem::replace(&mut self.store, early);
        self.early = Some(Early {
            path,
            main,
            root: root.to_path_buf(),
            writer: warming,
        });
        self.stamps.clear();
    }

    /// Put a tree built aside in place once it is done, waiting up to `wait`
    /// for it, or not at all.
    fn collect_tree(&mut self, root: &Path, wait: Option<std::time::Duration>) {
        let Some(checkout) = self.checkouts.get_mut(root) else {
            return;
        };
        let Some(next) = &checkout.next else {
            return;
        };
        let built = match wait {
            Some(wait) => next.done.recv_timeout(wait).ok(),
            None => next.done.try_recv().ok(),
        };
        let Some(built) = built else {
            return;
        };
        let next = checkout.next.take().expect("checked above");
        // A build that failed leaves the tree that answers; the next question
        // tries again.
        if let Ok((tree, partial)) = built {
            checkout.replace(tree, next.stamp, partial);
        }
    }

    /// Every tree built aside that is done, put in place: the serve loop's
    /// quiet moments.
    pub(crate) fn collect_trees(&mut self) {
        let roots: Vec<PathBuf> = self
            .checkouts
            .iter()
            .filter(|(_, checkout)| checkout.next.is_some())
            .map(|(root, _)| root.clone())
            .collect();
        for root in roots {
            self.collect_tree(&root, None);
        }
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

    /// A checkout's tree together with its listed members, for completion,
    /// and whether they are this tree's — while listing them takes longer
    /// than `ASIDE`, the last tree's, or `None` when there were none: at
    /// scale it is seconds, and the client asks again as the word grows
    /// (DEC-323).
    pub(crate) fn members(
        &mut self,
        root: &Path,
    ) -> anyhow::Result<(&Tree, Option<(&Members, bool)>)> {
        self.tree(root)?;
        if self.checkouts[root].members.is_none() {
            match self.store.path() {
                // Being listed already: waiting for it beats starting over.
                // With the last tree's listing to answer from, not at all.
                Some(_) => {
                    self.list_members(root)?;
                    let wait = match self.checkouts[root].stale {
                        Some(_) => std::time::Duration::ZERO,
                        None => ASIDE,
                    };
                    self.collect_members(Some((root, wait)));
                }
                // No second connection to list on: here, whatever it takes.
                None => {
                    let checkout = self.checkouts.get_mut(root).expect("tree() placed it");
                    let tree = checkout.tree.as_ref().expect("tree() built it");
                    let members = Members::of(tree);
                    checkout.listed(members);
                }
            }
        }
        let checkout = &self.checkouts[root];
        let tree = checkout.tree.as_ref().expect("tree() built it");
        Ok((tree, checkout.listing()))
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
    pub(crate) fn collect_members(&mut self, waiting_for: Option<(&Path, std::time::Duration)>) {
        let Some(listing) = &self.listing else {
            return;
        };
        let waited = waiting_for.filter(|(root, _)| *root == listing.root.as_path());
        let result = if let Some((_, wait)) = waited {
            match listing.done.recv_timeout(wait) {
                Ok(result) => Some(result),
                Err(mpsc::RecvTimeoutError::Timeout) => return,
                Err(mpsc::RecvTimeoutError::Disconnected) => None,
            }
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
            checkout.listed(members);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_replaced_tree_s_listing_serves_until_its_own_is_listed() {
        let store = Store::open_in_memory().unwrap();
        let tree = || Tree::build(&store, "/app").unwrap();
        let mut checkout = Checkout::default();
        checkout.replace(tree(), Stamp([0; 20]), None);
        assert!(checkout.listing().is_none(), "nothing listed yet");
        let members = Members::of(checkout.tree.as_ref().unwrap());
        checkout.listed(members);
        assert!(matches!(checkout.listing(), Some((_, true))));

        checkout.replace(tree(), Stamp([1; 20]), None);
        assert!(
            matches!(checkout.listing(), Some((_, false))),
            "the last tree's listing, said to be the last tree's"
        );
        let members = Members::of(checkout.tree.as_ref().unwrap());
        checkout.listed(members);
        assert!(matches!(checkout.listing(), Some((_, true))));
    }
}

/// What go-to-definition returns for a residue — a call trekr could not
/// resolve, whose ranked candidates it still has (DEC-443). An editor shows
/// a location the same whether it was resolved or guessed, so this is where
/// the guess is let through or held back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Unresolved {
    /// Every candidate, best first, as a peek list.
    Peek,
    /// The first candidate only.
    Best,
    /// The candidates, when the first's confidence is at least
    /// [`Unresolved::THRESHOLD`]; otherwise nothing.
    #[default]
    Confident,
    /// Nothing.
    None,
}

impl Unresolved {
    /// Between the two grades a residue's first candidate gets (DEC-442).
    pub(crate) const THRESHOLD: f64 = 0.5;

    /// The setting as the client spells it; anything else is the default.
    pub(crate) fn parse(value: Option<&str>) -> Unresolved {
        match value {
            Some("peek") => Unresolved::Peek,
            Some("best") => Unresolved::Best,
            Some("confident") => Unresolved::Confident,
            Some("none") => Unresolved::None,
            _ => Unresolved::default(),
        }
    }

    /// How many of a residue's candidates to return, given its confidence.
    pub(crate) fn keep(self, confidence: f64) -> usize {
        match self {
            Unresolved::Peek => usize::MAX,
            Unresolved::Best => 1,
            Unresolved::Confident if confidence >= Self::THRESHOLD => usize::MAX,
            Unresolved::Confident | Unresolved::None => 0,
        }
    }
}
