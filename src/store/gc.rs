//! Collecting checkouts nothing will ask about again (DEC-049).
//!
//! Content addressing means an old gem version is never an orphan — it still
//! maps its own blobs — so "blobs no file references" finds nothing (DEC-030)
//! while every version any project ever resolved is kept forever. The unit of
//! collection is therefore the checkout, and a blob goes only when the last
//! checkout mapping it does. A version no checkout maps — a query's draft
//! (DEC-035), one an edit or a drop unmapped — goes once nothing has mapped
//! it for the retention (DEC-651).

use super::Store;
use rusqlite::{OptionalExtension, Result, params};
use std::collections::HashSet;

/// How long a blob a query recorded is kept whatever the cutoff: the query
/// that recorded it may still be reading it, and nothing else says so.
const READING: i64 = 3600;

/// What a collection removed, or — on a dry run — would have.
#[derive(Debug, Default, serde::Serialize)]
pub(crate) struct Garbage {
    pub(crate) checkouts: Vec<Collected>,
    /// File-map rows: one per path in each collected checkout.
    pub(crate) files: usize,
    /// Blobs no surviving checkout maps: a collected checkout's, and those
    /// unmapped past the retention. A blob a checkout still maps is never
    /// among them.
    pub(crate) blobs: usize,
    /// Of `blobs`, those collected for going unmapped longer than the
    /// retention.
    pub(crate) expired: usize,
    /// Fact rows (defs, ancestry, constant refs, call sites) those blobs held.
    pub(crate) facts: usize,
    /// Pages the deletion returned to SQLite's free list, which later writes
    /// reuse. The file itself shrinks only when it is vacuumed.
    pub(crate) freed_bytes: i64,
    /// A Ruby's signatures no surviving stdlib is served with (DEC-274).
    pub(crate) signatures: usize,
    /// The core directories the surviving signatures are written under, for
    /// the sweep of the files beside the store.
    #[serde(skip)]
    pub(crate) core_dirs: HashSet<String>,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct Collected {
    pub(crate) repo: String,
    /// `repo`, `gem` or `stdlib`.
    pub(crate) kind: String,
    /// `vanished`: the root is gone from disk. `unclaimed`: it is there, but no
    /// surviving checkout's bundle names it — a gem version every project has
    /// moved past.
    pub(crate) reason: &'static str,
    /// Unix seconds: when an index last vouched for it.
    pub(crate) last_seen: i64,
}

/// The versions of files no checkout maps, kept for the retention (DEC-651).
#[derive(Debug, Default, Clone, Copy, serde::Serialize)]
pub(crate) struct Retained {
    /// Stamped or not: one an older trekr unmapped is stamped by the next
    /// pass.
    pub(crate) blobs: usize,
    /// Of `blobs`, those unmapped longer than the retention: the next
    /// collection's.
    pub(crate) expired: usize,
}

/// What the background cleanup last did (DEC-650), from `meta`.
#[derive(Debug, Default, Clone, Copy, PartialEq, serde::Serialize)]
pub(crate) struct Cleanup {
    /// Unix seconds: when a cleanup last measured the store, or claimed it.
    pub(crate) checked_at: Option<i64>,
    /// Unix seconds: when one last removed or compacted anything.
    pub(crate) ran_at: Option<i64>,
    /// The bytes that run returned to the disk.
    pub(crate) reclaimed_bytes: Option<i64>,
    /// Why the last pass left free pages it was meant to give back.
    pub(crate) skipped: Option<Held>,
}

/// Why a pass that meant to compact the file did not.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Held {
    /// A full rewrite would not fit on the disk.
    NoRoom,
    /// Another writer held the lock past the wait.
    Busy,
}

impl Held {
    fn as_str(self) -> &'static str {
        match self {
            Held::NoRoom => "no_room",
            Held::Busy => "busy",
        }
    }

    fn parse(text: &str) -> Option<Held> {
        [Held::NoRoom, Held::Busy]
            .into_iter()
            .find(|held| held.as_str() == text)
    }
}

/// The store's size and what the background cleanup did, as of one moment.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Storage {
    pub(crate) cleanup: Cleanup,
    pub(crate) free_bytes: i64,
    /// Pages, as SQLite counts them.
    pub(crate) bytes: i64,
}

/// `meta` keys: when a cleanup last looked, and `<at> <bytes>` of its last
/// run that reclaimed anything.
pub(super) const CLEANUP_CHECKED: &str = "cleanup checked";
const CLEANUP_RAN: &str = "cleanup ran";
const CLEANUP_SKIPPED: &str = "cleanup skipped";

struct Row {
    id: i64,
    root: String,
    kind: String,
    last_seen: i64,
}

