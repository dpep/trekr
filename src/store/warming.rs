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
}

impl Warming {
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
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )?;
        let value = format!("{} {read} {of}", std::process::id());
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

/// `pid read of`, and whether that pid is still running.
fn parse(value: &str) -> Option<Warming> {
    let mut parts = value.split(' ').map(str::parse::<u64>);
    let (pid, read, of) = (
        parts.next()?.ok()?,
        parts.next()?.ok()?,
        parts.next()?.ok()?,
    );
    Some(Warming {
        read,
        of,
        interrupted: !alive(pid),
    })
}

/// Is a process with this pid running? A pid reused since reads as running,
/// which keeps the checkout marked partial until its next index — the safe
/// side of the mistake.
fn alive(pid: u64) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 checks for the process and delivers nothing.
    let sent = unsafe { libc::kill(pid, 0) };
    sent == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
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
    fn coverage_never_rounds_up_to_whole() {
        let warming = Warming {
            read: 999,
            of: 1000,
            interrupted: false,
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
