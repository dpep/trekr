//! A checkout's first index, while it is still filling the store (DEC-320).
//!
//! The checkout's rows land before its gems', so a question asked in between
//! finds an index that looks whole and is not. The index says so in `meta`,
//! one row per checkout, written before its first rows are and removed once
//! its last are: a reader sees the marker in the same snapshot as the rows it
//! qualifies. Only a checkout with no complete map is marked — a reindex
//! replaces a whole map with a whole map in one write.
//!
//! `meta` rather than a column: nothing needs the marker to answer, so a store
//! without it (an older trekr's, at this version) is not rebuilt for it.

use super::Store;
use rusqlite::{OptionalExtension, Result, params};

/// An index of the asked checkout still under way, or cut short.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub(crate) struct Warming {
    /// Files of this checkout's tree — its own, its Ruby's stdlib, its gems'
    /// — whose facts the tree can see now.
    pub(crate) read: u64,
    /// Files it will span once the index is done.
    pub(crate) of: u64,
    /// The index that set it is no longer running: the checkout stays
    /// partial until the next one.
    pub(crate) interrupted: bool,
    /// That index's process: whose early store to read (DEC-332).
    #[serde(skip)]
    pub(crate) pid: u32,
    /// Written before the index listed the gems: `of` is the checkout's own
    /// files alone, not the tree's, so it is no count to show beside a later
    /// one.
    #[serde(skip)]
    pub(crate) uncounted: bool,
    /// When that process started, as the kernel tells it: a pid reused
    /// since names another process. `None` in a mark an older trekr wrote.
    #[serde(skip)]
    pub(crate) start: Option<u64>,
}

impl Warming {
    /// As of now: the index that was running when this was read may have
    /// died since, and a tree keeps what it was built with for its life.
    pub(crate) fn now(mut self) -> Warming {
        self.interrupted |= !alive(u64::from(self.pid), self.start);
        self
    }

    /// The share of the tree read, two places: what scales an answer's
    /// confidence while the rest is unread.
    pub(crate) fn coverage(&self) -> f64 {
        if self.of == 0 {
            return 0.0;
        }
        let share = self.read.min(self.of) as f64 / self.of as f64;
        (share * 100.0).floor() / 100.0
    }
}

fn key(root: &str) -> String {
    format!("warming {root}")
}

impl Store {
    /// The asked checkout's index, when it is not whole yet. `None` for a
    /// complete checkout — and for one never indexed, which is `not_indexed`.
    pub(crate) fn warming(&self, root: &str) -> Result<Option<Warming>> {
        let value: Option<String> = match self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                params![key(root)],
                |r| r.get(0),
            )
            .optional()
        {
            Ok(value) => value,
            // An older trekr's store at this version has no `meta`.
            Err(error) if error.to_string().contains("no such table") => None,
            Err(error) => return Err(error),
        };
        Ok(value.and_then(|value| parse(&value)))
    }

    /// Mark `root` as filling, `read` of `of` files visible, by this process.
    pub(crate) fn set_warming(&self, root: &str, read: u64, of: u64) -> Result<()> {
        self.mark(root, &format!("{} {read} {of}{}", std::process::id(), me()))
    }

    /// Mark `root` as filling before its gems are listed: `own` is the
    /// checkout's files alone, and the mark says so ([`Warming::uncounted`]).
    /// A trekr before this one reads the first three fields and ignores the
    /// rest.
    pub(crate) fn begin_warming(&self, root: &str, own: u64) -> Result<()> {
        self.mark(root, &format!("{} 0 {own} own{}", std::process::id(), me()))
    }

    /// Mark `root` as filling by this process — as [`Store::begin_warming`]
    /// with `counted` false, [`Store::set_warming`]'s `0 of own` with it true
    /// — unless another index already fills it: then that one's mark, and
    /// nothing written. One transaction, so of two first indexes started at
    /// once one fills the checkout and the other learns it in time.
    ///
    /// `ours`: another process whose mark is this one's to take — the query
    /// that claimed the checkout for the index it then started.
    pub(crate) fn claim_warming(
        &mut self,
        root: &str,
        own: u64,
        counted: bool,
        ours: Option<u32>,
    ) -> Result<Option<Warming>> {
        let me = std::process::id();
        self.batch(|store| {
            if let Some(other) = store.warming(root)?
                && !other.interrupted
                && other.pid != me
                && Some(other.pid) != ours
            {
                return Ok(Some(other));
            }
            match counted {
                true => store.set_warming(root, 0, own)?,
                false => store.begin_warming(root, own)?,
            }
            Ok(None)
        })
    }

    /// Claim `root` for the first index this process is about to start, so
    /// another query finds it under way rather than starting its own; that
    /// index takes the claim over as its own. Never waits for the write
    /// lock: busy is an error, and the caller starts its index anyway.
    pub(crate) fn claim_for_child(&mut self, root: &str) -> Result<Option<Warming>> {
        self.conn.busy_timeout(std::time::Duration::ZERO)?;
        let claimed = self.claim_warming(root, 0, false, None);
        self.conn.busy_timeout(super::BUSY)?;
        claimed
    }

    fn mark(&self, root: &str, value: &str) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )?;
        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
               ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key(root), value],
        )?;
        Ok(())
    }

    /// `root` is whole: its last rows are in.
    pub(crate) fn clear_warming(&self, root: &str) -> Result<()> {
        match self
            .conn
            .execute("DELETE FROM meta WHERE key = ?1", params![key(root)])
        {
            Err(error) if error.to_string().contains("no such table") => Ok(()),
            done => done.map(|_| ()),
        }
    }
}

