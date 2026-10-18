//! The store cleans up after itself (DEC-650).
//!
//! A rebuild or a collection leaves its old rows' pages free inside the file,
//! and a release leaves each checkout not queried since with a tree snapshot
//! no key names. Nobody should have to know `--gc --vacuum` exists. So an
//! index pass, once it is done, measures what could be given back — at most
//! every [`INTERVAL`] — and when that passes the threshold it claims the
//! store's cleanup and starts a detached `trekr --cleanup STORE`, which
//! nothing waits for: a query that spawned the index waits for that
//! process's exit, never for this one's. The claim is made before the spawn,
//! so a pass that fails is not started again by every index after it.

use crate::store::{Held, Store};
use crate::tree::{Overlays, Swept};
use std::path::Path;

/// The least time between two looks at the store, in seconds. Long enough
/// that a busy day of indexes measures a handful of times, short enough that
/// what a rebuild left is gone within a working session.
pub(crate) const INTERVAL: i64 = 2 * 3600;

/// How soon a pass that found the store busy is asked for again, in
/// seconds: the claim it took would otherwise hold off the next for a whole
/// interval.
const RETRY: i64 = 10 * 60;

/// Less than this is not worth a pass: a vacuum rewrites every live page to
/// win it back.
const MIN_WASTE: i64 = 32 * 1024 * 1024;

/// Free pages handed back per write transaction: about 0.1 s each on a 1 GB
/// store, so a writer waits that long at most, not the whole compaction.
const CHUNK_PAGES: i64 = 1024;

/// Blobs collected per write transaction: about as long as a chunk of
/// [`CHUNK_PAGES`] takes — 60 ms on rails, 0.1 s on a 1 GB store (DEC-651).
const BATCH_BLOBS: usize = 100;

/// Between chunks: longer than a waiting writer's longest backoff (100 ms),
/// so a queued index takes its turn rather than waiting out the pass.
const CHUNK_PAUSE: std::time::Duration = std::time::Duration::from_millis(150);

/// How long a version of a file no checkout maps is kept (DEC-651): a
/// query's draft, or one an edit, a branch switch or a `--drop` left behind.
/// Within it, a switch back reuses the facts; after it, the file is read
/// again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Retention {
    /// Every version, forever.
    Never,
    /// This many seconds after nothing maps it.
    For(u64),
}

pub(crate) const DEFAULT_RETENTION: Retention = Retention::For(14 * 86_400);

/// The standing setting; `--retention` overrides it for one `--gc` or
/// `--status`. Every process reads it, the background cleanup included.
pub(crate) const RETENTION_ENV: &str = "TREKR_RETENTION";

impl std::str::FromStr for Retention {
    type Err = String;

    fn from_str(text: &str) -> Result<Retention, String> {
        match text {
            "never" => Ok(Retention::Never),
            _ => parse_age(text)
                .map(Retention::For)
                .map_err(|why| format!("{why}, or `never`")),
        }
    }
}

impl Retention {
    /// The flag, else the environment's value (empty is unset), else the
    /// default.
    pub(crate) fn choose(flag: Option<Retention>, env: Option<&str>) -> Result<Retention, String> {
        match (flag, env) {
            (Some(flag), _) => Ok(flag),
            (None, Some(text)) if !text.is_empty() => text
                .parse()
                .map_err(|why| format!("{RETENTION_ENV}: {why}")),
            (None, _) => Ok(DEFAULT_RETENTION),
        }
    }

    /// [`Retention::choose`] with this process's environment.
    pub(crate) fn configured(flag: Option<Retention>) -> Result<Retention, String> {
        let env = std::env::var(RETENTION_ENV).ok();
        Retention::choose(flag, env.as_deref())
    }

    /// In seconds; `None` for never.
    pub(crate) fn seconds(self) -> Option<u64> {
        match self {
            Retention::Never => None,
            Retention::For(seconds) => Some(seconds),
        }
    }

    /// At `now`, the latest a version may have gone unmapped and be
    /// collected; `None` collects none.
    pub(crate) fn before(self, now: i64) -> Option<i64> {
        self.seconds()
            .map(|seconds| now.saturating_sub(i64::try_from(seconds).unwrap_or(i64::MAX)))
    }
}