impl Store {
    /// Remove every checkout that is neither live nor seen after `cutoff`.
    ///
    /// Live is what a future index could still reach: a repo whose root is on
    /// disk, or a gem on disk that a surviving repo's bundle names. The cutoff
    /// is hysteresis on top of that, so a lockfile flipped by a branch switch
    /// and flipped back does not re-parse. `on_disk` is the filesystem probe,
    /// injected so the store never touches the disk itself.
    ///
    /// One transaction; a dry run does all of it, measures, and rolls back —
    /// so the reclaimed size it reports is the real one, not an estimate.
    pub(crate) fn collect(
        &mut self,
        cutoff: i64,
        unmapped_before: Option<i64>,
        on_disk: impl Fn(&str) -> bool,
        dry_run: bool,
    ) -> Result<Garbage> {
        // Immediate, as the writers' own: what it decides to delete is what
        // the store holds when it deletes.
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let rows: Vec<Row> = {
            let mut stmt = tx.prepare("SELECT id, root, kind, indexed_at FROM checkout")?;
            stmt.query_map([], |r| {
                Ok(Row {
                    id: r.get(0)?,
                    root: r.get(1)?,
                    kind: r.get(2)?,
                    last_seen: r.get(3)?,
                })
            })?
            .collect::<Result<_>>()?
        };
        let uses: Vec<(i64, String)> = {
            let mut stmt = tx.prepare("SELECT checkout_id, gem_root FROM gem_use")?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_>>()?
        };

        let present: HashSet<i64> = rows
            .iter()
            .filter(|row| on_disk(&row.root))
            .map(|row| row.id)
            .collect();
        let recent = |row: &Row| row.last_seen > cutoff;
        // Repos first: whether a gem survives depends on which repos do.
        let kept_repos: HashSet<i64> = rows
            .iter()
            .filter(|row| row.kind == "repo" && (present.contains(&row.id) || recent(row)))
            .map(|row| row.id)
            .collect();
        let claimed: HashSet<&str> = uses
            .iter()
            .filter(|(user, _)| kept_repos.contains(user))
            .map(|(_, gem)| gem.as_str())
            .collect();
        let doomed: Vec<&Row> = rows
            .iter()
            .filter(|row| {
                let live = if row.kind != "repo" {
                    present.contains(&row.id) && claimed.contains(row.root.as_str())
                } else {
                    kept_repos.contains(&row.id)
                };
                !live && !recent(row)
            })
            .collect();

        let ids: Vec<i64> = doomed.iter().map(|row| row.id).collect();
        let mut garbage = drop_blobs(&tx, &ids, unmapped_before, i64::MAX)?;
        garbage.checkouts = doomed
            .into_iter()
            .map(|row| Collected {
                reason: if present.contains(&row.id) {
                    "unclaimed"
                } else {
                    "vanished"
                },
                repo: row.root.clone(),
                kind: row.kind.clone(),
                last_seen: row.last_seen,
            })
            .collect();
        if dry_run {
            tx.rollback()?;
        } else {
            tx.commit()?;
        }
        Ok(garbage)
    }

