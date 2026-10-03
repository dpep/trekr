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

/// How long after an index outwaited another writer's lock it is run again.
/// The index itself waits for the lock as long as one writer's turn may take
/// (DEC-139), so this only spaces the attempts; it is never a tight loop.
const AGAIN: std::time::Duration = std::time::Duration::from_secs(2);

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
    let facts = (!known).then(|| crate::extract::extract_file(&located.relative, &bytes));
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
    /// Roots whose first index died mid-session and was started again: once
    /// each, so an index that dies every time is not run forever.
    resumed: HashSet<PathBuf>,
    /// Roots whose index outwaited another writer, and when to run it again.
    waiting: Vec<(PathBuf, std::time::Instant)>,
}

/// What background indexing is doing, as of the serve loop's last turn.
#[derive(Default)]
pub(crate) struct View {
    enabled: bool,
    waiting: HashSet<PathBuf>,
    /// Indexed this session, and not to be again unasked. One that succeeded
    /// is in the store, and never asked about here.
    failed: HashSet<PathBuf>,
}

impl View {
    /// What is under way for `root`, as a hover may say it.
    pub(crate) fn of(&self, root: &Path) -> Background {
        if !self.enabled || self.failed.contains(root) {
            Background::Off
        } else if self.waiting.contains(root) {
            Background::Waiting
        } else {
            Background::Indexing
        }
    }
}