/// `36h`, `7d`, `2w` — or `0`. A bare number is refused: minutes and days
/// are both plausible readings, and guessing wrong deletes the wrong thing.
pub(crate) fn parse_age(text: &str) -> Result<u64, String> {
    if text == "0" {
        return Ok(0);
    }
    let split = text.len() - text.chars().last().map_or(0, char::len_utf8);
    let (count, unit) = text.split_at(split);
    let count: u64 = count
        .parse()
        .map_err(|_| format!("`{text}` is not an age like 36h, 7d or 2w"))?;
    let unit = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        "w" => 7 * 86_400,
        _ => return Err(format!("`{text}`: the unit is one of s, m, h, d, w")),
    };
    Ok(count * unit)
}

/// What the store could give back to the disk now.
#[derive(Debug, Default, Clone, Copy, serde::Serialize)]
struct Reclaimable {
    /// Free pages inside the index file: what a vacuum returns.
    pub(crate) free_bytes: i64,
    /// Tree snapshots no checkout's current key names, a query's own aside.
    pub(crate) snapshots: Swept,
}

impl Reclaimable {
    fn measure(store: &Store) -> anyhow::Result<Reclaimable> {
        Ok(Reclaimable {
            free_bytes: store.free_bytes()?,
            snapshots: crate::tree::sweep_snapshots(store, &[], Overlays::Keep, true)?,
        })
    }

    fn bytes(&self) -> i64 {
        self.free_bytes + self.snapshots.bytes as i64
    }
}

/// Worth a pass: at least [`MIN_WASTE`], and a quarter of the file. The
/// share keeps a large store from being rewritten for the churn every index
/// leaves and the next one reuses.
fn worth(waste: i64, file: i64) -> bool {
    waste >= MIN_WASTE && waste * 4 >= file
}

/// After an index pass: start a cleanup of `store` in the background when
/// one is due. Best effort, and never waits for it.
pub(crate) fn after_index(store: &mut Store) {
    let Some(path) = store.path().map(Path::to_path_buf) else {
        return;
    };
    // One the environment cannot give is never: nothing collected on a guess.
    let retention = Retention::configured(None).unwrap_or(Retention::Never);
    match start(store, now(), retention) {
        Ok(true) => {
            if let Err(error) = spawn(&path) {
                log(
                    "cleanup_spawn_failed",
                    serde_json::json!({ "error": error.to_string() }),
                );
            }
        }
        Ok(false) => {}
        Err(error) => log(
            "cleanup_check_failed",
            serde_json::json!({ "error": format!("{error:#}") }),
        ),
    }
}

/// Whether to start a cleanup now: one is [`due`], and this process took the
/// claim for it — which another process asking at once, or the next index
/// pass, then finds taken.
fn start(store: &mut Store, now: i64, retention: Retention) -> anyhow::Result<bool> {
    if !due(store, now, retention)? {
        return Ok(false);
    }
    match store.claim_cleanup(now, INTERVAL) {
        Err(error) if crate::store::is_busy(&error) => Ok(false),
        claimed => Ok(claimed?),
    }
}

/// Whether a cleanup should run: nobody looked within [`INTERVAL`], no first
/// index is filling the store, and what it would reclaim is worth it. A look
/// that finds nothing worth doing is recorded, so the next is an interval
/// away.
fn due(store: &Store, now: i64, retention: Retention) -> anyhow::Result<bool> {
    if store
        .cleanup()?
        .checked_at
        .is_some_and(|at| now - at < INTERVAL)
    {
        return Ok(false);
    }
    if store.any_warming()? {
        return Ok(false);
    }
    // An index the schema gained since this store was laid down: built here,
    // off every query's path, rather than by a rebuild.
    if !store.missing_indexes()?.is_empty() {
        return Ok(true);
    }
    // What an older trekr unmapped, which it did not stamp.
    if store.any_unstamped()? {
        return Ok(true);
    }
    if let Some(before) = retention.before(now)
        && store.any_unmapped_before(before)?
    {
        return Ok(true);
    }
    if worth(Reclaimable::measure(store)?.bytes(), store.db_bytes()?) {
        return Ok(true);
    }
    store.cleanup_checked(now)?;
    Ok(false)
}

/// `trekr --cleanup STORE`: from this binary, detached — its own process
/// group, so a Ctrl-C at the query that started the index does not reach it
/// — and marked background, so it yields as the index does. The store is
/// named, not looked up again: beside a newer trekr's, the index wrote its
/// own side store, which a new process would not know to open.
fn spawn(store: &Path) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    Command::new(std::env::current_exe()?)
        .arg("--cleanup")
        .arg(store)
        .env(crate::background::BACKGROUND, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map(drop)
}

