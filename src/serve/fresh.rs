//! Keeping the index current while the editor is open.
//!
//! Two mechanisms, sized to what changed:
//!
//! * **One file saved** — the file's facts are brought up to date in place,
//!   the same bounded refresh a CLI query does for the file it asks about
//!   (DEC-035). One hash, at most one parse, one transaction.
//! * **A checkout never indexed, or many files changed at once** (a branch
//!   switch, a pull) — a `trekr --index` child process, reported as progress.
//!
//! The child is an ordinary index run with a bounded lifetime, not a daemon:
//! it writes the same store any CLI invocation writes, through SQLite's own
//! locking, and exits when it is done. The server never waits on it — answers
//! come from whatever the store holds, and the tree is rebuilt on the next
//! question once the child's writes move the checkout's surface key (DEC-039).

use super::log::Log;
use super::state::Session;
use lsp_server::{Message, Notification, Request, RequestId};
use std::collections::{HashSet, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};

/// More changed files than this in one batch is an operation — a checkout, a
/// rebase — and is handed to a full index rather than refreshed one by one.
pub(crate) const BULK: usize = 32;

/// How often a refresh that met a busy index is tried again.
const RETRY: std::time::Duration = std::time::Duration::from_millis(250);

/// How a refresh ended.
enum Refreshed {
    /// Written, or nothing to write.
    Done,
    /// Another process holds the write lock: try again.
    Busy,
    /// The store was rebuilt for another schema since this server opened it.
    /// Writing would put this build's facts into a store that is not its
    /// format, so it stops until a restart or hot reload replaces it.
    Refused(String),
}

/// Bring one saved file's facts up to date.
///
/// Unconditional, unlike the CLI's probe-gated refresh: a save is the editor
/// telling us the file changed, so there is nothing to probe for.
fn refresh(session: &mut Session, path: &Path) -> Refreshed {
    let Some(located) = session.locate(path) else {
        return Refreshed::Done;
    };
    let root = located.root.to_string_lossy().into_owned();
    if !session.store().has_checkout(&root).unwrap_or(false) {
        return Refreshed::Done;
    }
    let Ok(bytes) = std::fs::read(&located.absolute) else {
        return Refreshed::Done;
    };
    let oid = crate::scan::hash_blob(&bytes);
    let known = session.store().has_blob(&oid).unwrap_or(false);
    // Parse only a blob the store has never seen; the common save-after-undo
    // is bytes it already has, which costs one hash.
    let facts = (!known).then(|| crate::extract::extract(&bytes));
    match session
        .store_mut()
        .refresh_file(&root, &located.relative, &oid, facts.as_ref())
    {
        Err(error) if crate::store::is_busy(&error) => Refreshed::Busy,
        Err(error) if crate::store::is_schema_mismatch(&error) => {
            Refreshed::Refused(error.to_string())
        }
        Err(_) | Ok(_) => Refreshed::Done,
    }
}

/// Background `trekr --index` runs, one at a time.
pub(crate) struct Indexer {
    running: Option<Job>,
    queue: VecDeque<PathBuf>,
    /// Roots already indexed (or attempted) this session. A root whose index
    /// fails — a directory that is not a repository — is not retried on every
    /// question about it.
    done: HashSet<PathBuf>,
    /// Whether the client can show `$/progress`.
    progress: bool,
    /// Whether to index at all — a client can turn it off.
    enabled: bool,
    /// Saved files whose refresh met another process writing the index. An
    /// index child scanned before the save, so its write will not carry the
    /// edit either: these are retried until they land (DEC-066).
    deferred: Vec<PathBuf>,
    retried: std::time::Instant,
    jobs: u32,
    /// The store was rebuilt for another schema: no more refreshes.
    refused: bool,
    /// Why, until `poll` logs it — once, not per save.
    unlogged: Option<String>,
}

struct Job {
    root: PathBuf,
    child: Child,
    /// Where the files the editor opens are sent, to be read first (DEC-322).
    hints: Option<ChildStdin>,
    token: String,
    started: std::time::Instant,
    /// Refilling a checkout an upgrade's rebuild of the store dropped.
    after_upgrade: bool,
}