/// What a background index of a checkout is doing, for a hover to say no
/// more than is true.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Background {
    /// Running, queued, or about to be: the request asking found it unindexed.
    Indexing,
    /// Outwaited another trekr writing the store; runs again after `AGAIN`.
    Waiting,
    /// Not coming: background indexing is off, or this session's index of it
    /// failed.
    Off,
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
    /// A first index (DEC-320): nothing whole of the checkout stood when it
    /// started, so a whole one standing at its end is its own.
    first: bool,
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
            resumed: HashSet::new(),
            waiting: Vec::new(),
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
            self.waiting.retain(|(waiting, _)| *waiting != root);
        }
        let busy = self.running.as_ref().is_some_and(|job| job.root == root);
        let waiting = self.waiting.iter().any(|(waiting, _)| *waiting == root);
        if busy && !again || waiting || self.done.contains(&root) || self.queue.contains(&root) {
            return;
        }
        self.queue.push_back(root);
    }

    /// What is under way, for the session to say: a hover is answered
    /// where the indexer is out of reach.
    pub(crate) fn view(&self) -> View {
        let queued = |root: &PathBuf| {
            self.queue.contains(root) || self.running.as_ref().is_some_and(|job| job.root == *root)
        };
        View {
            enabled: self.enabled,
            waiting: self.waiting.iter().map(|(root, _)| root.clone()).collect(),
            failed: self
                .done
                .iter()
                .filter(|root| !queued(root))
                .cloned()
                .collect(),
        }
    }

    /// An index waits to be run again: the loop has to wake to run it.
    pub(crate) fn waiting(&self) -> bool {
        !self.waiting.is_empty()
    }

    /// A checkout's first index died before it finished: run it again, as the
    /// server does for one found cut short at start (DEC-320), once a session.
    pub(crate) fn resume(&mut self, root: PathBuf) {
        if self.resumed.insert(root.clone()) {
            self.want(root, true);
        }
    }

    /// The editor opened a file: if an index of its checkout is running,
    /// that index reads it next, if it still can (DEC-322). `true` when this
    /// server's own index was told.
    /// `false` when no index of this server's has it to read.
    pub(crate) fn opened(&mut self, path: &Path) -> bool {
        if let Some(job) = &mut self.running
            && path.starts_with(&job.root)
        {
            hint(job, path);
            return true;
        }
        false
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
            let outcome = match job.child.try_wait() {
                Ok(None) => return out,
                Ok(Some(status)) => Exit {
                    ok: status.success(),
                    // `trekr --index` exits 2 only when it outwaited the lock:
                    // "no answer yet, ask again" (DEC-400).
                    outwaited: status.code() == Some(2),
                },
                Err(_) => Exit {
                    ok: false,
                    outwaited: false,
                },
            };
            let job = self.running.take().expect("just matched");
            log.event(
                "index",
                serde_json::json!({
                    "root": job.root.to_string_lossy(),
                    "ok": outcome.ok,
                    "outwaited": outcome.outwaited,
                    "ms": job.started.elapsed().as_millis() as u64,
                }),
            );
            log.count(
                "index",
                String::new(),
                if outcome.ok {
                    crate::usage::Outcome::Hit
                } else {
                    crate::usage::Outcome::Error("index")
                },
                Some(job.started.elapsed()),
                false,
            );
            let key = job.root.to_string_lossy();
            let store = session.main_store();
            let cut_short = store.warming(&key).ok().flatten().filter(|w| w.interrupted);
            let ending = ending(
                outcome,
                cut_short.as_ref(),
                job.first && store.has_checkout(&key).unwrap_or(false),
                self.resumed.contains(&job.root),
                &crate::core::paths::pretty(&key),
            );
            if self.progress {
                // VS Code shows no `end` message, so the last `report` carries it.
                for kind in ["report", "end"] {
                    out.push(progress(
                        &job.token,
                        serde_json::json!({ "kind": kind, "message": ending.message }),
                    ));
                }
            }
            if ending.warn {
                out.push(Message::Notification(Notification::new(
                    "window/showMessage".into(),
                    serde_json::json!({ "type": 2, "message": format!("trekr: {}", ending.message) }),
                )));
            }
            match ending.then {
                Then::Retry => self
                    .waiting
                    .push((job.root, std::time::Instant::now() + AGAIN)),
                Then::Resume => {
                    self.done.insert(job.root.clone());
                    self.resume(job.root);
                }
                Then::Rest => {
                    self.done.insert(job.root);
                }
            }
        }
        let now = std::time::Instant::now();
        let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.waiting)
            .into_iter()
            .partition(|(_, at)| *at <= now);
        self.waiting = later;
        if self.running.is_none() {
            self.queue.extend(due.into_iter().map(|(root, _)| root));
        } else {
            self.waiting.extend(due);
        }
        while let Some(root) = self.queue.pop_front() {
            // Asked before the child writes its first row: an empty store an
            // upgrade emptied, as the CLI tells it (`not_indexed_reason`).
            let store = session.store();
            let after_upgrade = store.roots().is_ok_and(|roots| roots.is_empty())
                && store.upgraded_from().ok().flatten().is_some();
            let key = root.to_string_lossy();
            let main = session.main_store();
            let first = !main.has_checkout(&key).unwrap_or(false)
                || main.warming(&key).ok().flatten().is_some();
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
                        first,
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

/// How an index child exited.
#[derive(Clone, Copy)]
struct Exit {
    ok: bool,
    /// It outwaited another writer's lock (exit 2).
    outwaited: bool,
}

/// What follows an index's end.
#[derive(Debug, PartialEq)]
enum Then {
    /// Nothing: it is done, or failed for good this session.
    Rest,
    /// Run it again once, as a first index found cut short is (DEC-320).
    Resume,
    /// Run it again after `AGAIN`, as often as it is outwaited.
    Retry,
}

/// What the editor is told when an index ends, and what follows.
#[derive(Debug, PartialEq)]
struct Ending {
    message: String,
    /// Said where a person sees it, once: the checkout is left partial or
    /// unindexed, and nothing will change that on its own.
    warn: bool,
    then: Then,
}

/// How an index's end reads. `cut_short` is the checkout's mark when the
/// index that left it is gone; `whole` that a first index left the checkout
/// whole — its last commit landed, whatever it died of after.
fn ending(
    exit: Exit,
    cut_short: Option<&crate::store::Warming>,
    whole: bool,
    resumed: bool,
    root: &str,
) -> Ending {
    let ending = |message: String, warn, then| Ending {
        message,
        warn,
        then,
    };
    match cut_short {
        _ if exit.outwaited => ending(
            format!(
                "waiting for another trekr writing the index{} — trying again once it is done",
                cut_short
                    .map(|w| format!(" ({})", how_far(w)))
                    .unwrap_or_default()
            ),
            false,
            Then::Retry,
        ),
        Some(w) if !resumed => ending(
            format!("index cut short ({}) — reading the rest", how_far(w)),
            false,
            Then::Resume,
        ),
        Some(w) => ending(
            format!(
                "index cut short ({}) — answers are partial until: trekr --index {root}",
                how_far(w)
            ),
            true,
            Then::Rest,
        ),
        None if exit.ok || whole => ending("indexed".into(), false, Then::Rest),
        None => ending(
            format!("index failed — see trekr --index {root}"),
            true,
            Then::Rest,
        ),
    }
}

/// How far a first index has read, as every surface says it: of the files
/// the checkout's tree spans — its own, its gems' and its Ruby's — which is
/// what an answer's confidence is scaled by (DEC-320).
pub(crate) fn how_far(warming: &crate::store::Warming) -> String {
    match warming.uncounted {
        true => "files not counted yet".to_string(),
        false => format!(
            "{} of {} files read, counting its gems and Ruby's",
            warming.read, warming.of
        ),
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
pub(crate) struct Hints {
    sent: std::sync::Arc<std::sync::Mutex<Vec<PathBuf>>>,
    /// Someone may send any: this index was spawned by a language server.
    pub(crate) listening: bool,
}

impl Hints {
    /// Listen on stdin, in an index the language server or a query
    /// spawned (DEC-500). Never a
    /// terminal: a background job reading one would be stopped by the shell.
    pub(crate) fn listen() -> Hints {
        let mut hints = Hints::default();
        // SAFETY: asks whether a descriptor is a terminal; nothing is read.
        let told = in_background() || crate::cli::autoindex::spawned();
        if !told || unsafe { libc::isatty(0) } == 1 {
            return hints;
        }
        hints.listening = true;
        let sink = hints.sent.clone();
        let read = move || {
            use std::io::BufRead;
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else {
                    return;
                };
                if let Ok(mut sink) = sink.lock() {
                    sink.push(PathBuf::from(line));
                }
            }
        };
        // A query writes its one file and closes the pipe at spawn: read it
        // now, or the first part may be written without it.
        match in_background() {
            true => drop(std::thread::spawn(read)),
            false => read(),
        }
        hints
    }

    /// Also read the hints other processes append to `file` (DEC-512),
    /// from its start, as they arrive. Someone may send some now.
    pub(crate) fn tail(&mut self, file: PathBuf) {
        self.listening = true;
        let sink = self.sent.clone();
        std::thread::spawn(move || {
            use std::io::{Read, Seek};
            let (mut at, mut partial) = (0u64, Vec::new());
            loop {
                if let Ok(mut opened) = crate::store::early::open_hints(&file)
                    && opened.seek(std::io::SeekFrom::Start(at)).is_ok()
                {
                    let mut more = Vec::new();
                    if let Ok(read) = opened.read_to_end(&mut more) {
                        at += read as u64;
                        partial.extend_from_slice(&more);
                    }
                }
                while let Some(end) = partial.iter().position(|b| *b == b'\n') {
                    let line: Vec<u8> = partial.drain(..=end).collect();
                    let line = String::from_utf8_lossy(&line[..end]).into_owned();
                    if let Ok(mut sink) = sink.lock() {
                        sink.push(PathBuf::from(line));
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        });
    }

    /// The hints that arrived since the last call, as they were sent: for a
    /// losing index to hand to the winning one.
    pub(crate) fn take_sent(&self) -> Vec<PathBuf> {
        self.sent
            .lock()
            .map(|mut sent| std::mem::take(&mut *sent))
            .unwrap_or_default()
    }

    /// Hints as if sent already.
    #[cfg(test)]
    pub(crate) fn sent(paths: &[PathBuf]) -> Hints {
        let hints = Hints {
            listening: true,
            ..Hints::default()
        };
        hints.sent.lock().unwrap().extend_from_slice(paths);
        hints
    }

    /// The hints that arrived since the last call, as paths in `root`.
    pub(crate) fn take(&self, root: &Path) -> Vec<String> {
        let Ok(mut hints) = self.sent.lock() else {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn mark(read: u64, of: u64, uncounted: bool) -> crate::store::Warming {
        crate::store::Warming {
            read,
            of,
            interrupted: true,
            pid: 1,
            uncounted,
            start: None,
        }
    }

    #[test]
    fn an_index_end_reads_as_what_the_store_holds() {
        let ok = Exit {
            ok: true,
            outwaited: false,
        };
        let killed = Exit {
            ok: false,
            outwaited: false,
        };
        let outwaited = Exit {
            ok: false,
            outwaited: true,
        };
        let part = mark(4091, 17322, false);
        let begun = mark(0, 3270, true);
        let at = |exit, mark, whole, resumed| ending(exit, mark, whole, resumed, "~/app");
        let read = "4091 of 17322 files read, counting its gems and Ruby's";
        let cases = [
            (
                at(ok, None, true, false),
                "indexed".into(),
                false,
                Then::Rest,
            ),
            // Killed after its last commit: the store is whole, so no alarm.
            (
                at(killed, None, true, true),
                "indexed".into(),
                false,
                Then::Rest,
            ),
            (
                at(killed, None, false, false),
                "index failed — see trekr --index ~/app".into(),
                true,
                Then::Rest,
            ),
            (
                at(outwaited, None, false, false),
                "waiting for another trekr writing the index — trying again once it is done".into(),
                false,
                Then::Retry,
            ),
            // Outwaited is "ask again" however often, resumed or not.
            (
                at(outwaited, Some(&part), false, true),
                format!(
                    "waiting for another trekr writing the index ({read}) — trying again once it is done"
                ),
                false,
                Then::Retry,
            ),
            (
                at(killed, Some(&part), false, false),
                format!("index cut short ({read}) — reading the rest"),
                false,
                Then::Resume,
            ),
            (
                at(killed, Some(&part), false, true),
                format!(
                    "index cut short ({read}) — answers are partial until: trekr --index ~/app"
                ),
                true,
                Then::Rest,
            ),
            // A count of the checkout's own files is not shown beside the tree's.
            (
                at(killed, Some(&begun), false, false),
                "index cut short (files not counted yet) — reading the rest".into(),
                false,
                Then::Resume,
            ),
        ];
        for (got, message, warn, then) in cases {
            assert_eq!(
                got,
                Ending {
                    message,
                    warn,
                    then
                }
            );
        }
    }

    #[test]
    fn a_hover_is_told_only_what_is_under_way() {
        let root = PathBuf::from("/app");
        let mut indexer = Indexer::new(true, true);
        assert_eq!(
            indexer.view().of(&root),
            Background::Indexing,
            "about to be asked for"
        );
        indexer
            .waiting
            .push((root.clone(), std::time::Instant::now() + AGAIN));
        assert_eq!(indexer.view().of(&root), Background::Waiting);
        indexer.want(root.clone(), false);
        assert!(indexer.queue.is_empty(), "a waiting index is not run early");
        indexer.waiting.clear();
        indexer.done.insert(root.clone());
        assert_eq!(
            indexer.view().of(&root),
            Background::Off,
            "failed this session"
        );
        assert_eq!(
            Indexer::new(true, false).view().of(&root),
            Background::Off,
            "turned off"
        );
    }
}