    /// Up to `limit` of the blobs no checkout maps, unmapped at or before
    /// `before`: what the background cleanup collects, a batch per call, and
    /// it never judges a checkout (DEC-650, DEC-651).
    pub(crate) fn collect_unmapped(&mut self, before: i64, limit: usize) -> Result<Garbage> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let garbage = drop_blobs(&tx, &[], Some(before), limit)?;
        tx.commit()?;
        Ok(garbage)
    }

    /// Stamp, as of now, every blob no file maps and nothing has stamped:
    /// one an older trekr unmapped — before this build ran, or since, an
    /// editor's server still running it — when it went being unknown
    /// (DEC-651). How many it stamped. The scan runs only when the counts say
    /// one is there.
    pub(crate) fn stamp_unmapped(&mut self) -> Result<usize> {
        if !self.any_unstamped()? {
            return Ok(0);
        }
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        super::loose_blob(&tx)?;
        let stamped = tx.execute(
            "INSERT OR IGNORE INTO loose_blob (blob_id, recorded_at)
             SELECT b.id, unixepoch() FROM blob b
              WHERE NOT EXISTS (SELECT 1 FROM main.file f WHERE f.blob_id = b.id)",
            [],
        )?;
        tx.commit()?;
        Ok(stamped)
    }

    /// Whether some blob no file maps has no stamp.
    pub(crate) fn any_unstamped(&self) -> Result<bool> {
        let (unmapped, stamped, _) = self.unmapped(None)?;
        Ok(unmapped > stamped)
    }

    /// Whether some version no file maps went unmapped at or before
    /// `before`, and a collection may take it now.
    pub(crate) fn any_unmapped_before(&self, before: i64) -> Result<bool> {
        Ok(self.unmapped(Some(before))?.2 > 0)
    }

    /// The versions no checkout maps, stamped or not, and which of them went
    /// unmapped at or before `before`; `None` keeps them all. Reads only.
    pub(crate) fn retained(&self, before: Option<i64>) -> Result<Retained> {
        let (blobs, _, expired) = self.unmapped(before)?;
        Ok(Retained { blobs, expired })
    }

    /// Blobs no file maps; those of them stamped; and of those, the ones
    /// stamped at or before `before` and past a query's reading. The first
    /// by subtraction, two scans of covering indexes, where probing the map
    /// per blob took 20× as long (DEC-651): a mapped blob always exists.
    fn unmapped(&self, before: Option<i64>) -> Result<(usize, usize, usize)> {
        let unmapped: i64 = self.conn.query_row(
            "SELECT (SELECT COUNT(*) FROM blob) - (SELECT COUNT(DISTINCT blob_id) FROM main.file)",
            [],
            |r| r.get(0),
        )?;
        if !self.has_loose_blob()? {
            return Ok((unmapped as usize, 0, 0));
        }
        let (stamped, expired): (i64, i64) = self.conn.query_row(
            &format!(
                "SELECT COUNT(*), COALESCE(SUM(recorded_at <= MIN(?1, unixepoch() - ?2)), 0)
                   FROM loose_blob l WHERE {UNMAPPED}"
            ),
            params![before.unwrap_or(i64::MIN), READING],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok((unmapped as usize, stamped as usize, expired as usize))
    }

    fn has_loose_blob(&self) -> Result<bool> {
        self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'loose_blob')",
            [],
            |r| r.get(0),
        )
    }

    /// What the background cleanup last did.
    pub(crate) fn cleanup(&self) -> Result<Cleanup> {
        let read = |key: &str| -> Result<Option<String>> {
            self.conn
                .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
                .optional()
        };
        let checked_at = read(CLEANUP_CHECKED)?.and_then(|v| v.parse().ok());
        let ran = read(CLEANUP_RAN)?;
        let mut ran = ran.iter().flat_map(|v| v.split(' ')).map(str::parse);
        Ok(Cleanup {
            checked_at,
            ran_at: ran.next().and_then(|at| at.ok()),
            reclaimed_bytes: ran.next().and_then(|bytes| bytes.ok()),
            skipped: read(CLEANUP_SKIPPED)?.as_deref().and_then(Held::parse),
        })
    }

    /// Record why the last pass left the file as it was, or that it did not.
    pub(crate) fn cleanup_held(&self, held: Option<Held>) -> Result<()> {
        match held {
            Some(held) => set_meta(&self.conn, CLEANUP_SKIPPED, held.as_str()),
            None => self
                .conn
                .execute("DELETE FROM meta WHERE key = ?1", [CLEANUP_SKIPPED])
                .map(drop),
        }
    }

    /// The cleanup record, the free pages and the file's size, read in one
    /// snapshot: a cleanup that finishes between two separate reads would
    /// otherwise report its run beside the pages it has since given back.
    pub(crate) fn storage(&self) -> Result<Storage> {
        let tx = self.conn.unchecked_transaction()?;
        let storage = Storage {
            cleanup: self.cleanup()?,
            free_bytes: self.free_bytes()?,
            bytes: self.db_bytes()?,
        };
        tx.commit()?;
        Ok(storage)
    }

    /// Record that a cleanup looked at `at` and found nothing worth doing.
    pub(crate) fn cleanup_checked(&self, at: i64) -> Result<()> {
        set_meta(&self.conn, CLEANUP_CHECKED, &at.to_string())
    }

    /// Take the cleanup at `at`, unless another looked within `interval`
    /// seconds. One immediate transaction, so of two processes asking at
    /// once only one is told yes. Never waits for the lock: busy is an
    /// error, and the next index pass asks again.
    pub(crate) fn claim_cleanup(&mut self, at: i64, interval: i64) -> Result<bool> {
        self.conn.busy_timeout(std::time::Duration::ZERO)?;
        let claimed = self.claim_cleanup_once(at, interval);
        self.conn.busy_timeout(super::BUSY)?;
        claimed
    }

    fn claim_cleanup_once(&mut self, at: i64, interval: i64) -> Result<bool> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let checked: Option<i64> = tx
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                [CLEANUP_CHECKED],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .and_then(|v| v.parse().ok());
        if checked.is_some_and(|checked| at - checked < interval) {
            return Ok(false);
        }
        set_meta(&tx, CLEANUP_CHECKED, &at.to_string())?;
        tx.commit()?;
        Ok(true)
    }

    /// Record a cleanup run at `at` that returned `reclaimed` bytes.
    pub(crate) fn cleanup_ran(&self, at: i64, reclaimed: i64) -> Result<()> {
        set_meta(&self.conn, CLEANUP_RAN, &format!("{at} {reclaimed}"))
    }

    /// Pages SQLite holds free inside the file, in bytes: what a vacuum
    /// would return to the disk.
    pub(crate) fn free_bytes(&self) -> Result<i64> {
        self.conn.query_row(
            "SELECT freelist_count * page_size FROM pragma_freelist_count(), pragma_page_size()",
            [],
            |r| r.get(0),
        )
    }

    /// Rewrite the database without its free pages, and fold the WAL back in
    /// so the file on disk actually shrinks. Seconds on a large store and it
    /// holds the write lock throughout, so it is asked for, never implied.
    ///
    /// The copy SQLite builds on the way goes to a temporary file, not to
    /// memory as this connection's own temporary tables do: it is the whole
    /// live database, and in memory it was 1.3 GB of footprint for a 1 GB
    /// store. The rewrite also switches an older store to incremental
    /// vacuuming, so it is the last full one the cleanup needs.
    pub(crate) fn vacuum(&self) -> Result<()> {
        self.conn
            .execute_batch("PRAGMA auto_vacuum = INCREMENTAL; PRAGMA temp_store = FILE;")?;
        let vacuumed = self.conn.execute_batch("VACUUM;");
        self.conn.execute_batch("PRAGMA temp_store = MEMORY;")?;
        vacuumed?;
        self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
    }

    /// Whether free pages can be handed back a chunk at a time, without
    /// rewriting the file.
    pub(crate) fn incremental(&self) -> Result<bool> {
        // Read under a transaction, so the answer is the file's header as it
        // is now, not as this connection last read it.
        let tx = self.conn.unchecked_transaction()?;
        let _: i64 = tx.query_row("SELECT COUNT(*) FROM meta", [], |r| r.get(0))?;
        let mode: i64 = tx.query_row("PRAGMA auto_vacuum", [], |r| r.get(0))?;
        tx.commit()?;
        Ok(mode == 2)
    }

    /// Hand back up to `pages` free pages, moving pages from the end of the
    /// file into the holes: one short write transaction, then the log folded
    /// back as far as readers allow.
    pub(crate) fn vacuum_step(&self, pages: i64) -> Result<()> {
        // Each step of the statement frees a page: run it to the end.
        let mut stmt = self
            .conn
            .prepare(&format!("PRAGMA incremental_vacuum({pages})"))?;
        let mut rows = stmt.query([])?;
        while rows.next()?.is_some() {}
        self.conn
            .query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |_| Ok(()))
    }

    /// The schema's fact-table indexes this store lacks: one added to the
    /// schema at this version, which a store laid down before it has not got.
    pub(crate) fn missing_indexes(&self) -> Result<Vec<&'static str>> {
        let mut have = self
            .conn
            .prepare("SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = ?1")?;
        let mut missing = Vec::new();
        for (name, _) in super::schema::BULK_INDEXES {
            if !have.exists([name])? {
                missing.push(name);
            }
        }
        Ok(missing)
    }

    /// Add the indexes [`Store::missing_indexes`] names, and say which. Holds
    /// the write lock while each is built: a writer behind it waited 2.4 s on
    /// a 1 GB store.
    pub(crate) fn add_missing_indexes(&self) -> Result<Vec<&'static str>> {
        let missing = self.missing_indexes()?;
        // As a bulk load builds them: the sort spills to a file, not memory.
        self.conn.execute_batch("PRAGMA temp_store = FILE;")?;
        let added = missing.iter().try_for_each(|name| {
            let (_, create) = super::schema::BULK_INDEXES
                .iter()
                .find(|(bulk, _)| bulk == name)
                .expect("named from the same list");
            self.conn.execute_batch(create)
        });
        self.conn.execute_batch("PRAGMA temp_store = MEMORY;")?;
        added.map(|()| missing)
    }

    /// The database's size, in bytes, as SQLite counts it.
    pub(crate) fn db_bytes(&self) -> Result<i64> {
        self.conn.query_row(
            "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()",
            [],
            |r| r.get(0),
        )
    }
}

