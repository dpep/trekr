//! Collecting checkouts nothing will ask about again (DEC-049).
//!
//! Content addressing means an old gem version is never an orphan — it still
//! maps its own blobs — so "blobs no file references" finds nothing (DEC-030)
//! while every version any project ever resolved is kept forever. The unit of
//! collection is therefore the checkout, and a blob goes only when the last
//! checkout mapping it does. A blob a query recorded and no index mapped
//! (DEC-035) was never any checkout's, so it goes once nothing maps it.

use super::Store;
use rusqlite::{Result, params};
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
    /// only a query recorded. A blob a checkout still maps is never among
    /// them.
    pub(crate) blobs: usize,
    /// Fact rows (defs, ancestry, constant refs, call sites) those blobs held.
    pub(crate) facts: usize,
    /// Pages the deletion returned to SQLite's free list, which later writes
    /// reuse. The file itself shrinks only on `--vacuum`.
    pub(crate) reclaimed_bytes: i64,
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

        let mut garbage = Garbage::default();
        let page_size: i64 = tx.query_row("PRAGMA page_size", [], |r| r.get(0))?;
        let free_before: i64 = tx.query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
        tx.execute_batch("CREATE TEMP TABLE gc_blob (id INTEGER PRIMARY KEY)")?;
        {
            let mut candidates = tx.prepare(
                "INSERT OR IGNORE INTO gc_blob SELECT blob_id FROM file WHERE checkout_id = ?1",
            )?;
            let mut forget = tx.prepare("DELETE FROM checkout WHERE id = ?1")?;
            for row in &doomed {
                garbage.files += candidates.execute(params![row.id])?;
                // Cascades to the file map and to the bundle it claimed.
                forget.execute(params![row.id])?;
            }
        }
        // What a query recorded and no index mapped (DEC-035): the edits it
        // read, kept only while a map points at them — and, like a checkout,
        // until the cutoff, but never while the query may be reading it.
        tx.execute_batch(super::schema::LOOSE_BLOB)?;
        tx.execute(
            "INSERT OR IGNORE INTO gc_blob
             SELECT blob_id FROM loose_blob WHERE recorded_at <= MIN(?1, unixepoch() - ?2)",
            params![cutoff, READING],
        )?;
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
        garbage.signatures = unserved_signatures(&tx)?;
        garbage.core_dirs = core_dirs(&tx)?;
        let free_after: i64 = tx.query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
        garbage.reclaimed_bytes = (free_after - free_before).max(0) * page_size;

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

    /// Rewrite the database without its free pages, and fold the WAL back in
    /// so the file on disk actually shrinks. Seconds on a large store and it
    /// holds the write lock throughout, so it is asked for, never implied.
    pub(crate) fn vacuum(&self) -> Result<()> {
        self.conn
            .execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);")
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

        let garbage = store.collect(now() - 60, |_| true, false).unwrap();
        assert_eq!(roots(&garbage), ["/gem-1"]);
        assert_eq!(garbage.checkouts[0].reason, "unclaimed");
        assert_eq!(garbage.checkouts[0].kind, "gem");
        // `own.rb` went; `shared.rb` is still mapped by gem-2.
        assert_eq!((garbage.files, garbage.blobs), (2, 1));
        assert!(garbage.facts > 0 && garbage.reclaimed_bytes >= 0);
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
        // wants it (DEC-003), whoever asks.
        indexed(&mut store, "/app", "kept.rb", "class Before\nend\n");
        let (before, _) = blob("class Before\nend\n");
        indexed(&mut store, "/app", "kept.rb", "class Kept\nend\n");

        let dry = store.collect(now() - 60, |_| true, true).unwrap();
        assert_eq!(dry.blobs, 1, "a dry run counts it");
        assert!(store.has_blob(&edit).unwrap(), "and keeps it");
        let garbage = store.collect(now() - 60, |_| true, false).unwrap();
        assert!(garbage.checkouts.is_empty());
        assert_eq!(garbage.blobs, 1);
        assert!(garbage.facts > 0);
        assert!(!store.has_blob(&edit).unwrap(), "the orphan went");
        assert!(store.has_blob(&committed).unwrap(), "a mapped blob stays");
        assert!(store.has_blob(&before).unwrap(), "an index's blob stays");
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
        let garbage = store.collect(now() + 1, |_| true, false).unwrap();
        assert_eq!(garbage.blobs, 0);
        assert!(store.has_blob(&edit).unwrap(), "just recorded: kept");
        store
            .conn
            .execute("UPDATE loose_blob SET recorded_at = 0", [])
            .unwrap();
        let garbage = store.collect(now() - 60, |_| true, false).unwrap();
        assert_eq!(garbage.blobs, 1, "recorded long ago: collected");
    }

    #[test]
    fn a_dry_run_reports_the_same_and_keeps_everything() {
        let mut store = Store::open_in_memory().unwrap();
        bundle(&mut store);
        let dry = store.collect(now() - 60, |_| true, true).unwrap();
        assert_eq!(roots(&dry), ["/gem-1"]);
        assert!(store.has_checkout("/gem-1").unwrap(), "rolled back");
        let real = store.collect(now() - 60, |_| true, false).unwrap();
        assert_eq!(
            (dry.files, dry.blobs, dry.facts, dry.reclaimed_bytes),
            (real.files, real.blobs, real.facts, real.reclaimed_bytes),
            "a dry run measures the real thing"
        );
    }

    #[test]
    fn nothing_seen_since_the_cutoff_is_collected() {
        let mut store = Store::open_in_memory().unwrap();
        bundle(&mut store);
        // gem-1 was last seen at 0, so a cutoff before that spares it.
        let garbage = store.collect(-1, |_| false, false).unwrap();
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
        let garbage = store.collect(now() - 60, |_| true, false).unwrap();
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
        let garbage = store.collect(now() - 60, |_| true, false).unwrap();
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
            .collect(now() - 60, |root| root != "/app", false)
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
            .collect(now() - 60, |root| root != "/gem-2", false)
            .unwrap();
        let reasons: Vec<(&str, &str)> = garbage
            .checkouts
            .iter()
            .map(|c| (c.repo.as_str(), c.reason))
            .collect();
        assert!(reasons.contains(&("/gem-2", "vanished")), "{reasons:?}");
    }
}
