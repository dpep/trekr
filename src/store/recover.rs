//! Opening a store that can't be used as it is (DEC-300), rq's D51 in
//! trekr's terms.
//!
//! The store is a cache (DEC-009), so a damaged file or a rebuild that fails
//! is set aside and built again rather than reported until someone deletes it
//! by hand. A store a *newer* trekr wrote is never touched: this trekr keeps
//! one of its own beside it, so two installed versions don't take turns
//! wiping each other's index.

use std::fs::{File, OpenOptions};
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rusqlite::{Connection, ErrorCode, Result, ffi};

use super::Store;
use super::schema::Layout;

/// Why [`Store::init`] couldn't hand back a store.
pub(super) enum Refusal {
    /// Written by a newer trekr, at this schema version.
    Newer(i64),
    /// Damaged, or a rebuild that failed: set it aside and start over.
    Broken {
        /// The version a failed rebuild started from.
        from: Option<i64>,
        reason: String,
    },
    /// Anything a fresh file wouldn't fix (busy, disk full, permissions).
    Failed(rusqlite::Error),
}

impl From<rusqlite::Error> for Refusal {
    fn from(e: rusqlite::Error) -> Refusal {
        if damaged(&e) {
            Refusal::Broken {
                from: None,
                reason: short(&e),
            }
        } else {
            Refusal::Failed(e)
        }
    }
}

impl Refusal {
    /// A failed rebuild: whatever it hit, short of the environment.
    pub(super) fn upgrade(from: i64, e: rusqlite::Error) -> Refusal {
        if environmental(&e) {
            return Refusal::Failed(e);
        }
        Refusal::Broken {
            from: Some(from),
            reason: format!("from v{from}: {}", short(&e)),
        }
    }
}

/// The file itself is unreadable as a database.
fn damaged(e: &rusqlite::Error) -> bool {
    matches!(e, rusqlite::Error::SqliteFailure(f, _)
        if matches!(f.code, ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase))
}

/// A failure a new file would meet too, so moving this one aside can't help.
fn environmental(e: &rusqlite::Error) -> bool {
    matches!(e, rusqlite::Error::SqliteFailure(f, _) if matches!(
        f.code,
        ErrorCode::DatabaseBusy
            | ErrorCode::DatabaseLocked
            | ErrorCode::DiskFull
            | ErrorCode::SystemIoFailure
            | ErrorCode::OutOfMemory
            | ErrorCode::ReadOnly
            | ErrorCode::CannotOpen
            | ErrorCode::PermissionDenied
            | ErrorCode::OperationInterrupted
            | ErrorCode::NoLargeFileSupport
    ))
}

/// SQLite's message without the SQL it came from.
fn short(e: &rusqlite::Error) -> String {
    match e {
        rusqlite::Error::SqliteFailure(_, Some(msg)) => msg.clone(),
        rusqlite::Error::SqliteFailure(f, None) => f.to_string(),
        rusqlite::Error::SqlInputError { msg, .. } => msg.clone(),
        other => other.to_string(),
    }
}

fn failure(code: std::os::raw::c_int, message: String) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(ffi::Error::new(code), Some(message))
}

/// Open `path` at `layout`'s schema, recovering what can be recovered.
pub(super) fn open(path: &Path, layout: &Layout) -> Result<Store> {
    open_as(path, layout, true)
}