fn set_meta(conn: &rusqlite::Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
           ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )
    .map(drop)
}

/// A `loose_blob l` row whose blob no file maps. `main.file`: a query's
/// overlay shadows `file` with its edits.
const UNMAPPED: &str = "NOT EXISTS (SELECT 1 FROM main.file f WHERE f.blob_id = l.blob_id)";

/// Forget the checkouts `doomed`, then delete the blobs no surviving
/// checkout maps among theirs, and up to `limit` of those unmapped at or
/// before `unmapped_before` (none for `None`), measuring what went.
fn drop_blobs(
    tx: &rusqlite::Transaction,
    doomed: &[i64],
    unmapped_before: Option<i64>,
    limit: i64,
) -> Result<Garbage> {
    let mut garbage = Garbage::default();
    let page_size: i64 = tx.query_row("PRAGMA page_size", [], |r| r.get(0))?;
    let free_before: i64 = tx.query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
    tx.execute_batch("CREATE TEMP TABLE gc_blob (id INTEGER PRIMARY KEY)")?;
    {
        let mut candidates = tx.prepare(
            "INSERT OR IGNORE INTO gc_blob SELECT blob_id FROM file WHERE checkout_id = ?1",
        )?;
        let mut forget = tx.prepare("DELETE FROM checkout WHERE id = ?1")?;
        for id in doomed {
            garbage.files += candidates.execute(params![id])?;
            // Cascades to the file map and to the bundle it claimed.
            forget.execute(params![id])?;
        }
    }
    // Versions nothing maps — a query's draft, an edited-away or dropped
    // file's — once unmapped past the retention (DEC-651), but never while a
    // query may be reading one.
    if let Some(before) = unmapped_before {
        super::loose_blob(tx)?;
        garbage.expired = tx.execute(
            &format!(
                "INSERT OR IGNORE INTO gc_blob
                 SELECT blob_id FROM loose_blob l
                  WHERE recorded_at <= MIN(?1, unixepoch() - ?2) AND {UNMAPPED}
                  LIMIT ?3"
            ),
            params![before, READING, limit],
        )?;
    }
    // A blob some surviving checkout maps stays. The foreign key would
    // refuse the delete anyway; this makes the refusal a non-event.
    tx.execute(
        "DELETE FROM gc_blob WHERE EXISTS (SELECT 1 FROM file WHERE blob_id = gc_blob.id)",
        [],
    )?;
    // A call is a site, counted from its name's posting (DEC-193).
    for (table, rows) in [
        ("def", "COUNT(*)"),
        ("ancestry", "COUNT(*)"),
        ("const_ref", "COUNT(*)"),
        ("call_name", "COALESCE(SUM(calls), 0)"),
    ] {
        let n: i64 = tx.query_row(
            &format!("SELECT {rows} FROM {table} WHERE blob_id IN (SELECT id FROM gc_blob)"),
            [],
            |r| r.get(0),
        )?;
        garbage.facts += n as usize;
    }
    garbage.blobs = tx.execute("DELETE FROM blob WHERE id IN (SELECT id FROM gc_blob)", [])?;
    tx.execute_batch("DROP TABLE gc_blob")?;
    garbage.signatures = unserved_signatures(tx)?;
    garbage.core_dirs = core_dirs(tx)?;
    let free_after: i64 = tx.query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
    garbage.freed_bytes = (free_after - free_before).max(0) * page_size;
    Ok(garbage)
}

/// Drop the signatures no stdlib is served with any more: a collected
/// stdlib's, whose `rbs_use` went with it.
fn unserved_signatures(tx: &rusqlite::Transaction) -> Result<usize> {
    tx.execute(
        "DELETE FROM rbs WHERE id NOT IN (SELECT rbs_id FROM rbs_use)",
        [],
    )
}

fn core_dirs(tx: &rusqlite::Transaction) -> Result<HashSet<String>> {
    let mut stmt = tx.prepare("SELECT version, key FROM rbs")?;
    stmt.query_map([], |r| {
        Ok(crate::tree::core_dir_name(
            &r.get::<_, String>(0)?,
            &r.get::<_, String>(1)?,
        ))
    })?
    .collect()
}

#[cfg(test)]
mod tests {
    use super::super::tests::indexed;
    use super::*;
    use crate::scan::{Files, hash_blob};

