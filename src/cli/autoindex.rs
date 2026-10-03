//! A query in a checkout no index has filled indexes it first (DEC-500).
//!
//! The index is a `trekr --index` child in its own process group, so a
//! Ctrl-C or a caller's timeout stops the wait and not the work: the next
//! query finds the index under way, or done. The query only reads the store,
//! watching the child's commits through the checkout's `warming` mark
//! (DEC-320), and asks again once what it needs is in:
//!
//! - a question about the whole checkout — references, dead code, ancestors,
//!   a card's call sites — waits for the whole index, since a caller or an
//!   override may be in any file;
//! - a position waits for its own file and the files and gems that file
//!   names (DEC-322, DEC-330), told to the child as the language server tells
//!   it, and answers with `warming` while the rest is read in the background.
//!
//! One index per checkout: a first index claims the checkout as it marks it,
//! and one that finds another's live mark waits for it (`index_all`).

use super::failure::{Failure, Tag};
use super::{Output, incomplete, not_indexed_reason, paths};
use crate::store::{Store, Warming};
use std::io::Write;
use std::path::Path;
use std::process::{Child, ExitCode, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::{Duration, Instant};

static OFF: AtomicBool = AtomicBool::new(false);

/// `--no-index`: a query answers from what is indexed, and a checkout
/// nobody indexed is `not_indexed`, as before DEC-500.
pub(super) fn turn_off() {
    OFF.store(true, Relaxed);
}

/// Set on the index a query spawns: an index of the checkout another
/// process finished while this one waited for it is done.
const SPAWNED: &str = "TREKR_SPAWNED_BY_QUERY";

/// Is this `--index` one a query spawned?
pub(crate) fn spawned() -> bool {
    std::env::var_os(SPAWNED).is_some()
}

/// What a query needs of the index before it can answer.
#[derive(Clone, Copy)]
pub(super) enum Need<'a> {
    /// All of it: a caller, an override or a reopening may be in any file.
    Whole,
    /// This file, and what it names: a position's answer is about them.
    File(&'a Path),
}

/// After this long, a waiting query says why.
const NOTICE_AFTER: Duration = Duration::from_secs(1);
/// How often the store is read for the child's commits.
const POLL: Duration = Duration::from_millis(50);
/// How often a terminal's progress line is redrawn.
const REDRAW: Duration = Duration::from_millis(250);

/// Index `root` if the store cannot answer `need` from it yet, and wait
/// until it can. `Some` is the command's exit when the index could not
/// finish — reported as `--index` reports it (DEC-400). `None` is "ask
/// now": the store has what `need` asks, or indexing is off.
pub(super) fn ensure(
    out: Output,
    store: &Store,
    root: &Path,
    need: Need,
) -> anyhow::Result<Option<ExitCode>> {
    if OFF.load(Relaxed) {
        return Ok(None);
    }
    let lock = || {
        OURS.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    let mut ours = lock().take();
    let waited = wait(out, store, root, need, &mut ours);
    *lock() = ours;
    waited
}

/// The index this query started, while it runs: a second wait — for the
/// rest, after a miss in the part — watches it as the first did.
static OURS: std::sync::Mutex<Option<Index>> = std::sync::Mutex::new(None);

/// The checkout this query's index is still reading, once the query has
/// its answer.
pub(super) fn left_running() -> Option<String> {
    let mut ours = OURS.lock().ok()?;
    let index = ours.as_mut()?;
    match index.child.try_wait() {
        Ok(None) => Some(index.root.clone()),
        _ => None,
    }
}

fn wait(
    out: Output,
    store: &Store,
    root: &Path,
    need: Need,
    ours: &mut Option<Index>,
) -> anyhow::Result<Option<ExitCode>> {
    let root_str = root.to_string_lossy().into_owned();
    // A file outside the checkout — a gem's, asked with --context — is
    // never in its map: that question waits for the whole index.
    let file = match need {
        Need::File(file) => std::fs::canonicalize(file).ok().and_then(|file| {
            let relative = file.strip_prefix(root).ok()?.to_string_lossy().into_owned();
            Some((file, relative))
        }),
        Need::Whole => None,
    };
    let relative = file.as_ref().map(|(_, relative)| relative.as_str());
    let mut seen = look(store, &root_str, relative)?;
    if matches!(seen, Seen::Ready) {
        return Ok(None);
    }
    // Still ours, from this query's first wait: say what it said then.
    let mut heads = match (&seen, ours.as_ref()) {
        (Seen::Filling(w), Some(index)) if index.child.id() == w.pid => {
            Heads::new(&root_str, index.why, index.told)
        }
        _ => Heads::new(&root_str, Why::of(store, &seen)?, false),
    };
    // Whether the index that filled the store was ours, not another's.
    let mut filled = false;
    // The other index this query's file was handed to (DEC-512).
    let mut handed: Option<u32> = None;
    // Since when the claim has found the write lock held.
    let mut busy: Option<Instant> = None;
    loop {
        // Ours is reaped the moment it ends: until then it is a zombie, which
        // a liveness check takes for running, and its claim with it.
        if let Some(index) = ours.as_mut()
            && let Some(status) = index.child.try_wait()?
        {
            let index = ours.take().expect("just matched");
            // Its last commit may have landed since the look.
            if status.success() || matches!(look(store, &root_str, relative)?, Seen::Ready) {
                break;
            }
            heads.clear();
            let other = matches!(look(store, &root_str, relative)?, Seen::Filling(_));
            return ended(out, &root_str, status, other, &index).map(Some);
        }
        match &seen {
            Seen::Ready => break,
            // Another's index is bounded as a writer's turn is (DEC-139);
            // ours bounds itself.
            Seen::Filling(_) if ours.is_none() && heads.waited() >= crate::store::writer_wait() => {
                heads.clear();
                incomplete::report(
                    out,
                    &root_str,
                    "another trekr's index of it outlasted the writer wait",
                );
                crate::usage::outcome(crate::usage::Outcome::Error("incomplete"));
                return Ok(Some(ExitCode::from(2)));
            }
            Seen::Filling(warming) => {
                // Its index's mark, or the claim this query made for it.
                let mine = ours.as_ref().is_some_and(|ours| {
                    ours.child.id() == warming.pid || warming.pid == std::process::id()
                });
                filled |= mine;
                // Another's index reads this query's file next, as it would
                // the editor's (DEC-512).
                if let (false, Some((file, _)), Some(main)) = (mine, &file, store.path())
                    && handed != Some(warming.pid)
                {
                    handed = Some(warming.pid);
                    let _ =
                        crate::store::early::hint(main, warming.pid, std::slice::from_ref(file));
                }
                heads.tick(Some(warming));
            }
            Seen::Idle(warming) => {
                if ours.is_none() && claim(&root_str, &mut busy)? {
                    let file = file.as_ref().map(|(file, _)| file.as_path());
                    *ours = Some(spawn(root, file, heads.why)?);
                    crate::usage::flag("indexed");
                }
                heads.tick(warming.as_ref());
            }
        }
        std::thread::sleep(POLL);
        seen = look(store, &root_str, relative)?;
    }
    // The child prepares the tree after its last commit (DEC-192): a whole
    // question waits for that rather than assembling it again.
    if let (Need::Whole, Some(index), true) = (need, ours.as_mut(), filled) {
        let _ = index.child.wait();
    }
    if let Some(index) = ours.as_mut() {
        index.told |= heads.told;
    }
    heads.clear();
    Ok(None)
}

/// What a position query does with the answer a partial first index gave.
pub(super) enum Then {
    /// Report it.
    Keep,
    /// The rest is in now: ask again.
    Again,
    /// The rest could not be indexed: exit so.
    Exit(ExitCode),
}

/// A position's answer from a first index still under way stands when it
/// found something — said to be partial (DEC-320) — but a miss there is
/// "no answer yet", and this query can wait for the rest rather than hand
/// the caller a retry loop: in DEC-500's measurement two misses in five
/// became answers once the index ended.
pub(super) fn after_partial(
    out: Output,
    store: &Store,
    root: &Path,
    found: bool,
) -> anyhow::Result<Then> {
    // Read from a live index's part, as `answering_in` saw it.
    let partial = super::warming().is_some_and(|(_, w)| !w.interrupted);
    if found || OFF.load(Relaxed) || !partial {
        return Ok(Then::Keep);
    }
    // A miss from an early store — which holds the file and its neighbours,
    // not the rest of the checkout — waits for the rest's write to land in
    // the store, then asks afresh: this query reads the early copy. Only
    // then: a fresh process asking while the early store stands would find
    // it, miss again and loop for as long as its index stood still.
    if let Some(early) = forget_early() {
        let main = Store::open(&super::store_path()?)?;
        let root_str = root.to_string_lossy().into_owned();
        let mut heads = Heads::new(&root_str, Why::UnderWay, TOLD.load(Relaxed));
        // Ended, or cut short: the fresh process finishes it as any query.
        while let Some(warming) = main.warming(&root_str)?.filter(|w| !w.interrupted)
            && early.exists()
        {
            if heads.waited() >= crate::store::writer_wait() {
                heads.clear();
                incomplete::report(
                    out,
                    &root_str,
                    "another trekr's index of it outlasted the writer wait",
                );
                crate::usage::outcome(crate::usage::Outcome::Error("incomplete"));
                return Ok(Then::Exit(ExitCode::from(2)));
            }
            heads.tick(Some(&warming));
            std::thread::sleep(POLL);
        }
        heads.clear();
        use std::os::unix::process::CommandExt;
        let error = std::process::Command::new(std::env::current_exe()?)
            .args(std::env::args_os().skip(1))
            .exec();
        return Err(error.into());
    }
    match store.warming(&root.to_string_lossy())? {
        // Cut short since: the miss stands, said to be partial.
        Some(w) if w.interrupted => Ok(Then::Keep),
        Some(_) => Ok(match ensure(out, store, root, Need::Whole)? {
            Some(code) => Then::Exit(code),
            None => Then::Again,
        }),
        // Whole since the answer was read: ask the whole.
        None => Ok(Then::Again),
    }
}

/// The checkout as the store holds it now, against what is needed.
enum Seen {
    /// Ask now.
    Ready,
    /// A live index is filling it, and has not reached what is needed.
    Filling(Warming),
    /// Nothing is indexing it: never indexed, dropped by an upgrade, or cut
    /// short (the mark of an index no longer running).
    Idle(Option<Warming>),
}

fn look(store: &Store, root: &str, file: Option<&str>) -> anyhow::Result<Seen> {
    Ok(match store.warming(root)? {
        None if store.has_checkout(root)? => Seen::Ready,
        None => Seen::Idle(None),
        Some(warming) if warming.interrupted => Seen::Idle(Some(warming)),
        // The gems the file names land in the commit that counts the tree.
        Some(warming) => match file {
            Some(file) if !warming.uncounted && store.maps(root, file)? => Seen::Ready,
            Some(file) if early_maps(store, root, file, &warming) => Seen::Ready,
            _ => Seen::Filling(warming),
        },
    })
}

/// Whether the early store of the index filling the checkout (DEC-332) holds
/// `file`: the query then reads that, as the language server does (DEC-512).
fn early_maps(store: &Store, root: &str, file: &str, warming: &Warming) -> bool {
    let Some(main) = store.path() else {
        return false;
    };
    let path = crate::store::early::path(main, warming.pid);
    if !path.exists() {
        return false;
    }
    let Ok(early) = Store::open_existing(&path) else {
        return false;
    };
    let holds = early
        .warming(root)
        .is_ok_and(|w| w.is_some_and(|w| !w.uncounted))
        && early.maps(root, file).unwrap_or(false);
    if holds {
        *lock_early() = Some(path);
    }
    holds
}

/// The early store this query answers from, once its wait found the file
/// there.
static EARLY: std::sync::Mutex<Option<std::path::PathBuf>> = std::sync::Mutex::new(None);

fn lock_early() -> std::sync::MutexGuard<'static, Option<std::path::PathBuf>> {
    EARLY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The early store a query reads in place of the store, if any.
pub(super) fn early_store() -> Option<std::path::PathBuf> {
    lock_early().clone()
}

/// Stop reading the early store: gone, or the answer needs the whole.
pub(super) fn forget_early() -> Option<std::path::PathBuf> {
    lock_early().take()
}

/// Claim the checkout for the index this query is about to start, so a
/// query beside it waits for that one rather than starting its own. `false`
/// when another's claim stood: no index to start. A write lock held a while
/// — another checkout's index — is not worth waiting out: start it anyway,
/// and its own claim decides.
fn claim(root: &str, busy: &mut Option<Instant>) -> anyhow::Result<bool> {
    let mut store = Store::open(&super::store_path()?)?;
    match store.claim_for_child(root) {
        Ok(None) => Ok(true),
        Ok(Some(_)) => Ok(false),
        Err(error) if crate::store::is_busy(&error) => {
            Ok(busy.get_or_insert_with(Instant::now).elapsed() >= CLAIM_BUSY)
        }
        Err(error) => Err(error.into()),
    }
}

/// How long a query's claim waits on the write lock before starting its
/// index without one.
const CLAIM_BUSY: Duration = Duration::from_millis(500);

/// An index this query started.
struct Index {
    child: Child,
    /// The checkout it indexes, as a person reads it.
    root: String,
    /// Why this query waits for it, and whether it has said so.
    why: Why,
    told: bool,
    /// Its stderr: what it said when it failed. Unlinked once open, so
    /// nothing is left behind however either process ends — and a file
    /// rather than a pipe, which a child outliving this query would die
    /// writing to.
    said: Option<std::fs::File>,
}

impl Index {
    /// The last thing it said, without its `trekr: ` prefix.
    fn last_words(&self) -> Option<String> {
        use std::io::{Read, Seek};
        let mut file = self.said.as_ref()?;
        file.seek(std::io::SeekFrom::Start(0)).ok()?;
        let mut said = String::new();
        file.take(64 * 1024).read_to_string(&mut said).ok()?;
        let line = said.lines().rev().find(|line| !line.trim().is_empty())?;
        Some(line.trim().trim_start_matches("trekr: ").to_string())
    }
}

/// `trekr --index ROOT`, from this binary, in a process group of its own so
/// a Ctrl-C at the terminal stops this query and not the index. Told `file`
/// as the language server tells its child an open file, it reads that first
/// (DEC-322). At full priority, unlike the language server's (DEC-062):
/// another query may be waiting on it.
fn spawn(root: &Path, file: Option<&Path>, why: Why) -> anyhow::Result<Index> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let said = said_file();
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("--index")
        .arg(root)
        .env(SPAWNED, "1")
        .stdout(Stdio::null())
        .stderr(match said.as_ref().and_then(|f| f.try_clone().ok()) {
            Some(file) => Stdio::from(file),
            None => Stdio::null(),
        })
        .process_group(0);
    command.stdin(match file {
        Some(_) => Stdio::piped(),
        None => Stdio::null(),
    });
    let mut child = command.spawn().tag(Failure::Io)?;
    if let (Some(file), Some(mut hints)) = (file, child.stdin.take()) {
        // A child that has already moved on leaves it unread.
        let _ = writeln!(hints, "{}", file.display());
    }
    Ok(Index {
        child,
        root: paths::pretty(&root.to_string_lossy()),
        why,
        told: false,
        said,
    })
}

/// A file for the child's stderr, already unlinked.
fn said_file() -> Option<std::fs::File> {
    let path = std::env::temp_dir().join(format!("trekr-index-{}.err", std::process::id()));
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .ok()?;
    let _ = std::fs::remove_file(&path);
    Some(file)
}

/// The child ended without the store holding what was needed. `other`: a
/// live index of another process is filling the checkout now.
fn ended(
    out: Output,
    root: &str,
    status: ExitStatus,
    other: bool,
    index: &Index,
) -> anyhow::Result<ExitCode> {
    let why = match status.code() {
        // DEC-400's "ask again": the wait for a writer or an index ran out.
        Some(2) if other => "another trekr's index of it outlasted the writer wait",
        Some(2) => "another trekr writer kept the write lock longer than an index waits",
        None => "the index was stopped by a signal",
        Some(code) => {
            let pretty = paths::pretty(root);
            let why = index.last_words().map_or_else(
                || format!("exit {code}"),
                |said| format!("{said} (exit {code})"),
            );
            return Err(failure_of(code).error(format!(
                "indexing {pretty} failed: {why}; `trekr --index {pretty}` tries again"
            )));
        }
    };
    incomplete::report(out, root, why);
    crate::usage::outcome(crate::usage::Outcome::Error("incomplete"));
    Ok(ExitCode::from(2))
}

/// The kind an index child's exit code names (DEC-067), for a caller that
/// branches on `kind`.
fn failure_of(code: i32) -> Failure {
    [
        Failure::Usage,
        Failure::NotFound,
        Failure::Git,
        Failure::Database,
    ]
    .into_iter()
    .find(|kind| i32::from(kind.exit_code()) == code)
    .unwrap_or(Failure::Internal)
}

/// Why a query is waiting, as its notice says it.
#[derive(Clone, Copy)]
enum Why {
    First,
    Upgraded,
    CutShort,
    UnderWay,
}

impl Why {
    fn of(store: &Store, seen: &Seen) -> anyhow::Result<Why> {
        Ok(match seen {
            Seen::Filling(_) => Why::UnderWay,
            Seen::Idle(Some(_)) => Why::CutShort,
            Seen::Idle(None) | Seen::Ready => match not_indexed_reason(store)?.1 {
                true => Why::Upgraded,
                false => Why::First,
            },
        })
    }
}

/// Whether this query has said why it waits: a second wait says it once.
static TOLD: AtomicBool = AtomicBool::new(false);

/// What a person or an agent waiting on the index is told: one line on
/// stderr once it has taken a second — stdout stays the answer alone — and
/// at a terminal a progress line that clears itself.
struct Heads {
    root: String,
    why: Why,
    started: Instant,
    told: bool,
    terminal: bool,
    drawn: Option<Instant>,
}

impl Heads {
    fn new(root: &str, why: Why, told: bool) -> Heads {
        use std::io::IsTerminal;
        Heads {
            root: paths::pretty(root),
            why,
            started: Instant::now(),
            told,
            terminal: std::io::stderr().is_terminal(),
            drawn: None,
        }
    }

    fn waited(&self) -> Duration {
        self.started.elapsed()
    }

    fn tick(&mut self, warming: Option<&Warming>) {
        let waited = self.waited();
        if waited < NOTICE_AFTER {
            return;
        }
        if !self.told {
            self.told = true;
            TOLD.store(true, Relaxed);
            eprintln!("{}", notice(&self.root, &self.why, warming));
        }
        if self.terminal && self.drawn.is_none_or(|at| at.elapsed() >= REDRAW) {
            self.drawn = Some(Instant::now());
            eprint!("\r\x1b[K{}", progress(warming, waited));
        }
    }

    fn clear(&mut self) {
        if self.drawn.take().is_some() {
            eprint!("\r\x1b[K");
        }
    }
}

fn notice(root: &str, why: &Why, warming: Option<&Warming>) -> String {
    let files = match warming {
        // A query's claim, made before its index has listed anything.
        Some(w) if w.of == 0 => String::new(),
        Some(w) if w.uncounted => format!(" ({} files)", w.of),
        Some(w) => format!(" ({} files, counting its gems and Ruby's)", w.of),
        None => String::new(),
    };
    match why {
        Why::First => {
            format!("trekr: indexing {root} for the first time{files} — once; later queries use it")
        }
        Why::Upgraded => format!(
            "trekr: indexing {root} again{files}: this trekr's index format is new, \
             and the upgrade dropped the old one"
        ),
        Why::CutShort => format!("trekr: finishing the index of {root}, cut short earlier{files}"),
        Why::UnderWay => {
            format!("trekr: waiting for the index of {root} that another trekr is building{files}")
        }
    }
}

fn progress(warming: Option<&Warming>, waited: Duration) -> String {
    let secs = waited.as_secs();
    match warming {
        Some(w) if !w.uncounted => format!("trekr: {} of {} files read, {secs}s", w.read, w.of),
        _ => format!("trekr: listing the files, {secs}s"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn warming(read: u64, of: u64, uncounted: bool) -> Warming {
        Warming {
            read,
            of,
            interrupted: false,
            pid: 1,
            uncounted,
            start: None,
        }
    }

    #[test]
    fn the_notice_counts_what_the_mark_counts() {
        let first = notice("~/app", &Why::First, Some(&warming(0, 3270, true)));
        assert_eq!(
            first,
            "trekr: indexing ~/app for the first time (3270 files) — once; later queries use it"
        );
        let counted = notice("~/app", &Why::UnderWay, Some(&warming(10, 9000, false)));
        assert!(
            counted.contains("(9000 files, counting its gems and Ruby's)"),
            "{counted}"
        );
        assert!(
            counted.contains("of ~/app that another trekr is building"),
            "{counted}"
        );
        // A query's claim, before its index has counted anything.
        let claimed = notice("~/app", &Why::UnderWay, Some(&warming(0, 0, true)));
        assert!(!claimed.contains("files"), "{claimed}");
        let unseen = notice("~/app", &Why::First, None);
        assert!(unseen.contains("for the first time — once"), "{unseen}");
    }

    #[test]
    fn progress_says_files_only_once_they_are_counted() {
        let secs = Duration::from_millis(2600);
        assert_eq!(
            progress(Some(&warming(400, 9000, false)), secs),
            "trekr: 400 of 9000 files read, 2s"
        );
        assert_eq!(
            progress(Some(&warming(0, 3270, true)), secs),
            "trekr: listing the files, 2s"
        );
    }

    #[test]
    fn a_childs_exit_code_names_its_failure() {
        assert_eq!(failure_of(74), Failure::Database);
        assert_eq!(failure_of(66), Failure::NotFound);
        assert_eq!(failure_of(101), Failure::Internal);
    }
}