fn open_as(path: &Path, layout: &Layout, main: bool) -> Result<Store> {
    // The version a store set aside here held, once it has been.
    let mut set_aside: Option<i64> = None;
    loop {
        let before = identity(path);
        let refusal = {
            // Shared while opening: setting a file aside takes it exclusively,
            // so nothing opens the old file between its parts moving.
            let _open = Lock::take(path, false)?;
            match Store::init(Connection::open(path)?, layout) {
                Ok((mut store, dropped)) => {
                    store.path = Some(path.to_path_buf());
                    store.file = identity(path);
                    if let Some(from) = set_aside {
                        let _ = store.record_rebuild(from);
                    }
                    if main && dropped.is_some() {
                        sweep_side_stores(path, layout.version);
                    }
                    if dropped.is_some_and(|from| from != 0) {
                        super::sweep_core_after_upgrade(path, &store);
                    }
                    return Ok(store);
                }
                Err(refusal) => refusal,
            }
        };
        match refusal {
            Refusal::Failed(e) => return Err(e),
            Refusal::Newer(version) if main => return side(path, layout, version),
            Refusal::Newer(version) => {
                return Err(super::schema_mismatch(format!(
                    "{} is schema v{version}, newer than this trekr's v{}",
                    path.display(),
                    layout.version
                )));
            }
            Refusal::Broken { reason, .. } if set_aside.is_some() => {
                return Err(failure(ffi::SQLITE_CORRUPT, reason));
            }
            Refusal::Broken { from, reason } => {
                set_aside = Some(from.unwrap_or(layout.version));
                let _only = Lock::take(path, true)?;
                // Another opener already did it: open whatever is there now.
                if identity(path) != before {
                    continue;
                }
                let to = quarantine(path)?;
                let what = if from.is_some() { "upgraded" } else { "read" };
                eprintln!(
                    "trekr: the index couldn't be {what} ({reason}); rebuilding it — the old file is at {}",
                    to.display()
                );
            }
        }
    }
}

/// The side store this process opened in place of a newer trekr's, keyed by
/// the path it stands in for.
static SIDE: Mutex<Option<(PathBuf, PathBuf)>> = Mutex::new(None);

/// The store this process reads for `path`: its side store, when a newer
/// trekr owns `path`. What sits beside the store (core's files) follows it.
pub(crate) fn in_use(path: &Path) -> PathBuf {
    match &*SIDE.lock().unwrap_or_else(|e| e.into_inner()) {
        Some((main, side)) if main == path => side.clone(),
        _ => path.to_path_buf(),
    }
}

/// The store this trekr keeps while a newer one owns `path`.
fn side(path: &Path, layout: &Layout, newer: i64) -> Result<Store> {
    let side = side_path(path, layout.version);
    let first = !side.exists();
    let store = open_as(&side, layout, false)?;
    *SIDE.lock().unwrap_or_else(|e| e.into_inner()) = Some((path.to_path_buf(), side.clone()));
    if first {
        let by = written_by(path).unwrap_or_else(|| format!("store v{newer}"));
        eprintln!(
            "trekr: {} was written by a newer trekr ({by}); using {} beside it for this version",
            path.display(),
            file_name(&side)
        );
    }
    Ok(store)
}

/// `trekr.db` → `trekr.v51.db`.
pub(crate) fn side_path(path: &Path, version: i64) -> PathBuf {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let name = match path.extension() {
        Some(ext) => format!("{stem}.v{version}.{}", ext.to_string_lossy()),
        None => format!("{stem}.v{version}"),
    };
    path.with_file_name(name)
}

/// Which trekr built a store, read without writing to it.
fn written_by(path: &Path) -> Option<String> {
    let conn =
        Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    conn.query_row("SELECT value FROM meta WHERE key = 'schema_by'", [], |r| {
        r.get(0)
    })
    .ok()
}

/// The file at `path` now, so a second opener can tell it was replaced.
pub(super) fn identity(path: &Path) -> Option<(u64, u64)> {
    std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

/// Move the store and its WAL to `<name>.broken-<unix time>`, keeping only
/// the newest such copy. The shared-memory file is rebuilt from the WAL, so it
/// goes.
fn quarantine(path: &Path) -> Result<PathBuf> {
    let name = file_name(path);
    let prefix = format!("{name}.broken-");
    for old in siblings(path) {
        if file_name(&old).starts_with(&prefix) {
            let _ = std::fs::remove_file(&old);
        }
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let to = path.with_file_name(format!("{prefix}{now}"));
    // The WAL before the store: a new file must never find the old WAL.
    for (from, dest) in [
        (with_suffix(path, "-wal"), with_suffix(&to, "-wal")),
        (path.to_path_buf(), to.clone()),
    ] {
        match std::fs::rename(&from, &dest) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(failure(
                    ffi::SQLITE_IOERR,
                    format!("can't move aside {}: {e}", from.display()),
                ));
            }
        }
    }
    let _ = std::fs::remove_file(with_suffix(path, "-shm"));
    Ok(to)
}