/// What one cleanup did.
#[derive(Debug, Default, serde::Serialize)]
pub(crate) struct Done {
    pub(crate) snapshots: Swept,
    /// Versions no checkout maps, unmapped past the retention (DEC-651).
    pub(crate) blobs: usize,
    pub(crate) compacted: Compacted,
    /// Indexes the schema gained that this store had not got.
    pub(crate) indexes: Vec<&'static str>,
    /// What left the disk: the files removed and the index file's shrink.
    pub(crate) reclaimed_bytes: i64,
}

/// What a pass did with the file's free pages.
#[derive(Debug, Default, Clone, Copy, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Compacted {
    /// Too few to be worth it.
    #[default]
    NotWorth,
    /// Handed back a chunk at a time.
    Incremental,
    /// Rewritten whole: a store from before incremental vacuuming, which
    /// the rewrite switches.
    Full,
    /// Rewriting whole would not fit on the disk.
    NoRoom,
    /// Another writer held the lock past the wait.
    Busy,
}

/// Why a cleanup did nothing.
#[derive(Debug, PartialEq)]
pub(crate) enum Skipped {
    /// A first index is under way, or another writer kept the lock past the
    /// wait.
    Busy,
}

/// Clean up `store`, claimed for this pass by [`start`], unless another
/// process holds it. Writes wait as long as a query's own writes do, never
/// as long as an index's.
pub(crate) fn run(
    store: &mut Store,
    now: i64,
    retention: Retention,
) -> anyhow::Result<Result<Done, Skipped>> {
    let ran = match pass(store, now, retention) {
        Err(error)
            if error
                .downcast_ref::<rusqlite::Error>()
                .is_some_and(crate::store::is_busy) =>
        {
            Ok(Err(Skipped::Busy))
        }
        done => done,
    };
    let busy = match &ran {
        Ok(Err(Skipped::Busy)) => true,
        Ok(Ok(done)) => done.compacted == Compacted::Busy,
        _ => false,
    };
    // Give back most of the claim: the next index asks again in `RETRY`.
    // Best effort — a writer still holding the lock keeps the interval.
    if busy {
        let _ = store.cleanup_checked(now - INTERVAL + RETRY);
    }
    ran
}

fn pass(
    store: &mut Store,
    now: i64,
    retention: Retention,
) -> anyhow::Result<Result<Done, Skipped>> {
    if store.any_warming()? {
        return Ok(Err(Skipped::Busy));
    }
    // In pages: a writer queued behind the vacuum keeps the log from being
    // folded back, so the file may not have shrunk yet when this measures.
    let before = store.db_bytes()?;
    // First: the collection below deletes blobs, which cascade through them.
    let indexes = store.add_missing_indexes()?;
    store.stamp_unmapped()?;
    let mut done = Done {
        indexes,
        blobs: collect_unmapped(store, retention.before(now))?,
        snapshots: crate::tree::sweep_snapshots(store, &[], Overlays::Keep, false)?,
        ..Done::default()
    };
    let (free, file) = (store.free_bytes()?, store.db_bytes()?);
    done.compacted = match () {
        () if !worth(free, file) => Compacted::NotWorth,
        () if store.incremental()? => in_chunks(store)?,
        () if !room_for(store, file - free) => Compacted::NoRoom,
        () => match store.vacuum() {
            Ok(()) => Compacted::Full,
            Err(error) if crate::store::is_busy(&error) => Compacted::Busy,
            Err(error) => return Err(error.into()),
        },
    };
    done.reclaimed_bytes = (before - store.db_bytes()?).max(0) + done.snapshots.bytes as i64;
    record(store, now, &done)?;
    Ok(Ok(done))
}

/// Collect the versions unmapped at or before `before`, a batch per write
/// transaction, pausing between them as [`in_chunks`] does.
fn collect_unmapped(store: &mut Store, before: Option<i64>) -> anyhow::Result<usize> {
    let Some(before) = before else {
        return Ok(0);
    };
    let mut collected = 0;
    loop {
        let batch = store.collect_unmapped(before, BATCH_BLOBS)?.blobs;
        collected += batch;
        if batch < BATCH_BLOBS {
            return Ok(collected);
        }
        std::thread::sleep(CHUNK_PAUSE);
    }
}

/// What `--status` says of a pass: a run, if it gave something back, and why
/// it left free pages it meant to hand back, if it did.
fn record(store: &Store, now: i64, done: &Done) -> rusqlite::Result<()> {
    if done.reclaimed_bytes > 0 {
        store.cleanup_ran(now, done.reclaimed_bytes)?;
    }
    store.cleanup_held(match done.compacted {
        Compacted::NoRoom => Some(Held::NoRoom),
        Compacted::Busy => Some(Held::Busy),
        Compacted::NotWorth | Compacted::Incremental | Compacted::Full => None,
    })
}