impl Indexer {
    pub(crate) fn new(progress: bool, enabled: bool) -> Indexer {
        Indexer {
            running: None,
            queue: VecDeque::new(),
            done: HashSet::new(),
            progress,
            enabled,
            deferred: Vec::new(),
            retried: std::time::Instant::now(),
            jobs: 0,
            refused: false,
            unlogged: None,
        }
    }

    /// Ask for a root to be (re)indexed. `again` forces a run for a root
    /// indexed earlier this session — a bulk change needs it, a first
    /// question does not.
    pub(crate) fn want(&mut self, root: PathBuf, again: bool) {
        if !self.enabled {
            return;
        }
        if again {
            self.done.remove(&root);
        }
        let busy = self.running.as_ref().is_some_and(|job| job.root == root);
        if busy && !again || self.done.contains(&root) || self.queue.contains(&root) {
            return;
        }
        self.queue.push_back(root);
    }

    /// The editor opened a file: if an index of its checkout is running,
    /// that index reads it next, if it still can (DEC-322).
    pub(crate) fn opened(&mut self, path: &Path) {
        if let Some(job) = &mut self.running
            && path.starts_with(&job.root)
        {
            hint(job, path);
        }
    }

    /// The checkout being refilled after an upgrade dropped the store, and
    /// since when.
    pub(crate) fn after_upgrade(&self) -> Option<(PathBuf, std::time::Instant)> {
        self.running
            .as_ref()
            .filter(|job| job.after_upgrade)
            .map(|job| (job.root.clone(), job.started))
    }

    /// Work still in flight: an index running or queued, or a refresh that
    /// has yet to land.
    pub(crate) fn busy(&self) -> bool {
        self.running.is_some() || !self.queue.is_empty() || !self.deferred.is_empty()
    }

    /// Refresh one saved file now, or keep it for [`Indexer::retry`] if the
    /// index is being written. Never waits on the lock.
    pub(crate) fn refresh(&mut self, session: &mut Session, path: &Path) {
        if self.refused {
            return;
        }
        match refresh(session, path) {
            Refreshed::Busy if !self.deferred.iter().any(|p| p == path) => {
                self.deferred.push(path.to_path_buf());
            }
            Refreshed::Refused(why) => self.refuse(why),
            _ => {}
        }
    }

    /// The server reopened its store (DEC-300): writing is safe again, and
    /// every root is to be indexed afresh.
    pub(crate) fn reopened(&mut self) {
        self.refused = false;
        self.done.clear();
    }

    fn refuse(&mut self, why: String) {
        self.refused = true;
        self.unlogged = Some(why);
        self.deferred.clear();
    }

    /// Try the deferred refreshes again, at the pace the loop wakes for an
    /// index rather than before every request: each attempt re-reads, hashes
    /// and may parse the file. What lands is the file's latest bytes.
    pub(crate) fn retry(&mut self, session: &mut Session) {
        if self.deferred.is_empty() || self.retried.elapsed() < RETRY {
            return;
        }
        self.retried = std::time::Instant::now();
        let mut refused = None;
        self.deferred.retain(|path| match refresh(session, path) {
            Refreshed::Busy => true,
            Refreshed::Refused(why) => {
                refused = Some(why);
                false
            }
            Refreshed::Done => false,
        });
        if let Some(why) = refused {
            self.refuse(why);
        }
    }