/// How long an older trekr's own store outlives its last use.
const SIDE_STORE_IDLE: Duration = Duration::from_secs(30 * 24 * 3600);

/// After laying down a schema at `path`: this version's own side store is
/// stale now, and an older one unused for a month is abandoned. Probed by
/// name: a store may sit in a directory of many thousands of files.
fn sweep_side_stores(path: &Path, version: i64) {
    for v in 1..=version {
        let side = side_path(path, v);
        if !side.exists() {
            continue;
        }
        let idle = || {
            [side.clone(), with_suffix(&side, "-wal")]
                .iter()
                .filter_map(|f| std::fs::metadata(f).and_then(|m| m.modified()).ok())
                .max()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > SIDE_STORE_IDLE)
        };
        if v == version || idle() {
            for suffix in ["", "-wal", "-shm", ".lock"] {
                let _ = std::fs::remove_file(with_suffix(&side, suffix));
            }
            let _ = std::fs::remove_dir_all(super::core_dir_of(&side));
        }
    }
}

/// A file kept beside the store: another version's own store, or a damaged
/// one set aside. Nothing removes these on its own sooner than a month idle,
/// so `--status` lists them and `--gc` removes them.
#[derive(Debug, serde::Serialize)]
pub(crate) struct Kept {
    pub(crate) path: String,
    /// `side` (with the schema `version` it holds) or `broken`.
    pub(crate) kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) version: Option<i64>,
    pub(crate) bytes: u64,
    /// Seconds since it was last written.
    pub(crate) idle: u64,
    /// The store this trekr reads: listed, never removed.
    pub(crate) in_use: bool,
}

/// What sits beside `path`. Side stores are probed by name, as the sweep does.
pub(crate) fn kept(path: &Path, version: i64) -> Vec<Kept> {
    let using = in_use(path);
    let mut kept: Vec<Kept> = (1..=version + 64)
        .map(|v| (side_path(path, v), v))
        .filter(|(side, _)| side.exists())
        .map(|(side, v)| Kept {
            in_use: side == using,
            ..describe(&side, "side", Some(v))
        })
        .collect();
    let prefix = format!("{}.broken-", file_name(path));
    kept.extend(
        siblings(path)
            .into_iter()
            .filter(|p| {
                let name = file_name(p);
                name.starts_with(&prefix) && !name.ends_with("-wal") && !name.ends_with("-shm")
            })
            .map(|p| describe(&p, "broken", None)),
    );
    kept
}

fn describe(path: &Path, kind: &'static str, version: Option<i64>) -> Kept {
    let files = [path.to_path_buf(), with_suffix(path, "-wal")];
    let meta: Vec<std::fs::Metadata> = files
        .iter()
        .filter_map(|f| std::fs::metadata(f).ok())
        .collect();
    let idle = meta
        .iter()
        .filter_map(|m| m.modified().ok())
        .max()
        .and_then(|t| t.elapsed().ok())
        .map_or(0, |d| d.as_secs());
    Kept {
        path: path.to_string_lossy().into_owned(),
        kind,
        version,
        bytes: meta.iter().map(|m| m.len()).sum(),
        idle,
        in_use: false,
    }
}