/// Hand every free page back, a chunk per write transaction, pausing between
/// them so a waiting writer goes first. A writer that keeps the lock past the
/// wait ends the pass where it is.
fn in_chunks(store: &Store) -> anyhow::Result<Compacted> {
    while store.free_bytes()? > 0 {
        match store.vacuum_step(CHUNK_PAGES) {
            Ok(()) => {}
            Err(error) if crate::store::is_busy(&error) => return Ok(Compacted::Busy),
            Err(error) => return Err(error.into()),
        }
        std::thread::sleep(CHUNK_PAUSE);
    }
    store.truncate_wal();
    Ok(Compacted::Incremental)
}

/// Whether the disks hold what a full vacuum writes on the way: the live
/// pages twice — a temporary copy where SQLite keeps its temporary files,
/// and the rewrite into the log beside the store — and a quarter again.
fn room_for(store: &Store, live: i64) -> bool {
    let Some(dir) = store.path().and_then(Path::parent) else {
        return true;
    };
    let temp = sqlite_temp_dir();
    let (Some(beside), Some(spare)) = (available(dir), available(&temp)) else {
        return false;
    };
    fits(live.max(0) as u64, beside, spare, same_disk(dir, &temp))
}

/// `live` bytes written once beside the store and once to the temporary
/// directory, with a quarter of each to spare.
fn fits(live: u64, beside: u64, temp: u64, same_disk: bool) -> bool {
    let need = live + live / 4;
    match same_disk {
        true => beside >= 2 * need,
        false => beside >= need && temp >= need,
    }
}

/// Where SQLite puts a temporary file: the first of these that is a
/// directory, as `unixTempFileDir` in its os_unix.c looks.
fn sqlite_temp_dir() -> std::path::PathBuf {
    ["SQLITE_TMPDIR", "TMPDIR"]
        .iter()
        .filter_map(std::env::var_os)
        .map(std::path::PathBuf::from)
        .chain(["/var/tmp", "/usr/tmp", "/tmp", "."].map(std::path::PathBuf::from))
        .find(|dir| dir.is_dir())
        .unwrap_or_else(|| ".".into())
}

fn same_disk(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev(),
        _ => true,
    }
}

/// Bytes an unprivileged process may still write on `dir`'s filesystem.
#[allow(
    clippy::useless_conversion,
    reason = "statvfs field widths differ: u32 on macOS, u64 on Linux"
)]
fn available(dir: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `path` is NUL-terminated and outlives the call; `stat` is
    // written by `statvfs` and read only after it reports success.
    let stat = unsafe {
        if libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) != 0 {
            return None;
        }
        stat.assume_init()
    };
    Some(u64::from(stat.f_bavail) * u64::from(stat.f_frsize))
}

pub(crate) fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn log(event: &str, fields: serde_json::Value) {
    crate::log::Log::open(false).event(event, fields);
}