    /// Reap a finished run and start the next. Returns the messages to send —
    /// progress, and nothing else.
    pub(crate) fn poll(&mut self, log: &Log, session: &Session) -> Vec<Message> {
        let mut out = Vec::new();
        if let Some(why) = self.unlogged.take() {
            log.event("refresh_refused", serde_json::json!({ "error": why }));
        }
        if let Some(job) = &mut self.running {
            match job.child.try_wait() {
                Ok(None) => return out,
                outcome => {
                    let ok = matches!(outcome, Ok(Some(status)) if status.success());
                    let job = self.running.take().expect("just matched");
                    log.event(
                        "index",
                        serde_json::json!({
                            "root": job.root.to_string_lossy(),
                            "ok": ok,
                            "ms": job.started.elapsed().as_millis() as u64,
                        }),
                    );
                    log.count(
                        "index",
                        String::new(),
                        if ok {
                            crate::usage::Outcome::Hit
                        } else {
                            crate::usage::Outcome::Error("index")
                        },
                        Some(job.started.elapsed()),
                        false,
                    );
                    if self.progress {
                        out.push(progress(
                            &job.token,
                            serde_json::json!({
                                "kind": "end",
                                "message": if ok { "indexed" } else { "index failed — see trekr --index" },
                            }),
                        ));
                    }
                    self.done.insert(job.root);
                }
            }
        }
        while let Some(root) = self.queue.pop_front() {
            // Asked before the child writes its first row: an empty store an
            // upgrade emptied, as the CLI tells it (`not_indexed_reason`).
            let store = session.store();
            let after_upgrade = store.roots().is_ok_and(|roots| roots.is_empty())
                && store.upgraded_from().ok().flatten().is_some();
            match spawn(&root) {
                Ok(mut child) => {
                    let hints = child.stdin.take();
                    self.jobs += 1;
                    let token = format!("trekr-index-{}", self.jobs);
                    log.event(
                        "index_start",
                        serde_json::json!({
                            "root": root.to_string_lossy(),
                            "after_upgrade": after_upgrade,
                        }),
                    );
                    if self.progress {
                        out.push(Message::Request(Request::new(
                            RequestId::from(token.clone()),
                            "window/workDoneProgress/create".into(),
                            serde_json::json!({ "token": token }),
                        )));
                        out.push(progress(
                            &token,
                            serde_json::json!({
                                "kind": "begin",
                                "title": "trekr",
                                "message": format!(
                                    "{} {}",
                                    if after_upgrade { "reindexing after an upgrade:" } else { "indexing" },
                                    crate::core::paths::pretty(&root.to_string_lossy())
                                ),
                                "cancellable": false,
                            }),
                        ));
                    }
                    let mut job = Job {
                        root,
                        child,
                        hints,
                        token,
                        started: std::time::Instant::now(),
                        after_upgrade,
                    };
                    // What is open now goes first; what opens later follows.
                    for path in session.open_paths() {
                        if path.starts_with(&job.root) {
                            hint(&mut job, path);
                        }
                    }
                    self.running = Some(job);
                    break;
                }
                Err(error) => {
                    log.event(
                        "index",
                        serde_json::json!({ "root": root.to_string_lossy(), "ok": false, "error": error.to_string() }),
                    );
                    log.count(
                        "index",
                        String::new(),
                        crate::usage::Outcome::Error("spawn"),
                        None,
                        false,
                    );
                    self.done.insert(root);
                }
            }
        }
        out
    }
}

/// `trekr --index ROOT`, from this very binary, silently. It inherits
/// `TREKR_DB`, so it writes the store this server reads, and is marked
/// [`BACKGROUND`] so it steps out of the editor's way.
fn spawn(root: &Path) -> std::io::Result<Child> {
    let binary = std::env::current_exe()?;
    Command::new(binary)
        .arg("--index")
        .arg(root)
        .env(BACKGROUND, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

/// Send the index child one path to read first. A child that has moved on or
/// gone just leaves it unread.
fn hint(job: &mut Job, path: &Path) {
    let sent = job
        .hints
        .as_mut()
        .map(|pipe| writeln!(pipe, "{}", path.to_string_lossy()));
    if matches!(sent, Some(Err(_))) {
        job.hints = None;
    }
}

/// Set on the index child: this run is background work, so it lowers its own
/// CPU and disk priority. The child does it rather than the spawn, so a
/// `trekr --index` someone runs by hand stays at full speed.
const BACKGROUND: &str = "TREKR_BACKGROUND";

/// Is this an index run the LSP spawned?
pub(crate) fn in_background() -> bool {
    std::env::var_os(BACKGROUND).is_some()
}

/// Files the language server wants read first: the ones open in the editor,
/// a path a line on the index child's stdin, as the editor opens them
/// (DEC-322). Read on a thread of their own, so the index never waits for one.
#[derive(Default)]
pub(crate) struct Hints(std::sync::Arc<std::sync::Mutex<Vec<PathBuf>>>);

impl Hints {
    /// Listen on stdin, in an index the language server spawned. Never a
    /// terminal: a background job reading one would be stopped by the shell.
    pub(crate) fn listen() -> Hints {
        let hints = Hints::default();
        // SAFETY: asks whether a descriptor is a terminal; nothing is read.
        if !in_background() || unsafe { libc::isatty(0) } == 1 {
            return hints;
        }
        let sink = hints.0.clone();
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else {
                    return;
                };
                if let Ok(mut sink) = sink.lock() {
                    sink.push(PathBuf::from(line));
                }
            }
        });
        hints
    }

    /// Hints as if sent already.
    #[cfg(test)]
    pub(crate) fn sent(paths: &[PathBuf]) -> Hints {
        let hints = Hints::default();
        hints.0.lock().unwrap().extend_from_slice(paths);
        hints
    }

    /// The hints that arrived since the last call, as paths in `root`.
    pub(crate) fn take(&self, root: &Path) -> Vec<String> {
        let Ok(mut hints) = self.0.lock() else {
            return Vec::new();
        };
        std::mem::take(&mut *hints)
            .into_iter()
            .filter_map(|path| {
                let path = std::fs::canonicalize(&path).unwrap_or(path);
                Some(path.strip_prefix(root).ok()?.to_string_lossy().into_owned())
            })
            .collect()
    }
}