/// Remove a kept file with its WAL, lock and, for a side store, the core
/// files beside it.
pub(crate) fn remove_kept(kept: &Kept) {
    let path = Path::new(&kept.path);
    for suffix in ["", "-wal", "-shm", ".lock"] {
        let _ = std::fs::remove_file(with_suffix(path, suffix));
    }
    if kept.kind == "side" {
        let _ = std::fs::remove_dir_all(super::core_dir_of(path));
    }
}

fn siblings(path: &Path) -> Vec<PathBuf> {
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    std::fs::read_dir(dir)
        .map(|entries| entries.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default()
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// How long an opener waits for another to finish setting the store aside, or
/// for a slow open to let it: DEC-139's writer wait.
const LOCK_WAIT: Duration = Duration::from_secs(600);

/// An advisory lock on `<store>.lock`: shared while opening, exclusive while
/// setting the store aside. Released on drop, or by the kernel when a holder
/// dies.
struct Lock(Option<File>);

impl Lock {
    fn take(path: &Path, exclusive: bool) -> Result<Lock> {
        let lock = with_suffix(path, ".lock");
        // A directory trekr can't write to fails the open itself, and says so.
        let Ok(file) = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock)
        else {
            return Ok(Lock(None));
        };
        let op = if exclusive {
            libc::LOCK_EX
        } else {
            libc::LOCK_SH
        };
        let deadline = Instant::now() + LOCK_WAIT;
        loop {
            // SAFETY: flock on a descriptor this function owns.
            if unsafe { libc::flock(file.as_raw_fd(), op | libc::LOCK_NB) } == 0 {
                return Ok(Lock(Some(file)));
            }
            if Instant::now() >= deadline {
                return Err(failure(
                    ffi::SQLITE_BUSY,
                    format!("timed out waiting for {}", lock.display()),
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        if let Some(file) = &self.0 {
            // SAFETY: as above; closing the file would release it too.
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::schema;

    /// A directory of its own: set-aside copies and side stores are siblings.
    struct Dir(PathBuf);

    impl Dir {
        fn new(label: &str) -> Dir {
            let dir =
                std::env::temp_dir().join(format!("trekr-recover-{label}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Dir(dir)
        }
        fn db(&self) -> PathBuf {
            self.0.join("trekr.db")
        }
        fn broken(&self) -> Vec<PathBuf> {
            siblings(&self.db())
                .into_iter()
                .filter(|p| {
                    let n = file_name(p);
                    n.contains(".broken-") && !n.ends_with("-wal")
                })
                .collect()
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn kept_lists_side_stores_and_broken_copies_and_removes_them() {
        let dir = Dir::new("kept");
        let db = dir.db();
        let side = side_path(&db, 7);
        std::fs::write(&side, b"x").unwrap();
        std::fs::write(with_suffix(&side, "-wal"), b"yy").unwrap();
        let broken = db.with_file_name("trekr.db.broken-100");
        std::fs::write(&broken, b"z").unwrap();
        std::fs::write(with_suffix(&broken, "-wal"), b"z").unwrap();

        let kept = kept(&db, schema::VERSION);
        let found: Vec<(&str, Option<i64>, u64)> =
            kept.iter().map(|k| (k.kind, k.version, k.bytes)).collect();
        assert_eq!(found, [("side", Some(7), 3), ("broken", None, 2)]);
        assert!(kept.iter().all(|k| !k.in_use));

        kept.iter().for_each(remove_kept);
        assert!(super::kept(&db, schema::VERSION).is_empty());
        assert!(!with_suffix(&side, "-wal").exists());
    }

    /// The trekr after this one: its schema adds a table.
    fn newer() -> Layout {
        let mut tables = schema::TABLES.to_vec();
        tables.insert(0, "extra");
        Layout {
            version: schema::VERSION + 1,
            sql: Box::leak(format!("{}CREATE TABLE extra (x);", schema::SCHEMA).into_boxed_str()),
            tables: Box::leak(tables.into_boxed_slice()),
        }
    }

    fn version(path: &Path) -> i64 {
        Connection::open(path)
            .unwrap()
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap()
    }

    fn put(store: &Store, key: &str) {
        store
            .conn
            .execute("INSERT INTO meta (key, value) VALUES (?1, 'v')", [key])
            .unwrap();
    }

    fn has(store: &Store, key: &str) -> bool {
        store
            .conn
            .query_row("SELECT COUNT(*) FROM meta WHERE key = ?1", [key], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap()
            == 1
    }

    #[test]
    fn a_file_that_is_not_a_database_is_set_aside_and_rebuilt() {
        let dir = Dir::new("garbage");
        std::fs::write(dir.db(), "not a database ".repeat(1000)).unwrap();
        let store = Store::open(&dir.db()).expect("a working store");
        put(&store, "k");
        let copies = dir.broken();
        assert_eq!(copies.len(), 1);
        assert!(
            std::fs::read(&copies[0])
                .unwrap()
                .starts_with(b"not a database")
        );
        // "not indexed" can say why the index is empty
        assert_eq!(store.upgraded_from().unwrap(), Some(schema::VERSION));
    }

    #[test]
    fn a_truncated_file_is_set_aside_and_rebuilt() {
        let dir = Dir::new("truncated");
        {
            let store = Store::open(&dir.db()).unwrap();
            let rows = (0..5000)
                .map(|i| format!("('key{i}', '{}')", "x".repeat(200)))
                .collect::<Vec<_>>()
                .join(",");
            store
                .conn
                .execute_batch(&format!("INSERT INTO meta (key, value) VALUES {rows}"))
                .unwrap();
        }
        let len = std::fs::metadata(dir.db()).unwrap().len();
        File::options()
            .write(true)
            .open(dir.db())
            .unwrap()
            .set_len(len / 2)
            .unwrap();
        let store = Store::open(&dir.db()).expect("a working store");
        assert!(!has(&store, "key1"), "rebuilt, not the old file");
        assert_eq!(dir.broken().len(), 1);
    }

    #[test]
    fn a_rebuild_that_fails_midway_is_set_aside_and_rebuilt() {
        let dir = Dir::new("failed-rebuild");
        // An older store whose drop fails partway: a view where a table was.
        Connection::open(dir.db())
            .unwrap()
            .execute_batch(
                "CREATE TABLE def (x); CREATE VIEW checkout AS SELECT 1; \
                 PRAGMA user_version = 3;",
            )
            .unwrap();
        let store = Store::open(&dir.db()).expect("a working store");
        assert_eq!(store.schema_version().unwrap(), schema::VERSION);
        assert_eq!(store.upgraded_from().unwrap(), Some(3));
        drop(store);
        let copy = &dir.broken()[0];
        assert_eq!(version(copy), 3, "the failed rebuild rolled back");
    }

    #[test]
    fn a_store_missing_a_table_is_rebuilt_but_not_for_an_optional_one() {
        let dir = Dir::new("missing-table");
        drop(Store::open(&dir.db()).unwrap());
        // built by a trekr from before the table existed, at this version
        Connection::open(dir.db())
            .unwrap()
            .execute_batch("DROP TABLE meta;")
            .unwrap();
        drop(Store::open(&dir.db()).unwrap());
        assert!(dir.broken().is_empty());
        Connection::open(dir.db())
            .unwrap()
            .execute_batch("DROP TABLE def;")
            .unwrap();
        let store = Store::open(&dir.db()).unwrap();
        assert!(store.conn.prepare("SELECT * FROM def").is_ok());
        assert_eq!(dir.broken().len(), 1);
    }

    #[test]
    fn only_the_newest_set_aside_copy_is_kept() {
        let dir = Dir::new("one-copy");
        for _ in 0..2 {
            std::fs::write(dir.db(), "garbage ".repeat(1000)).unwrap();
            let _ = std::fs::remove_file(with_suffix(&dir.db(), "-wal"));
            drop(Store::open(&dir.db()).unwrap());
            std::thread::sleep(Duration::from_millis(1100));
        }
        assert_eq!(dir.broken().len(), 1);
    }

    #[test]
    fn a_path_that_cannot_be_opened_is_an_error_and_nothing_moves() {
        let dir = Dir::new("cantopen");
        std::fs::create_dir(dir.db()).unwrap();
        assert!(Store::open(&dir.db()).is_err());
        assert!(dir.db().is_dir());
        assert!(dir.broken().is_empty());
    }

    #[test]
    fn a_newer_store_is_left_alone_and_this_trekr_keeps_its_own() {
        let dir = Dir::new("newer");
        put(&open(&dir.db(), &newer()).unwrap(), "newer");
        let store = Store::open(&dir.db()).expect("a working store");
        put(&store, "older");
        assert_eq!(
            store.path(),
            Some(side_path(&dir.db(), schema::VERSION).as_path())
        );
        drop(store);
        assert_eq!(version(&dir.db()), schema::VERSION + 1);
        let main = open(&dir.db(), &newer()).unwrap();
        assert!(has(&main, "newer") && !has(&main, "older"));
    }

    #[test]
    fn two_versions_taking_turns_keep_their_stores() {
        let dir = Dir::new("alternate");
        for turn in 0..3 {
            let newer_store = open(&dir.db(), &newer()).unwrap();
            let older_store = Store::open(&dir.db()).unwrap();
            if turn == 0 {
                put(&newer_store, "n");
                put(&older_store, "o");
            }
            assert!(
                has(&newer_store, "n"),
                "turn {turn}: the newer store survived"
            );
            assert!(
                has(&older_store, "o"),
                "turn {turn}: the older store survived"
            );
        }
        assert!(dir.broken().is_empty());
    }

    #[test]
    fn a_newer_trekr_retires_its_own_stale_side_store() {
        let dir = Dir::new("retire");
        drop(open(&dir.db(), &newer()).unwrap());
        drop(Store::open(&dir.db()).unwrap());
        let side = side_path(&dir.db(), schema::VERSION);
        assert!(side.exists());
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(with_suffix(&dir.db(), suffix));
        }
        drop(Store::open(&dir.db()).unwrap());
        assert!(!side.exists());
    }

    #[test]
    fn a_long_lived_reader_notices_its_store_replaced() {
        let dir = Dir::new("replaced");
        let store = Store::open(&dir.db()).unwrap();
        assert_eq!(store.replaced(), None);
        Connection::open(dir.db())
            .unwrap()
            .pragma_update(None, "user_version", schema::VERSION + 1)
            .unwrap();
        assert!(store.replaced().is_some(), "rebuilt for another schema");
        let other = Dir::new("replaced-file");
        let store = Store::open(&other.db()).unwrap();
        quarantine(&other.db()).unwrap();
        assert!(store.replaced().is_some(), "set aside");
    }

    #[test]
    fn concurrent_openers_of_a_broken_store_rebuild_it_once() {
        let dir = Dir::new("concurrent");
        for round in 0..5 {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(with_suffix(&dir.db(), suffix));
            }
            std::fs::write(dir.db(), "garbage ".repeat(1000)).unwrap();
            let start = std::sync::Arc::new(std::sync::Barrier::new(8));
            let opens: Vec<_> = (0..8)
                .map(|i| {
                    let (path, start) = (dir.db(), std::sync::Arc::clone(&start));
                    std::thread::spawn(move || {
                        start.wait();
                        let store = Store::open(&path).expect("every opener gets a store");
                        put(&store, &format!("t{i}"));
                    })
                })
                .collect();
            for open in opens {
                open.join().unwrap();
            }
            let store = Store::open(&dir.db()).unwrap();
            for i in 0..8 {
                assert!(
                    has(&store, &format!("t{i}")),
                    "round {round}: t{i}'s write survived"
                );
            }
            assert_eq!(dir.broken().len(), 1);
        }
    }
}