    /// A checkout mapping several files at once — `indexed` maps exactly one.
    fn checkout(store: &mut Store, root: &str, files: &[(&str, &str)]) {
        let map: Files = files
            .iter()
            .map(|(path, src)| (path.to_string(), hash_blob(src.as_bytes())))
            .collect();
        let facts: Vec<_> = files
            .iter()
            .map(|(_, src)| (hash_blob(src.as_bytes()), src))
            .filter(|(oid, _)| !store.has_blob(oid).unwrap())
            .map(|(oid, src)| (oid, crate::extract::extract(src.as_bytes())))
            .collect();
        store.write(root, &map, facts, 0).unwrap();
    }

    fn now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    /// An app that resolves `gem-2`, and an older `gem-1` it has moved past.
    /// The two versions share one file byte for byte.
    fn bundle(store: &mut Store) {
        let shared = "module Shared\nend\n";
        indexed(store, "/app", "app.rb", "class App\nend\n");
        for (gem, own) in [
            ("/gem-1", "class Old\nend\n"),
            ("/gem-2", "class New\nend\n"),
        ] {
            checkout(store, gem, &[("shared.rb", shared), ("own.rb", own)]);
        }
        store
            .set_gems_used("/app", &[("/gem-1".into(), "gem".into())], None)
            .unwrap();
        store
            .set_gems_used("/app", &[("/gem-2".into(), "gem".into())], None)
            .unwrap();
        // Last seen long ago, as if the switch to gem-2 were old news.
        store
            .conn
            .execute(
                "UPDATE checkout SET indexed_at = 0 WHERE root = '/gem-1'",
                [],
            )
            .unwrap();
    }

    fn roots(garbage: &Garbage) -> Vec<&str> {
        garbage.checkouts.iter().map(|c| c.repo.as_str()).collect()
    }

    #[test]
    fn collects_the_unclaimed_version_and_only_the_blobs_it_alone_mapped() {
        let mut store = Store::open_in_memory().unwrap();
        bundle(&mut store);
        let before = store.totals().unwrap().blobs;

        let garbage = store
            .collect(now() - 60, Some(now() - 60), |_| true, false)
            .unwrap();
        assert_eq!(roots(&garbage), ["/gem-1"]);
        assert_eq!(garbage.checkouts[0].reason, "unclaimed");
        assert_eq!(garbage.checkouts[0].kind, "gem");
        // `own.rb` went; `shared.rb` is still mapped by gem-2.
        assert_eq!((garbage.files, garbage.blobs), (2, 1));
        assert!(garbage.facts > 0 && garbage.freed_bytes >= 0);
        assert_eq!(store.totals().unwrap().blobs, before - 1);
        assert!(store.has_checkout("/gem-2").unwrap());
        assert_eq!(store.gems_used("/app").unwrap(), ["/gem-2"]);
    }

    #[test]
    fn a_blob_only_a_query_recorded_goes_once_no_checkout_maps_it() {
        let mut store = Store::open_in_memory().unwrap();
        let blob = |src: &str| {
            (
                hash_blob(src.as_bytes()),
                crate::extract::extract(src.as_bytes()),
            )
        };
        // An edit a query read and nothing indexed: the orphan.
        let (edit, facts) = blob("class Draft\nend\n");
        store.add_blob(&edit, &facts).unwrap();
        // One a query read first and an index then mapped: referenced.
        let (committed, facts) = blob("class Kept\nend\n");
        store.add_blob(&committed, &facts).unwrap();
        store
            .conn
            .execute("UPDATE loose_blob SET recorded_at = 0", [])
            .unwrap();
        indexed(&mut store, "/app", "kept.rb", "class Kept\nend\n");
        // One an index mapped and then edited away: a branch switch back
        // wants it, for the retention (DEC-651).
        indexed(&mut store, "/app", "kept.rb", "class Before\nend\n");
        let (before, _) = blob("class Before\nend\n");
        indexed(&mut store, "/app", "kept.rb", "class Kept\nend\n");

        let dry = store
            .collect(now() - 60, Some(now() - 60), |_| true, true)
            .unwrap();
        assert_eq!(dry.blobs, 1, "a dry run counts it");
        assert!(store.has_blob(&edit).unwrap(), "and keeps it");
        let garbage = store
            .collect(now() - 60, Some(now() - 60), |_| true, false)
            .unwrap();
        assert!(garbage.checkouts.is_empty());
        assert_eq!(garbage.blobs, 1);
        assert!(garbage.facts > 0);
        assert!(!store.has_blob(&edit).unwrap(), "the orphan went");
        assert!(store.has_blob(&committed).unwrap(), "a mapped blob stays");
        assert!(store.has_blob(&before).unwrap(), "an index's blob stays");
    }

    #[test]
    fn the_background_collection_takes_old_query_blobs_and_never_a_checkout() {
        let mut store = Store::open_in_memory().unwrap();
        bundle(&mut store);
        let src = "class Draft\nend\n";
        let edit = hash_blob(src.as_bytes());
        store
            .add_blob(&edit, &crate::extract::extract(src.as_bytes()))
            .unwrap();
        store
            .conn
            .execute("UPDATE loose_blob SET recorded_at = 0", [])
            .unwrap();
        let garbage = store.collect_unmapped(now() - 60, 100).unwrap();
        assert_eq!(garbage.blobs, 1);
        assert!(garbage.checkouts.is_empty());
        assert!(!store.has_blob(&edit).unwrap());
        assert!(store.has_checkout("/gem-1").unwrap(), "unclaimed, but kept");
    }