/// `pid read of`, then `own` while the gems are uncounted and `start:N` once
/// the writer's start is known, and whether that process is still running.
fn parse(value: &str) -> Option<Warming> {
    let mut parts = value.split(' ');
    let mut number = || parts.next()?.parse::<u64>().ok();
    let (pid, read, of) = (number()?, number()?, number()?);
    let (mut uncounted, mut start) = (false, None);
    for part in parts {
        match part.strip_prefix("start:") {
            Some(at) => start = at.parse().ok(),
            None => uncounted |= part == "own",
        }
    }
    Some(Warming {
        read,
        of,
        interrupted: !alive(pid, start),
        pid: u32::try_from(pid).ok()?,
        uncounted,
        start,
    })
}

/// ` start:N` for this process's mark, or nothing where the kernel won't say.
fn me() -> String {
    static ME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ME.get_or_init(|| {
        started(std::process::id()).map_or_else(String::new, |start| format!(" start:{start}"))
    })
    .clone()
}

/// Is the process with this pid — started at `start`, when the mark says —
/// still running? One that has exited is not, though nobody has reaped it:
/// `kill(pid, 0)` still finds a zombie. One started at another time is the
/// pid reused, not the writer, and so is another user's process under a
/// mark that names a start — a store's writers are its owner's. A mark an
/// older trekr wrote names none, and where the kernel will not say, a pid
/// in use reads as running, which keeps the checkout marked partial until
/// its next index — the safe side of the mistake.
pub(super) fn alive(pid: u64, start: Option<u64>) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 checks for the process and delivers nothing.
    if unsafe { libc::kill(pid, 0) } != 0 {
        let theirs = std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        return theirs && start.is_none();
    }
    match process(pid) {
        Some(found) => !found.zombie && start.is_none_or(|start| start == found.start),
        None => true,
    }
}

/// When the process `pid` started, in the kernel's own units: comparable
/// only with another reading on the same machine.
pub(crate) fn started(pid: u32) -> Option<u64> {
    process(libc::pid_t::try_from(pid).ok()?)
        .filter(|found| !found.zombie)
        .map(|found| found.start)
}

/// A process as the kernel holds it.
struct Process {
    zombie: bool,
    start: u64,
}

#[cfg(target_os = "macos")]
fn process(pid: libc::pid_t) -> Option<Process> {
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: an all-zero proc_bsdinfo is a valid value of a plain C struct.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    // SAFETY: the buffer is `info`, `size` bytes long, written by the kernel.
    let got =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size) };
    if got != size {
        // A process `kill` finds and this cannot is a zombie: its task is gone.
        let error = std::io::Error::last_os_error().raw_os_error();
        return (error == Some(libc::ESRCH)).then_some(Process {
            zombie: true,
            start: 0,
        });
    }
    Some(Process {
        zombie: info.pbi_status == libc::SZOMB,
        start: info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec,
    })
}

