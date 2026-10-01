//! A first index's early store (DEC-332): where files opened while the
//! checkout's bulk write holds the store are written, so they are answered
//! before that write commits.
//!
//! The bulk write (DEC-057) is one transaction of up to tens of seconds, and
//! nothing else can commit to the store until it does. An early store is a
//! copy of the store as that write found it, plus the files opened since,
//! written by the same index. A language server reads it in place of the
//! store until the index removes it, which it does once the bulk write is in:
//! by then the store holds every file the early store does. The store itself
//! is never written from here, so it ends as an index without an early store
//! would leave it.

use rusqlite::Result;
use std::path::{Path, PathBuf};

/// The directory the index running as `pid` keeps its early store in, beside
/// the store at `main`: the store under the same name, and what SQLite and
/// the trees keep beside it, so one removal takes all of it.
pub(crate) fn dir(main: &Path, pid: u32) -> PathBuf {
    let mut name = main.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".early-{pid}"));
    main.with_file_name(name)
}

/// The early store itself.
pub(crate) fn path(main: &Path, pid: u32) -> PathBuf {
    dir(main, pid).join(main.file_name().unwrap_or_default())
}

/// Remove an early store's directory, and all of it.
pub(crate) fn remove(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

/// Remove early stores left by indexes that are no longer running.
pub(crate) fn sweep(main: &Path) {
    let (Some(dir), Some(name)) = (main.parent(), main.file_name()) else {
        return;
    };
    let prefix = format!("{}.early-", name.to_string_lossy());
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let file = entry.file_name().to_string_lossy().into_owned();
        let Some(pid) = file.strip_prefix(&prefix) else {
            continue;
        };
        let digits = pid.find(|c: char| !c.is_ascii_digit()).unwrap_or(pid.len());
        let Ok(pid) = pid[..digits].parse::<u64>() else {
            continue;
        };
        if !super::warming::alive(pid) {
            remove(&entry.path());
        }
    }
}

/// Copy the store at `main`, as of its last commit, to `to`; `false` when
/// that could not be done cleanly, and nothing is left at `to`.
///
/// By file, so a copy-on-write filesystem clones it in no time whatever the
/// store's size, where `VACUUM INTO` rewrites every page. The last commit is
/// all in the file once a checkpoint has copied every committed frame there,
/// and while no commit follows, nothing else writes the file: a checkpoint
/// only ever copies committed frames. The bulk write holds the store's write
/// lock, so the only commit that can land is its own, and a copy taken across
/// it is discarded.
pub(crate) fn copy(main: &Path, to: &Path) -> Result<bool> {
    let _ = std::fs::remove_file(to);
    let conn = rusqlite::Connection::open(main)?;
    conn.busy_timeout(std::time::Duration::from_secs(1))?;
    let version = |conn: &rusqlite::Connection| -> Result<i64> {
        conn.query_row("PRAGMA data_version", [], |r| r.get(0))
    };
    for _ in 0..TRIES {
        let before = version(&conn)?;
        let (busy, frames, copied): (i64, i64, i64) =
            conn.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
        // A reader on an older snapshot holds back the frames after it.
        if busy != 0 || frames != copied {
            std::thread::sleep(std::time::Duration::from_millis(10));
            continue;
        }
        let copied = std::fs::copy(main, to).is_ok();
        if copied && version(&conn)? == before {
            return Ok(true);
        }
        let _ = std::fs::remove_file(to);
        if copied {
            // The bulk write committed: the store needs no early now.
            return Ok(false);
        }
    }
    Ok(false)
}

/// Checkpoints tried before giving the early store up.
const TRIES: usize = 50;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("trekr-early-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_copy_holds_what_the_store_did_and_a_dead_index_s_is_swept() {
        let scratch = scratch("copy");
        let main = scratch.join("t.db");
        let store = Store::open(&main).unwrap();
        store.set_warming("/app", 1, 2).unwrap();
        let early = path(&main, std::process::id());
        std::fs::create_dir_all(early.parent().unwrap()).unwrap();
        assert!(copy(&main, &early).unwrap());
        let copied = Store::open(&early).unwrap();
        assert_eq!(
            copied.warming("/app").unwrap(),
            store.warming("/app").unwrap()
        );
        drop(copied);

        let dead = dir(&main, i32::MAX as u32);
        std::fs::create_dir_all(&dead).unwrap();
        sweep(&main);
        assert!(early.exists(), "a running index's early is kept");
        assert!(!dead.exists(), "a dead index's early is removed");
        assert!(main.exists());
        let _ = std::fs::remove_dir_all(&scratch);
    }
}