    /// How SQLite deletes a blob's rows from each table that cascades from
    /// `blob`: every one by an index, or each blob collected scans a table.
    fn cascade_plans(store: &Store) -> Vec<String> {
        let tables: Vec<String> = store
            .conn
            .prepare(
                "SELECT m.name FROM sqlite_master m, pragma_foreign_key_list(m.name) f
                  WHERE m.type = 'table' AND f.\"table\" = 'blob' AND f.on_delete = 'CASCADE'",
            )
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_>>()
            .unwrap();
        assert!(tables.contains(&"body_call".to_string()), "{tables:?}");
        tables
            .iter()
            .map(|table| {
                store
                    .conn
                    .query_row(
                        &format!("EXPLAIN QUERY PLAN DELETE FROM {table} WHERE blob_id = 1"),
                        [],
                        |r| r.get::<_, String>(3),
                    )
                    .unwrap()
            })
            .collect()
    }

    #[test]
    fn every_table_that_cascades_from_a_blob_is_reached_by_an_index() {
        let store = Store::open_in_memory().unwrap();
        for plan in cascade_plans(&store) {
            assert!(
                plan.contains("INDEX") || plan.contains("PRIMARY KEY"),
                "{plan}"
            );
        }
    }

    /// A store laid down before an index joined the schema at this version
    /// gains it from the background cleanup, not from a rebuild.
    #[test]
    fn an_index_the_schema_gained_is_added_to_a_store_without_it() {
        let store = Store::open_in_memory().unwrap();
        store
            .conn
            .execute_batch("DROP INDEX body_call_blob")
            .unwrap();
        assert_eq!(store.missing_indexes().unwrap(), ["body_call_blob"]);
        assert!(
            cascade_plans(&store)
                .iter()
                .any(|plan| plan.starts_with("SCAN"))
        );
        assert_eq!(store.add_missing_indexes().unwrap(), ["body_call_blob"]);
        assert!(store.missing_indexes().unwrap().is_empty());
        assert!(
            cascade_plans(&store)
                .iter()
                .all(|plan| !plan.starts_with("SCAN"))
        );
    }

