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

use crate::store::Store;
use crate::tree::{Overlays, Swept};
use std::path::Path;

/// The least time between two looks at the store, in seconds. Long enough
/// that a busy day of indexes measures a handful of times, short enough that
/// what a rebuild left is gone within a working session.
pub(crate) const INTERVAL: i64 = 2 * 3600;

/// Less than this is not worth a pass: a vacuum rewrites every live page to
/// win it back.
const MIN_WASTE: i64 = 32 * 1024 * 1024;

/// Free pages handed back per write transaction: about 0.1 s each on a 1 GB
/// store, so a writer waits that long at most, not the whole compaction.
const CHUNK_PAGES: i64 = 1024;

/// Between chunks: longer than a waiting writer's longest backoff (100 ms),
/// so a queued index takes its turn rather than waiting out the pass.
const CHUNK_PAUSE: std::time::Duration = std::time::Duration::from_millis(150);

/// A blob only a query recorded is kept this long, as `--gc` keeps it by
/// default (DEC-049).
const LOOSE_AGE: i64 = 7 * 86_400;

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
    match start(store, now()) {
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
fn start(store: &mut Store, now: i64) -> anyhow::Result<bool> {
    if !due(store, now)? {
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
fn due(store: &Store, now: i64) -> anyhow::Result<bool> {
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
    /// Blobs only a query recorded, no checkout maps.
    pub(crate) blobs: usize,
    pub(crate) compacted: Compacted,
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
pub(crate) fn run(store: &mut Store, now: i64) -> anyhow::Result<Result<Done, Skipped>> {
    match pass(store, now) {
        Err(error)
            if error
                .downcast_ref::<rusqlite::Error>()
                .is_some_and(crate::store::is_busy) =>
        {
            Ok(Err(Skipped::Busy))
        }
        done => done,
    }
}

fn pass(store: &mut Store, now: i64) -> anyhow::Result<Result<Done, Skipped>> {
    if store.any_warming()? {
        return Ok(Err(Skipped::Busy));
    }
    // In pages: a writer queued behind the vacuum keeps the log from being
    // folded back, so the file may not have shrunk yet when this measures.
    let before = store.db_bytes()?;
    let mut done = Done {
        blobs: store.collect_loose(now - LOOSE_AGE)?.blobs,
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
    // A run is one that gave something back; why one did not is the log's.
    if done.reclaimed_bytes > 0 {
        store.cleanup_ran(now, done.reclaimed_bytes)?;
    }
    Ok(Ok(done))
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

fn now() -> i64 {
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
    let started = std::time::Instant::now();
    let ran = run(&mut store, now());
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
        assert!(!due(&store, created + 60).unwrap(), "looked at just now");
        assert!(due(&store, created + INTERVAL).unwrap());
        // A store an earlier trekr rebuilt carries no stamp: due at once.
        rusqlite::Connection::open(dir.join("t.db"))
            .unwrap()
            .execute("DELETE FROM meta WHERE key = 'cleanup checked'", [])
            .unwrap();
        assert!(due(&store, created + 60).unwrap());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn only_waste_past_the_threshold_is_worth_a_pass() {
        let (dir, store) = store("threshold");
        store.cleanup_checked(0).unwrap();
        assert!(!due(&store, NOW).unwrap(), "a small store's churn");
        assert_eq!(
            store.cleanup().unwrap().checked_at,
            Some(NOW),
            "a look that found nothing waits an interval"
        );
        assert!(!due(&store, NOW + INTERVAL - 1).unwrap());
        bloat(&dir, 40);
        assert!(due(&store, NOW + INTERVAL).unwrap());
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
        assert!(start(&mut store, NOW).unwrap());

        let done = run(&mut store, NOW).unwrap().expect("ran");
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
        assert!(start(&mut store, NOW).unwrap());
        store.wait_briefly();
        let reader = rusqlite::Connection::open(dir.join("t.db")).unwrap();
        reader.execute_batch("BEGIN").unwrap();
        let _: i64 = reader
            .query_row("SELECT COUNT(*) FROM checkout", [], |r| r.get(0))
            .unwrap();
        let done = run(&mut store, NOW).unwrap().expect("ran");
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
        assert!(start(&mut store, NOW).unwrap());
        assert_eq!(
            run(&mut store, NOW).unwrap().unwrap().compacted,
            Compacted::Full
        );
        assert!(store.incremental().unwrap());
        assert_eq!(store.free_bytes().unwrap(), 0);

        bloat(&dir, 40);
        assert!(start(&mut store, NOW + INTERVAL).unwrap());
        let done = run(&mut store, NOW + INTERVAL).unwrap().unwrap();
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
        let done = run(&mut store, NOW).unwrap().unwrap();
        assert_eq!(
            (done.compacted, done.reclaimed_bytes),
            (Compacted::NotWorth, 0)
        );
        assert_eq!(store.cleanup().unwrap().ran_at, None);
    }

    #[test]
    fn a_pass_is_started_at_most_once_an_interval() {
        let (dir, mut store) = store("rate");
        bloat(&dir, 40);
        store.cleanup_checked(0).unwrap();
        assert!(start(&mut store, NOW).unwrap());
        assert!(
            !start(&mut store, NOW + 1).unwrap(),
            "claimed: a pass that then fails is not started again by the next index"
        );
        assert!(run(&mut store, NOW).unwrap().is_ok());
        bloat(&dir, 40);
        assert!(!start(&mut store, NOW + INTERVAL - 1).unwrap());
        assert!(start(&mut store, NOW + INTERVAL).unwrap());
        assert_ne!(
            run(&mut store, NOW + INTERVAL).unwrap().unwrap().compacted,
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
            !start(&mut store, NOW).unwrap(),
            "no claim without the lock"
        );
        assert_eq!(run(&mut store, NOW).unwrap().unwrap_err(), Skipped::Busy);
        writer.execute_batch("ROLLBACK").unwrap();
        assert_eq!(
            store.cleanup().unwrap().checked_at,
            Some(0),
            "nothing recorded: the next index pass asks again"
        );

        // A first index under way, in a live process: this one.
        store.set_warming("/other", 1, 10).unwrap();
        assert!(!due(&store, NOW).unwrap());
        assert_eq!(run(&mut store, NOW).unwrap().unwrap_err(), Skipped::Busy);
        store.clear_warming("/other").unwrap();
        assert!(start(&mut store, NOW).unwrap());
        assert_ne!(
            run(&mut store, NOW).unwrap().unwrap().compacted,
            Compacted::NotWorth
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
