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
    let mut heads = Heads::new(&root_str, Why::of(store, &seen)?);
    let mut ours: Option<Child> = None;
    // Whether the index that filled the store was ours, not another's.
    let mut filled = false;
    loop {
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
                filled |= ours.as_ref().is_some_and(|child| child.id() == warming.pid);
                heads.tick(Some(warming));
            }
            Seen::Idle(warming) => {
                match ours.as_mut() {
                    None => {
                        ours = Some(spawn(root, file.as_ref().map(|(file, _)| file.as_path()))?);
                        crate::usage::flag("indexed");
                    }
                    Some(child) => {
                        if let Some(status) = child.try_wait()? {
                            // Its last commit may have landed since the look.
                            if matches!(look(store, &root_str, relative)?, Seen::Ready) {
                                break;
                            }
                            heads.clear();
                            return ended(out, &root_str, status).map(Some);
                        }
                    }
                }
                heads.tick(warming.as_ref());
            }
        }
        std::thread::sleep(POLL);
        seen = look(store, &root_str, relative)?;
    }
    // The child prepares the tree after its last commit (DEC-192): a whole
    // question waits for that rather than assembling it again.
    if let (Need::Whole, Some(child), true) = (need, ours.as_mut(), filled) {
        let _ = child.wait();
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
    let live = store
        .warming(&root.to_string_lossy())?
        .is_some_and(|w| !w.interrupted);
    if found || OFF.load(Relaxed) || !live {
        return Ok(Then::Keep);
    }
    Ok(match ensure(out, store, root, Need::Whole)? {
        Some(code) => Then::Exit(code),
        None => Then::Again,
    })
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
            _ => Seen::Filling(warming),
        },
    })
}

/// `trekr --index ROOT`, from this binary, in a process group of its own so
/// a Ctrl-C at the terminal stops this query and not the index. Told `file`
/// as the language server tells its child an open file, it reads that first
/// (DEC-322). At full priority, unlike the language server's (DEC-062):
/// another query may be waiting on it.
fn spawn(root: &Path, file: Option<&Path>) -> anyhow::Result<Child> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("--index")
        .arg(root)
        .env(SPAWNED, "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
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
    Ok(child)
}

/// The child ended without the store holding what was needed.
fn ended(out: Output, root: &str, status: ExitStatus) -> anyhow::Result<ExitCode> {
    let why = match status.code() {
        // DEC-400's "ask again": the write lock was outwaited.
        Some(2) => "another trekr writer kept the write lock longer than an index waits",
        None => "the index was stopped by a signal",
        Some(code) => {
            let pretty = paths::pretty(root);
            return Err(failure_of(code).error(format!(
                "indexing {pretty} failed (exit {code}); `trekr --index {pretty}` says why"
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
    fn new(root: &str, why: Why) -> Heads {
        use std::io::IsTerminal;
        Heads {
            root: paths::pretty(root),
            why,
            started: Instant::now(),
            told: false,
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
            format!("trekr: waiting for the index of {root} another trekr is building{files}")
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