#[cfg(target_os = "linux")]
fn process(pid: libc::pid_t) -> Option<Process> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // `pid (comm) state …`: the name may hold spaces and parentheses.
    let mut fields = stat.get(stat.rfind(')')? + 1..)?.split_whitespace();
    let state = fields.next()?;
    // The start time is field 22 of stat(5); the state was field 3.
    let start = fields.nth(18)?.parse().ok()?;
    Some(Process {
        zombie: state == "Z" || state == "X",
        start,
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn process(_pid: libc::pid_t) -> Option<Process> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_marker_reads_back_until_cleared_and_names_a_dead_writer() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.warming("/app").unwrap(), None);
        store.set_warming("/app", 10, 40).unwrap();
        let warming = store.warming("/app").unwrap().unwrap();
        assert_eq!(
            (warming.read, warming.of, warming.interrupted),
            (10, 40, false)
        );
        assert_eq!(warming.coverage(), 0.25);
        assert_eq!(store.warming("/other").unwrap(), None);
        store.clear_warming("/app").unwrap();
        assert_eq!(store.warming("/app").unwrap(), None);

        // A writer that died with the marker set: the checkout is partial,
        // and says why.
        let gone = parse(&format!("{} 1 3", i32::MAX)).unwrap();
        assert!(gone.interrupted);
    }

    #[test]
    fn a_mark_written_before_the_gems_are_listed_says_its_count_is_partial() {
        let store = Store::open_in_memory().unwrap();
        store.begin_warming("/app", 40).unwrap();
        let begun = store.warming("/app").unwrap().unwrap();
        assert_eq!((begun.read, begun.of, begun.uncounted), (0, 40, true));
        store.set_warming("/app", 10, 400).unwrap();
        assert!(!store.warming("/app").unwrap().unwrap().uncounted);
    }

    #[test]
    fn a_marker_read_while_its_writer_ran_says_so_once_it_has_died() {
        let mut writer = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .unwrap();
        let read = parse(&format!("{} 1 3", writer.id())).unwrap();
        assert!(!read.clone().now().interrupted);
        writer.kill().unwrap();
        writer.wait().unwrap();
        assert!(read.now().interrupted);
    }

    #[test]
    fn a_claim_yields_to_a_live_index_and_takes_over_a_dead_one() {
        let mut store = Store::open_in_memory().unwrap();
        assert_eq!(store.claim_warming("/app", 40, false, None).unwrap(), None);
        assert!(store.warming("/app").unwrap().unwrap().uncounted);
        // Our own mark is ours to write again.
        assert_eq!(store.claim_warming("/app", 40, true, None).unwrap(), None);

        let mut writer = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .unwrap();
        store
            .mark("/app", &format!("{} 3 40", writer.id()))
            .unwrap();
        let other = store
            .claim_warming("/app", 40, false, None)
            .unwrap()
            .unwrap();
        assert_eq!((other.pid, other.read), (writer.id(), 3));
        writer.kill().unwrap();
        writer.wait().unwrap();
        assert_eq!(store.claim_warming("/app", 40, false, None).unwrap(), None);
        assert_eq!(
            store.warming("/app").unwrap().unwrap().pid,
            std::process::id()
        );
    }

    #[test]
    fn an_exited_process_nobody_reaped_is_not_running() {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        // Ended, and not yet reaped: a zombie, which `kill(pid, 0)` finds.
        std::thread::sleep(std::time::Duration::from_millis(300));
        let gone = !alive(u64::from(child.id()), None);
        child.wait().unwrap();
        assert!(gone, "a zombie reads as running");
    }

    #[test]
    fn a_mark_whose_pid_now_names_another_process_is_dead() {
        let me = std::process::id();
        let start = started(me).expect("this process's start");
        let mine = parse(&format!("{me} 1 3 own start:{start}")).unwrap();
        assert!(!mine.interrupted);
        assert!(mine.uncounted);
        // The same pid, started at another time: reused since the mark.
        let reused = parse(&format!("{me} 1 3 start:{}", start + 1)).unwrap();
        assert!(reused.interrupted);
        assert!(!reused.uncounted);
        // Another user's process — init's, here — is no writer of ours.
        assert!(parse("1 1 3 start:5").unwrap().interrupted);
        assert!(!parse("1 1 3").unwrap().interrupted);
        // A mark an older trekr wrote has no start, and reads as before.
        let old = parse(&format!("{me} 1 3 own")).unwrap();
        assert!(!old.interrupted && old.uncounted);
        assert_eq!(old.start, None);
    }

    #[test]
    fn a_mark_names_its_writers_start_and_a_claim_takes_over_a_reused_pid() {
        let mut store = Store::open_in_memory().unwrap();
        store.set_warming("/app", 1, 3).unwrap();
        let own = store.warming("/app").unwrap().unwrap();
        assert_eq!(own.start, started(std::process::id()));
        // A live process's pid, written with a start it never had.
        let mut other = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .unwrap();
        store
            .mark("/app", &format!("{} 3 40 start:1", other.id()))
            .unwrap();
        assert!(store.warming("/app").unwrap().unwrap().interrupted);
        assert_eq!(store.claim_warming("/app", 40, false, None).unwrap(), None);
        other.kill().unwrap();
        other.wait().unwrap();
    }

    #[test]
    fn coverage_never_rounds_up_to_whole() {
        let warming = Warming {
            read: 999,
            of: 1000,
            interrupted: false,
            pid: 1,
            uncounted: false,
            start: None,
        };
        assert_eq!(warming.coverage(), 0.99);
    }

    #[test]
    fn a_store_without_meta_is_not_warming() {
        let store = Store::open_in_memory().unwrap();
        store.conn.execute_batch("DROP TABLE meta;").unwrap();
        assert_eq!(store.warming("/app").unwrap(), None);
        store.clear_warming("/app").unwrap();
        store.set_warming("/app", 0, 1).unwrap();
        assert!(store.warming("/app").unwrap().is_some());
    }
}