/// In an index run the LSP spawned: drop CPU priority by 10 and disk I/O to a
/// low but not starvable tier, and log what the kernel then holds — read
/// back, not assumed, so a refused request shows as `unchanged`. Any other
/// run: nothing.
///
/// Not the lowest I/O tier (macOS `IOPOL_THROTTLE`, Linux's idle class): the
/// index holds SQLite's write lock while it writes, so an I/O tier that can
/// be starved stretches the lock that a save or a CLI query waits on (DEC-062).
///
/// Call before the run starts a thread: on Linux both settings are
/// per-thread, and only threads created afterwards inherit them.
pub(crate) fn yield_if_background() {
    if !in_background() {
        return;
    }
    // Best-effort: a refusal just means a less polite index.
    // SAFETY: plain syscalls on this process; no memory crosses them.
    let nice = unsafe {
        libc::nice(10);
        lower_io();
        libc::getpriority(libc::PRIO_PROCESS, 0)
    };
    Log::open(false).event(
        "index_priority",
        serde_json::json!({ "pid": std::process::id(), "nice": nice, "io": io_class() }),
    );
}

// <sys/resource.h>; not in the libc crate. IOPOL_TYPE_DISK = 0,
// IOPOL_SCOPE_PROCESS = 0, IOPOL_UTILITY = 4.
#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn setiopolicy_np(iotype: libc::c_int, scope: libc::c_int, policy: libc::c_int) -> libc::c_int;
    fn getiopolicy_np(iotype: libc::c_int, scope: libc::c_int) -> libc::c_int;
}

#[cfg(target_os = "macos")]
unsafe fn lower_io() {
    unsafe { setiopolicy_np(0, 0, 4) };
}

#[cfg(target_os = "macos")]
fn io_class() -> &'static str {
    // SAFETY: a read of this process's own policy.
    match unsafe { getiopolicy_np(0, 0) } {
        4 => "utility",
        _ => "unchanged",
    }
}

// <linux/ioprio.h>: IOPRIO_WHO_PROCESS = 1, class above IOPRIO_CLASS_SHIFT =
// 13, IOPRIO_CLASS_BE = 2 at its lowest level, 7. No libc wrapper exists.
#[cfg(target_os = "linux")]
const BEST_EFFORT_LOWEST: libc::c_long = (2 << 13) | 7;

#[cfg(target_os = "linux")]
unsafe fn lower_io() {
    unsafe { libc::syscall(libc::SYS_ioprio_set, 1, 0, BEST_EFFORT_LOWEST) };
}

#[cfg(target_os = "linux")]
fn io_class() -> &'static str {
    // SAFETY: a read of this thread's own I/O priority.
    match unsafe { libc::syscall(libc::SYS_ioprio_get, 1, 0) } {
        BEST_EFFORT_LOWEST => "best-effort-7",
        _ => "unchanged",
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
unsafe fn lower_io() {}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn io_class() -> &'static str {
    "unchanged"
}

fn progress(token: &str, value: serde_json::Value) -> Message {
    Message::Notification(Notification::new(
        "$/progress".into(),
        serde_json::json!({ "token": token, "value": value }),
    ))
}