/// The hidden `--cleanup STORE`: one pass over the store an index named,
/// said only in the log.
pub(crate) fn command(path: &Path) {
    crate::background::yield_if_background();
    // Never a new store: the one the index wrote may be gone by now.
    let mut store = match Store::open_existing(path) {
        Ok(store) => store,
        Err(error) => {
            return log(
                "cleanup_failed",
                serde_json::json!({ "store": path, "error": error.to_string() }),
            );
        }
    };
    let retention = Retention::configured(None).unwrap_or_else(|why| {
        log("cleanup_retention", serde_json::json!({ "error": why }));
        Retention::Never
    });
    let started = std::time::Instant::now();
    let ran = run(&mut store, now(), retention);
    let ms = started.elapsed().as_millis() as u64;
    match ran {
        Ok(Ok(done)) => log("cleanup", serde_json::json!({ "done": done, "ms": ms })),
        Ok(Err(skipped)) => log(
            "cleanup_skipped",
            serde_json::json!({ "why": format!("{skipped:?}") }),
        ),
        Err(error) => log(
            "cleanup_failed",
            serde_json::json!({ "error": format!("{error:#}"), "ms": ms }),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const NOW: i64 = 1_800_000_000;
    const KEEP: Retention = DEFAULT_RETENTION;

    /// A store on disk holding one indexed checkout, its tree snapshot built.
    fn store(name: &str) -> (PathBuf, Store) {
        let dir = std::env::temp_dir().join(format!("trekr-cleanup-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut store = Store::open(&dir.join("t.db")).unwrap();
        let src = "class Widget\nend\n";
        let oid = crate::scan::hash_blob(src.as_bytes());
        let files = crate::scan::Files::from([("widget.rb".to_string(), oid.clone())]);
        store
            .write(
                "/app",
                &files,
                vec![(oid, crate::extract::extract(src.as_bytes()))],
                0,
            )
            .unwrap();
        crate::tree::Tree::build(&store, "/app").unwrap();
        (dir, store)
    }

    /// Leave `mb` megabytes of free pages in the file, as a rebuild does.
    fn bloat(dir: &Path, mb: u32) {
        let conn = rusqlite::Connection::open(dir.join("t.db")).unwrap();
        conn.execute_batch(&format!(
            "CREATE TABLE junk (b BLOB);
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {mb})
             INSERT INTO junk SELECT zeroblob(1048576) FROM n;
             DROP TABLE junk;
             PRAGMA wal_checkpoint(TRUNCATE);"
        ))
        .unwrap();
    }

    fn snapshots(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir.join("t.trees"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_new_store_waits_an_interval_before_its_first_look() {
        let (dir, store) = store("new");
        bloat(&dir, 40);
        let created = store
            .cleanup()
            .unwrap()
            .checked_at
            .expect("stamped at creation");
        assert!(
            !due(&store, created + 60, KEEP).unwrap(),
            "looked at just now"
        );
        assert!(due(&store, created + INTERVAL, KEEP).unwrap());
        // A store an earlier trekr rebuilt carries no stamp: due at once.
        rusqlite::Connection::open(dir.join("t.db"))
            .unwrap()
            .execute("DELETE FROM meta WHERE key = 'cleanup checked'", [])
            .unwrap();
        assert!(due(&store, created + 60, KEEP).unwrap());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn only_waste_past_the_threshold_is_worth_a_pass() {
        let (dir, store) = store("threshold");
        store.cleanup_checked(0).unwrap();
        assert!(!due(&store, NOW, KEEP).unwrap(), "a small store's churn");
        assert_eq!(
            store.cleanup().unwrap().checked_at,
            Some(NOW),
            "a look that found nothing waits an interval"
        );
        assert!(!due(&store, NOW + INTERVAL - 1, KEEP).unwrap());
        bloat(&dir, 40);
        assert!(due(&store, NOW + INTERVAL, KEEP).unwrap());
        assert_eq!(
            store.cleanup().unwrap().checked_at,
            Some(NOW),
            "a due look leaves the claim to the cleanup"
        );
        // Plenty of bytes, but a small share of a large file: not worth it.
        assert!(!worth(MIN_WASTE * 2, MIN_WASTE * 9));
        assert!(!worth(MIN_WASTE - 1, MIN_WASTE));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_pass_compacts_the_file_and_sweeps_what_no_key_names() {
        let (dir, mut store) = store("pass");
        bloat(&dir, 40);
        let live = snapshots(&dir);
        let stale = dir
            .join("t.trees")
            .join(format!("{}-{}.tree", &live[0][..16], "0".repeat(40)));
        std::fs::write(&stale, vec![0u8; 4096]).unwrap();
        // A query's tree over its checkout's edits: a live one's, not stale.
        let overlay = format!("{}+{}.tree", &live[0][..16], "1".repeat(40));
        std::fs::write(dir.join("t.trees").join(&overlay), vec![0u8; 1024]).unwrap();
        assert_eq!(Reclaimable::measure(&store).unwrap().snapshots.files, 1);
        let before = store.db_bytes().unwrap();
        store.cleanup_checked(0).unwrap();
        assert!(start(&mut store, NOW, KEEP).unwrap());

        let done = run(&mut store, NOW, KEEP).unwrap().expect("ran");
        assert_eq!(
            done.compacted,
            Compacted::Incremental,
            "a new store's pages go back in chunks"
        );
        assert_eq!(done.snapshots.files, 1);
        let mut kept = live.clone();
        kept.push(overlay);
        kept.sort();
        assert_eq!(
            snapshots(&dir),
            kept,
            "the stale one went; the live one and the query's stayed"
        );
        let after = store.db_bytes().unwrap();
        assert!(after < before - 32_000_000, "{before} -> {after}");
        // Measured by the pass itself, around its own bookkeeping write.
        let seen = before - after + 4096;
        assert!(
            done.reclaimed_bytes.abs_diff(seen) < 65_536,
            "{} vs {seen}",
            done.reclaimed_bytes
        );
        let cleanup = store.cleanup().unwrap();
        assert_eq!(
            (cleanup.checked_at, cleanup.ran_at, cleanup.reclaimed_bytes),
            (Some(NOW), Some(NOW), Some(done.reclaimed_bytes))
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Another connection reading through the vacuum keeps the log from
    /// being folded back, so the file has not shrunk yet when the pass
    /// measures: what it gave back is counted in pages, not in file size.
    #[test]
    fn a_pass_counts_what_it_reclaimed_though_the_file_shrinks_later() {
        let (dir, mut store) = store("reader");
        bloat(&dir, 40);
        store.cleanup_checked(0).unwrap();
        assert!(start(&mut store, NOW, KEEP).unwrap());
        store.wait_briefly();
        let reader = rusqlite::Connection::open(dir.join("t.db")).unwrap();
        reader.execute_batch("BEGIN").unwrap();
        let _: i64 = reader
            .query_row("SELECT COUNT(*) FROM checkout", [], |r| r.get(0))
            .unwrap();
        let done = run(&mut store, NOW, KEEP).unwrap().expect("ran");
        reader.execute_batch("COMMIT").unwrap();
        assert_ne!(done.compacted, Compacted::NotWorth);
        assert!(
            done.reclaimed_bytes >= 40_000_000,
            "{}",
            done.reclaimed_bytes
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A store from before incremental vacuuming is rewritten once, which
    /// switches it: the next pass hands pages back in chunks.
    #[test]
    fn an_older_store_is_switched_by_one_full_vacuum() {
        let (dir, mut store) = store("switch");
        {
            let conn = rusqlite::Connection::open(dir.join("t.db")).unwrap();
            conn.execute_batch("PRAGMA auto_vacuum = NONE; VACUUM;")
                .unwrap();
        }
        assert!(!store.incremental().unwrap());
        bloat(&dir, 40);
        store.cleanup_checked(0).unwrap();
        assert!(start(&mut store, NOW, KEEP).unwrap());
        assert_eq!(
            run(&mut store, NOW, KEEP).unwrap().unwrap().compacted,
            Compacted::Full
        );
        assert!(store.incremental().unwrap());
        assert_eq!(store.free_bytes().unwrap(), 0);

        bloat(&dir, 40);
        assert!(start(&mut store, NOW + INTERVAL, KEEP).unwrap());
        let done = run(&mut store, NOW + INTERVAL, KEEP).unwrap().unwrap();
        assert_eq!(done.compacted, Compacted::Incremental);
        assert_eq!(store.free_bytes().unwrap(), 0);
        assert!(
            done.reclaimed_bytes >= 40_000_000,
            "{}",
            done.reclaimed_bytes
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_full_vacuum_needs_the_live_pages_twice_and_a_quarter_spare() {
        const GB: u64 = 1 << 30;
        let cases = [
            // live, beside the store, temporary dir, one disk, fits
            (GB, 3 * GB, 3 * GB, true, true),
            (GB, 2 * GB, 2 * GB, true, false),
            (GB, 2 * GB, 2 * GB, false, true),
            (GB, 2 * GB, GB, false, false),
            (GB, GB, 2 * GB, false, false),
        ];
        for (live, beside, temp, one, fit) in cases {
            assert_eq!(
                fits(live, beside, temp, one),
                fit,
                "{live} {beside} {temp} {one}"
            );
        }
    }

    /// A pass that gave nothing back — its rewrite skipped for room, say —
    /// is not reported as a run that reclaimed 0 bytes.
    #[test]
    fn a_pass_that_reclaims_nothing_is_not_recorded_as_a_run() {
        let (_dir, mut store) = store("nothing");
        let done = run(&mut store, NOW, KEEP).unwrap().unwrap();
        assert_eq!(
            (done.compacted, done.reclaimed_bytes),
            (Compacted::NotWorth, 0)
        );
        assert_eq!(store.cleanup().unwrap().ran_at, None);
    }

    /// A pass that found the store busy after its claim is asked again
    /// soon, not an interval later.
    #[test]
    fn a_busy_pass_is_retried_soon() {
        let (dir, mut store) = store("retry");
        bloat(&dir, 40);
        store.cleanup_checked(0).unwrap();
        assert!(start(&mut store, NOW, KEEP).unwrap());
        store.set_warming("/other", 1, 10).unwrap();
        assert_eq!(
            run(&mut store, NOW, KEEP).unwrap().unwrap_err(),
            Skipped::Busy
        );
        store.clear_warming("/other").unwrap();
        assert!(!start(&mut store, NOW + RETRY - 1, KEEP).unwrap());
        assert!(start(&mut store, NOW + RETRY, KEEP).unwrap());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A store laid down before an index joined the schema is due for a
    /// pass with nothing else to reclaim, and the pass adds it.
    #[test]
    fn a_pass_adds_an_index_the_store_lacks() {
        let (dir, mut store) = store("index");
        rusqlite::Connection::open(dir.join("t.db"))
            .unwrap()
            .execute_batch("DROP INDEX body_call_blob")
            .unwrap();
        store.cleanup_checked(0).unwrap();
        assert!(
            start(&mut store, NOW, KEEP).unwrap(),
            "due for the index alone"
        );
        let done = run(&mut store, NOW, KEEP).unwrap().unwrap();
        assert_eq!(done.indexes, ["body_call_blob"]);
        assert!(store.missing_indexes().unwrap().is_empty());
        assert!(
            !due(&store, NOW + INTERVAL, KEEP).unwrap(),
            "and nothing after"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A pass that left the free pages for want of room says so to
    /// `--status`, until a later pass hands them back.
    #[test]
    fn why_a_pass_left_the_file_is_kept_until_one_compacts_it() {
        let (dir, store) = store("held");
        let mut done = Done {
            compacted: Compacted::NoRoom,
            ..Done::default()
        };
        record(&store, NOW, &done).unwrap();
        assert_eq!(store.cleanup().unwrap().skipped, Some(Held::NoRoom));
        done.compacted = Compacted::Incremental;
        done.reclaimed_bytes = 1;
        record(&store, NOW, &done).unwrap();
        assert_eq!(store.cleanup().unwrap().skipped, None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_pass_is_started_at_most_once_an_interval() {
        let (dir, mut store) = store("rate");
        bloat(&dir, 40);
        store.cleanup_checked(0).unwrap();
        assert!(start(&mut store, NOW, KEEP).unwrap());
        assert!(
            !start(&mut store, NOW + 1, KEEP).unwrap(),
            "claimed: a pass that then fails is not started again by the next index"
        );
        assert!(run(&mut store, NOW, KEEP).unwrap().is_ok());
        bloat(&dir, 40);
        assert!(!start(&mut store, NOW + INTERVAL - 1, KEEP).unwrap());
        assert!(start(&mut store, NOW + INTERVAL, KEEP).unwrap());
        assert_ne!(
            run(&mut store, NOW + INTERVAL, KEEP)
                .unwrap()
                .unwrap()
                .compacted,
            Compacted::NotWorth
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_store_another_process_holds_is_left_for_the_next_pass() {
        let (dir, mut store) = store("busy");
        bloat(&dir, 40);
        store.cleanup_checked(0).unwrap();
        store.wait_briefly();
        let writer = rusqlite::Connection::open(dir.join("t.db")).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        assert!(
            !start(&mut store, NOW, KEEP).unwrap(),
            "no claim without the lock"
        );
        assert_eq!(
            run(&mut store, NOW, KEEP).unwrap().unwrap_err(),
            Skipped::Busy
        );
        writer.execute_batch("ROLLBACK").unwrap();
        assert_eq!(
            store.cleanup().unwrap().checked_at,
            Some(0),
            "the lock still held, nothing recorded: the next index pass asks again"
        );

        // A first index under way, in a live process: this one.
        store.set_warming("/other", 1, 10).unwrap();
        assert!(!due(&store, NOW, KEEP).unwrap());
        assert_eq!(
            run(&mut store, NOW, KEEP).unwrap().unwrap_err(),
            Skipped::Busy
        );
        store.clear_warming("/other").unwrap();
        assert!(start(&mut store, NOW + RETRY, KEEP).unwrap());
        assert_ne!(
            run(&mut store, NOW + RETRY, KEEP)
                .unwrap()
                .unwrap()
                .compacted,
            Compacted::NotWorth
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    fn real_now() -> i64 {
        now()
    }

    /// SQL on the store `store` laid down in `dir`, from beside it.
    fn sql(dir: &Path, sql: &str) {
        rusqlite::Connection::open(dir.join("t.db"))
            .unwrap()
            .execute_batch(sql)
            .unwrap();
    }

    /// As if every version nothing maps had gone unmapped long ago.
    fn age_unmapped(dir: &Path) {
        sql(dir, "UPDATE loose_blob SET recorded_at = 0;");
    }

    /// `store`'s checkout with one more version of its file, the one before
    /// edited away: stamped unmapped just now, or long ago when `aged`.
    fn edit(dir: &Path, store: &mut Store, src: &str, aged: bool) {
        let oid = crate::scan::hash_blob(src.as_bytes());
        let files = crate::scan::Files::from([("widget.rb".to_string(), oid.clone())]);
        store
            .write(
                "/app",
                &files,
                vec![(oid, crate::extract::extract(src.as_bytes()))],
                0,
            )
            .unwrap();
        if aged {
            age_unmapped(dir);
        }
    }

    #[test]
    fn retention_reads_an_age_or_never() {
        let cases = [
            ("14d", Ok(Retention::For(14 * 86_400))),
            ("2w", Ok(Retention::For(14 * 86_400))),
            ("36h", Ok(Retention::For(36 * 3600))),
            ("0", Ok(Retention::For(0))),
            ("never", Ok(Retention::Never)),
            ("off", Err(())),
            ("14", Err(())),
            ("", Err(())),
            ("-1d", Err(())),
        ];
        for (text, want) in cases {
            assert_eq!(text.parse::<Retention>().map_err(drop), want, "{text}");
        }
        assert_eq!(DEFAULT_RETENTION, Retention::For(14 * 86_400));
    }

    #[test]
    fn the_flag_wins_then_the_environment_then_the_default() {
        let month = Retention::For(30 * 86_400);
        let cases = [
            (Some(Retention::Never), Some("30d"), Ok(Retention::Never)),
            (Some(Retention::Never), Some("bogus"), Ok(Retention::Never)),
            (None, Some("30d"), Ok(month)),
            (None, Some("never"), Ok(Retention::Never)),
            (None, Some(""), Ok(DEFAULT_RETENTION)),
            (None, None, Ok(DEFAULT_RETENTION)),
        ];
        for (flag, env, want) in cases {
            assert_eq!(Retention::choose(flag, env), want, "{flag:?} {env:?}");
        }
        let bad = Retention::choose(None, Some("off")).unwrap_err();
        assert!(
            bad.contains("TREKR_RETENTION") && bad.contains("never"),
            "{bad}"
        );
    }

    #[test]
    fn a_version_unmapped_past_the_retention_makes_a_pass_due() {
        let (dir, mut store) = store("expired");
        let at = real_now();
        store.cleanup_checked(at - INTERVAL).unwrap();
        edit(&dir, &mut store, "class Widget\n  def a; end\nend\n", false);
        assert!(!due(&store, at, KEEP).unwrap(), "within the retention");
        store.cleanup_checked(at - INTERVAL).unwrap();
        age_unmapped(&dir);
        assert!(!due(&store, at, Retention::Never).unwrap(), "kept forever");
        store.cleanup_checked(at - INTERVAL).unwrap();
        assert!(due(&store, at, KEEP).unwrap());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_pass_collects_what_went_unmapped_past_the_retention_and_keeps_the_rest() {
        let (dir, mut store) = store("collects");
        edit(&dir, &mut store, "class Widget\n  def a; end\nend\n", true);
        edit(&dir, &mut store, "class Widget\n  def b; end\nend\n", false);
        let before = store.totals().unwrap().blobs;
        let done = run(&mut store, real_now(), KEEP).unwrap().unwrap();
        assert_eq!(done.blobs, 1, "the first version went, the second stays");
        assert_eq!(store.totals().unwrap().blobs, before - 1);
        assert_eq!(store.retained(None).unwrap().blobs, 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_version_an_older_build_unmapped_makes_a_pass_due_and_loses_nothing_that_day() {
        let (dir, mut store) = store("legacy");
        let at = real_now();
        for round in ["a", "b"] {
            edit(
                &dir,
                &mut store,
                &format!("class Widget\n  def {round}; end\nend\n"),
                true,
            );
            // As 0.9.5 leaves what it unmaps: no stamp — before this build
            // first ran, and again after.
            sql(&dir, "DELETE FROM loose_blob;");
            store.cleanup_checked(at - INTERVAL).unwrap();
            assert!(due(&store, at, KEEP).unwrap(), "due to stamp: {round}");
            let done = run(&mut store, at, KEEP).unwrap().unwrap();
            assert_eq!(done.blobs, 0, "stamped as of now: {round}");
            assert!(!store.any_unstamped().unwrap());
        }
        assert_eq!(store.retained(None).unwrap().blobs, 2);
        let _ = std::fs::remove_dir_all(dir);
    }
}