    /// Since when nothing has mapped each blob, as the store recorded it.
    fn unmapped(store: &Store) -> Vec<(String, i64)> {
        store
            .conn
            .prepare(
                "SELECT b.oid, l.recorded_at FROM loose_blob l JOIN blob b ON b.id = l.blob_id
                  ORDER BY b.oid",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_>>()
            .unwrap()
    }

    fn oid(src: &str) -> String {
        hash_blob(src.as_bytes()).0
    }

    /// As if every version nothing maps had gone unmapped long ago.
    fn age_unmapped(store: &Store) {
        store
            .conn
            .execute("UPDATE loose_blob SET recorded_at = 0", [])
            .unwrap();
    }

    const V1: &str = "class Widget\nend\n";
    const V2: &str = "class Widget\n  def go; end\nend\n";

    #[test]
    fn a_replaced_version_is_kept_within_the_retention_and_collected_after() {
        let mut store = Store::open_in_memory().unwrap();
        indexed(&mut store, "/app", "widget.rb", V1);
        assert!(unmapped(&store).is_empty(), "mapped: not stamped");
        indexed(&mut store, "/app", "widget.rb", V2);
        let stamped = unmapped(&store);
        assert_eq!(stamped.len(), 1);
        assert_eq!(stamped[0].0, oid(V1));
        assert!(stamped[0].1 >= now() - 5, "stamped as it went: {stamped:?}");

        let garbage = store
            .collect(now() - 60, Some(now() - 60), |_| true, false)
            .unwrap();
        assert_eq!(garbage.blobs, 0, "within the retention");
        assert!(store.has_blob(&hash_blob(V1.as_bytes())).unwrap());

        age_unmapped(&store);
        let garbage = store
            .collect(now() - 60, Some(now() - 60), |_| true, false)
            .unwrap();
        assert_eq!((garbage.blobs, garbage.expired), (1, 1));
        assert!(!store.has_blob(&hash_blob(V1.as_bytes())).unwrap());
        assert!(store.has_blob(&hash_blob(V2.as_bytes())).unwrap());
    }

    #[test]
    fn a_retention_of_never_keeps_every_version() {
        let mut store = Store::open_in_memory().unwrap();
        indexed(&mut store, "/app", "widget.rb", V1);
        indexed(&mut store, "/app", "widget.rb", V2);
        age_unmapped(&store);
        let garbage = store.collect(now() - 60, None, |_| true, false).unwrap();
        assert_eq!(garbage.blobs, 0);
        assert_eq!(store.retained(None).unwrap().expired, 0);
    }

    #[test]
    fn mapping_a_version_again_clears_its_stamp() {
        let mut store = Store::open_in_memory().unwrap();
        indexed(&mut store, "/app", "widget.rb", V1);
        indexed(&mut store, "/app", "widget.rb", V2);
        // The branch switch back: V1 again, nothing reparsed.
        let back = indexed(&mut store, "/app", "widget.rb", V1);
        assert_eq!(back.parsed, 0, "reused, not reparsed");
        let stamped = unmapped(&store);
        assert_eq!(stamped.len(), 1, "{stamped:?}");
        assert_eq!(stamped[0].0, oid(V2));
        age_unmapped(&store);
        store
            .collect(now() - 60, Some(now() - 60), |_| true, false)
            .unwrap();
        assert!(store.has_blob(&hash_blob(V1.as_bytes())).unwrap());
        assert!(!store.has_blob(&hash_blob(V2.as_bytes())).unwrap());
    }

    #[test]
    fn a_version_another_checkout_maps_is_never_collected() {
        let mut store = Store::open_in_memory().unwrap();
        // Two worktrees of one repo; one moves on.
        indexed(&mut store, "/a", "widget.rb", V1);
        indexed(&mut store, "/b", "widget.rb", V1);
        indexed(&mut store, "/a", "widget.rb", V2);
        assert!(unmapped(&store).is_empty(), "/b still maps it");
        // Stamped by an older build, or by hand: still never collected.
        store
            .conn
            .execute(
                "INSERT INTO loose_blob SELECT id, 0 FROM blob WHERE oid = ?1",
                [oid(V1)],
            )
            .unwrap();
        let garbage = store
            .collect(now() - 60, Some(now() - 60), |_| true, false)
            .unwrap();
        assert_eq!(garbage.blobs, 0);
        store.collect_unmapped(now() - 60, 100).unwrap();
        assert!(store.has_blob(&hash_blob(V1.as_bytes())).unwrap());
    }

    #[test]
    fn a_renamed_file_keeps_its_version_unstamped() {
        let mut store = Store::open_in_memory().unwrap();
        indexed(&mut store, "/app", "widget.rb", V1);
        indexed(&mut store, "/app", "gadget.rb", V1);
        assert!(unmapped(&store).is_empty(), "{:?}", unmapped(&store));
    }

    #[test]
    fn an_editors_save_and_a_drop_stamp_what_they_unmap() {
        let mut store = Store::open_in_memory().unwrap();
        indexed(&mut store, "/app", "widget.rb", V1);
        let v2 = hash_blob(V2.as_bytes());
        let facts = crate::extract::extract(V2.as_bytes());
        assert!(
            store
                .refresh_file("/app", "widget.rb", &v2, Some(&facts))
                .unwrap()
        );
        assert_eq!(
            unmapped(&store)
                .iter()
                .map(|(o, _)| o.clone())
                .collect::<Vec<_>>(),
            [oid(V1)]
        );
        store.drop_checkout("/app").unwrap();
        let mut stamped: Vec<String> = unmapped(&store).into_iter().map(|(o, _)| o).collect();
        stamped.sort();
        let mut both = vec![oid(V1), oid(V2)];
        both.sort();
        assert_eq!(stamped, both);
    }

    /// As 0.9.5 leaves a version it unmaps: no stamp. Before this build
    /// ran, or after it, from an older trekr still writing the store.
    fn forget_stamps(store: &Store) {
        store.conn.execute_batch("DELETE FROM loose_blob;").unwrap();
    }

    #[test]
    fn a_version_an_older_build_unmapped_is_stamped_as_now_whenever_found() {
        let mut store = Store::open_in_memory().unwrap();
        indexed(&mut store, "/app", "widget.rb", V1);
        indexed(&mut store, "/app", "widget.rb", V2);
        forget_stamps(&store);
        assert_eq!(store.retained(None).unwrap().blobs, 1, "counted unstamped");
        assert!(store.any_unstamped().unwrap());
        assert_eq!(store.stamp_unmapped().unwrap(), 1);
        assert!(!store.any_unstamped().unwrap());
        assert_eq!(store.stamp_unmapped().unwrap(), 0, "nothing left");
        let garbage = store
            .collect(now() - 60, Some(now() - 60), |_| true, false)
            .unwrap();
        assert_eq!(garbage.blobs, 0, "nothing goes the day it is found");

        // And again later: an older trekr unmapped another since.
        indexed(
            &mut store,
            "/app",
            "widget.rb",
            "class Widget\n  def v3; end\nend\n",
        );
        forget_stamps(&store);
        assert_eq!(store.retained(None).unwrap().blobs, 2);
        assert_eq!(store.stamp_unmapped().unwrap(), 2);
    }

    #[test]
    fn retained_counts_the_versions_nothing_maps() {
        let mut store = Store::open_in_memory().unwrap();
        indexed(&mut store, "/app", "widget.rb", V1);
        indexed(&mut store, "/app", "widget.rb", V2);
        let kept = store.retained(Some(now() - 60)).unwrap();
        assert_eq!((kept.blobs, kept.expired), (1, 0));
        age_unmapped(&store);
        assert_eq!(store.retained(Some(now() - 60)).unwrap().expired, 1);
    }

    #[test]
    fn the_background_collection_goes_in_batches() {
        let mut store = Store::open_in_memory().unwrap();
        for n in 0..5 {
            indexed(
                &mut store,
                "/app",
                "widget.rb",
                &format!("class W{n}\nend\n"),
            );
        }
        age_unmapped(&store);
        assert_eq!(store.collect_unmapped(now() - 60, 3).unwrap().blobs, 3);
        assert_eq!(store.collect_unmapped(now() - 60, 3).unwrap().blobs, 1);
        assert_eq!(store.collect_unmapped(now() - 60, 3).unwrap().blobs, 0);
        assert_eq!(store.totals().unwrap().blobs, 1, "the mapped one stays");
    }

    #[test]
    fn a_blob_a_query_may_still_be_reading_is_kept_whatever_the_cutoff() {
        let mut store = Store::open_in_memory().unwrap();
        let src = "class Draft\nend\n";
        let edit = hash_blob(src.as_bytes());
        store
            .add_blob(&edit, &crate::extract::extract(src.as_bytes()))
            .unwrap();
        // `--older-than 0`, while the query that recorded it answers.
        let garbage = store
            .collect(now() + 1, Some(now() + 1), |_| true, false)
            .unwrap();
        assert_eq!(garbage.blobs, 0);
        assert!(store.has_blob(&edit).unwrap(), "just recorded: kept");
        store
            .conn
            .execute("UPDATE loose_blob SET recorded_at = 0", [])
            .unwrap();
        let garbage = store
            .collect(now() - 60, Some(now() - 60), |_| true, false)
            .unwrap();
        assert_eq!(garbage.blobs, 1, "recorded long ago: collected");
    }

    #[test]
    fn a_loose_blob_table_an_earlier_build_laid_down_is_brought_up_to_date() {
        let blob = |src: &str| {
            (
                hash_blob(src.as_bytes()),
                crate::extract::extract(src.as_bytes()),
            )
        };
        // Without the table, and with it but no `recorded_at`.
        for (shape, recorded_before) in [
            ("DROP TABLE loose_blob;", 0),
            (
                "CREATE TABLE before AS SELECT blob_id FROM loose_blob;
                 DROP TABLE loose_blob;
                 CREATE TABLE loose_blob (
                   blob_id INTEGER PRIMARY KEY REFERENCES blob(id) ON DELETE CASCADE
                 );
                 INSERT INTO loose_blob SELECT blob_id FROM before;
                 DROP TABLE before;",
                1,
            ),
        ] {
            let mut store = Store::open_in_memory().unwrap();
            let (old, facts) = blob("class Old\nend\n");
            store.add_blob(&old, &facts).unwrap();
            store.conn.execute_batch(shape).unwrap();
            let (draft, facts) = blob("class Draft\nend\n");
            store
                .add_blob(&draft, &facts)
                .expect("a query records its edit");
            assert!(store.has_blob(&draft).unwrap());
            let garbage = store
                .collect(now() - 60, Some(now() - 60), |_| true, false)
                .unwrap();
            assert_eq!(garbage.blobs, recorded_before, "{shape}");
            assert!(store.has_blob(&draft).unwrap(), "just recorded: kept");
            indexed(&mut store, "/app", "draft.rb", "class Draft\nend\n");
        }
    }

    #[test]
    fn a_dry_run_reports_the_same_and_keeps_everything() {
        let mut store = Store::open_in_memory().unwrap();
        bundle(&mut store);
        let dry = store
            .collect(now() - 60, Some(now() - 60), |_| true, true)
            .unwrap();
        assert_eq!(roots(&dry), ["/gem-1"]);
        assert!(store.has_checkout("/gem-1").unwrap(), "rolled back");
        let real = store
            .collect(now() - 60, Some(now() - 60), |_| true, false)
            .unwrap();
        assert_eq!(
            (dry.files, dry.blobs, dry.facts, dry.freed_bytes),
            (real.files, real.blobs, real.facts, real.freed_bytes),
            "a dry run measures the real thing"
        );
    }

    #[test]
    fn nothing_seen_since_the_cutoff_is_collected() {
        let mut store = Store::open_in_memory().unwrap();
        bundle(&mut store);
        // gem-1 was last seen at 0, so a cutoff before that spares it.
        let garbage = store.collect(-1, Some(-1), |_| false, false).unwrap();
        assert!(garbage.checkouts.is_empty(), "{:?}", roots(&garbage));
    }

    #[test]
    fn a_stdlib_lives_as_long_as_an_app_runs_on_it() {
        let mut store = Store::open_in_memory().unwrap();
        indexed(&mut store, "/app", "app.rb", "class App\nend\n");
        for ruby in ["/ruby-1", "/ruby-2"] {
            checkout(&mut store, ruby, &[("set.rb", "class Set\nend\n")]);
        }
        store.set_gems_used("/app", &[], Some("/ruby-1")).unwrap();
        store.set_gems_used("/app", &[], Some("/ruby-2")).unwrap();
        store
            .conn
            .execute(
                "UPDATE checkout SET indexed_at = 0 WHERE root = '/ruby-1'",
                [],
            )
            .unwrap();
        let garbage = store
            .collect(now() - 60, Some(now() - 60), |_| true, false)
            .unwrap();
        assert_eq!(roots(&garbage), ["/ruby-1"]);
        assert_eq!(garbage.checkouts[0].kind, "stdlib");
    }

    #[test]
    fn a_gem_a_bundle_named_recently_is_spared_however_old_its_index() {
        // Its clock is the last bundle that named it, not its first parse:
        // gem-1 was parsed long ago, named again just now, then dropped.
        let mut store = Store::open_in_memory().unwrap();
        bundle(&mut store);
        store
            .set_gems_used("/app", &[("/gem-1".into(), "gem".into())], None)
            .unwrap();
        store
            .set_gems_used("/app", &[("/gem-2".into(), "gem".into())], None)
            .unwrap();
        let garbage = store
            .collect(now() - 60, Some(now() - 60), |_| true, false)
            .unwrap();
        assert!(garbage.checkouts.is_empty(), "{:?}", roots(&garbage));
    }

    #[test]
    fn a_vanished_repo_goes_and_takes_its_claim_on_a_gem_with_it() {
        let mut store = Store::open_in_memory().unwrap();
        bundle(&mut store);
        store
            .conn
            .execute("UPDATE checkout SET indexed_at = 0", [])
            .unwrap();
        let garbage = store
            .collect(now() - 60, Some(now() - 60), |root| root != "/app", false)
            .unwrap();
        let mut got = roots(&garbage);
        got.sort();
        assert_eq!(got, ["/app", "/gem-1", "/gem-2"]);
        assert_eq!(store.totals().unwrap().blobs, 0);
    }

    #[test]
    fn a_gem_whose_directory_is_gone_goes_even_if_a_bundle_still_names_it() {
        // The app has not been re-indexed since the gem was uninstalled; its
        // claim points at nothing any answer could open.
        let mut store = Store::open_in_memory().unwrap();
        bundle(&mut store);
        store
            .conn
            .execute(
                "UPDATE checkout SET indexed_at = 0 WHERE root = '/gem-2'",
                [],
            )
            .unwrap();
        let garbage = store
            .collect(now() - 60, Some(now() - 60), |root| root != "/gem-2", false)
            .unwrap();
        let reasons: Vec<(&str, &str)> = garbage
            .checkouts
            .iter()
            .map(|c| (c.repo.as_str(), c.reason))
            .collect();
        assert!(reasons.contains(&("/gem-2", "vanished")), "{reasons:?}");
    }
}
