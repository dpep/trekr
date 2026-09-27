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
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

/// More changed files than this in one batch is an operation — a checkout, a
/// rebase — and is handed to a full index rather than refreshed one by one.
pub(crate) const BULK: usize = 32;

/// How often a refresh that met a busy index is tried again.
const RETRY: std::time::Duration = std::time::Duration::from_millis(250);

/// Bring one saved file's facts up to date. `false` when another process
/// holds the write lock and the refresh has to be tried again.
///
/// Unconditional, unlike the CLI's probe-gated refresh: a save is the editor
/// telling us the file changed, so there is nothing to probe for.
fn refresh(session: &mut Session, path: &Path) -> bool {
    let Some(located) = session.locate(path) else {
        return true;
    };
    let root = located.root.to_string_lossy().into_owned();
    if !session.store().has_checkout(&root).unwrap_or(false) {
        return true;
    }
    let Ok(bytes) = std::fs::read(&located.absolute) else {
        return true;
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
        Err(error) => !crate::store::is_busy(&error),
        Ok(_) => true,
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
}

struct Job {
    root: PathBuf,
    child: Child,
    token: String,
    started: std::time::Instant,
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

    /// Work still in flight: an index running or queued, or a refresh that
    /// has yet to land.
    pub(crate) fn busy(&self) -> bool {
        self.running.is_some() || !self.queue.is_empty() || !self.deferred.is_empty()
    }

    /// Refresh one saved file now, or keep it for [`Indexer::retry`] if the
    /// index is being written. Never waits on the lock.
    pub(crate) fn refresh(&mut self, session: &mut Session, path: &Path) {
        if !refresh(session, path) && !self.deferred.iter().any(|p| p == path) {
            self.deferred.push(path.to_path_buf());
        }
    }

    /// Try the deferred refreshes again, at the pace the loop wakes for an
    /// index rather than before every request: each attempt re-reads, hashes
    /// and may parse the file. What lands is the file's latest bytes.
    pub(crate) fn retry(&mut self, session: &mut Session) {
        if self.deferred.is_empty() || self.retried.elapsed() < RETRY {
            return;
        }
        self.retried = std::time::Instant::now();
        self.deferred.retain(|path| !refresh(session, path));
    }

    /// Reap a finished run and start the next. Returns the messages to send —
    /// progress, and nothing else.
    pub(crate) fn poll(&mut self, log: &Log) -> Vec<Message> {
        let mut out = Vec::new();
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
            match spawn(&root) {
                Ok(child) => {
                    self.jobs += 1;
                    let token = format!("trekr-index-{}", self.jobs);
                    log.event(
                        "index_start",
                        serde_json::json!({ "root": root.to_string_lossy() }),
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
                                "message": format!("indexing {}", crate::core::paths::pretty(&root.to_string_lossy())),
                                "cancellable": false,
                            }),
                        ));
                    }
                    self.running = Some(Job {
                        root,
                        child,
                        token,
                        started: std::time::Instant::now(),
                    });
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
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

/// Set on the index child: this run is background work, so it lowers its own
/// CPU and disk priority. The child does it rather than the spawn, so a
/// `trekr --index` someone runs by hand stays at full speed.
const BACKGROUND: &str = "TREKR_BACKGROUND";

/// Is this an index run the LSP spawned?
pub(crate) fn in_background() -> bool {
    std::env::var_os(BACKGROUND).is_some()
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
