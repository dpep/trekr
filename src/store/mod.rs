//! SQLite, WAL, and nothing clever.
//!
//! Facts are keyed by blob OID, so two worktrees of one repo store one copy and
//! a branch switch reparses only what is genuinely new. The store's job is to
//! make that diff cheap and to stay out of the way otherwise.
//!
//! Conventions (pragmas, `user_version` as the migration marker, `$TREKR_DB`)
//! follow rq's `src/store/`.

mod gc;
mod schema;

pub(crate) use schema::VERSION;

use crate::core::*;
use crate::scan::Files;
use rusqlite::{Connection, OptionalExtension, Result, params};
use std::collections::{HashMap, HashSet};
use std::path::Path;

pub(crate) struct Store {
    conn: Connection,
    /// Where this store lives, so a second connection to it can be opened.
    /// `None` for an in-memory store, which cannot be reached twice.
    path: Option<std::path::PathBuf>,
}

/// What one indexing pass did. Every count is honest about *work*, not about
/// contents: `parsed` is the only expensive number in it.
#[derive(Debug, Default, serde::Serialize)]
pub(crate) struct Indexed {
    pub(crate) files: usize,
    /// Distinct blobs the checkout references.
    pub(crate) blobs: usize,
    /// Blobs whose bytes this machine had never seen. A reindex with no edits
    /// makes this zero, which is the entire point of blob keying.
    pub(crate) parsed: usize,
    pub(crate) defs: usize,
    pub(crate) refs: usize,
    pub(crate) calls: usize,
}

/// Where the database lives: `$TREKR_DB`, else `~/.local/share/trekr/trekr.db`.
pub(crate) fn default_path() -> anyhow::Result<std::path::PathBuf> {
    Ok(match std::env::var("TREKR_DB") {
        Ok(path) => std::path::PathBuf::from(path),
        Err(_) => {
            std::path::PathBuf::from(std::env::var("HOME")?).join(".local/share/trekr/trekr.db")
        }
    })
}

/// Ruby core, written out beside the database as real readable files — one
/// per owner, `core/String.rb` — and the directory they are in.
///
/// The stub is compiled into the binary, so a definition in it had no location
/// to point at and every `require` or `Array#each` answered nothing — worse
/// than ruby-lsp, which at least sends you to an RBS declaration. Writing it
/// out means "go to definition" lands on a signature a person can read, in a
/// file whose name says whose it is.
pub(crate) fn core_dir() -> anyhow::Result<std::path::PathBuf> {
    let beside = default_path()?
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .to_path_buf();
    let dir = beside.join("core");
    crate::tree::materialize_core(&dir)?;
    // The single file earlier builds wrote, which nothing points into now.
    let _ = std::fs::remove_file(beside.join("core.rb"));
    Ok(dir)
}

/// The database every command uses.
pub(crate) fn open_default() -> anyhow::Result<Store> {
    let path = default_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(Store::open(&path)?)
}

/// See `Store::files_calling`.
const FILES_CALLING: &str = "SELECT f.path
   FROM call_site s INDEXED BY call_site_name
   CROSS JOIN file f
  WHERE s.name = ?2
    AND f.blob_id = s.blob_id
    AND f.checkout_id = (SELECT id FROM checkout WHERE root = ?1)";

/// See `Store::files_calling_page`.
const FILES_CALLING_PAGE: &str = "SELECT s.rowid, f.path
   FROM call_site s INDEXED BY call_site_name
   CROSS JOIN file f
  WHERE s.name = ?2 AND s.rowid > ?3
    AND f.blob_id = s.blob_id
    AND f.checkout_id = (SELECT id FROM checkout WHERE root = ?1)
  ORDER BY s.rowid
  LIMIT ?4";

impl Store {
    pub(crate) fn open(path: &Path) -> Result<Store> {
        let mut store = Store::init(Connection::open(path)?)?;
        store.path = Some(path.to_path_buf());
        Ok(store)
    }

    /// A second connection to the same database.
    ///
    /// `None` for an in-memory store: there is no path to reach it by, and a
    /// caller that needs its own handle has to fall back to reading everything
    /// through the one it already holds.
    pub(crate) fn reopen(&self) -> Result<Option<Store>> {
        match &self.path {
            Some(path) => Store::open(path).map(Some),
            None => Ok(None),
        }
    }

    /// The database file, or `None` for an in-memory store.
    pub(crate) fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn open_in_memory() -> Result<Store> {
        Store::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Store> {
        // Before any other statement: switching to WAL and the migration below
        // both take locks, and without a handler a second process opening the
        // store at the same moment fails at once instead of waiting its turn.
        conn.busy_timeout(BUSY)?;
        // WAL lets a reader answer while an indexer writes.
        wal(&conn)?;
        // mmap reads pages from the shared page cache instead of copying each
        // into this connection's own cache — the difference is most of an LSP
        // server's private memory (DEC-051). An I/O error under a mapping is a
        // SIGBUS rather than an error return, the risk rq's D8 took too.
        conn.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA synchronous=NORMAL; PRAGMA temp_store=MEMORY; PRAGMA cache_size=-32768; \
             PRAGMA mmap_size=1073741824;",
        )?;
        let mut store = Store { conn, path: None };
        if schema_version(&store.conn)? != schema::VERSION {
            store.migrate()?;
        }
        Ok(store)
    }

    /// Bring the schema to this binary's, as one transaction (DEC-079).
    ///
    /// The version is read again under the write lock: another process may
    /// have rebuilt the store between the unlocked check and here, and a
    /// second drop-and-create interleaved with the first is what left tables
    /// from two generations side by side.
    fn migrate(&mut self) -> Result<()> {
        // A no-op inside a transaction, so it is set around one. Off, the
        // drops are plain drops rather than a cascading delete of every fact.
        self.conn.execute_batch("PRAGMA foreign_keys=OFF;")?;
        let rebuilt = (|| {
            let tx = self
                .conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let version = schema_version(&tx)?;
            // An *older* binary must not drop a newer database. Two trekrs on
            // one machine — one installed, one freshly built — would otherwise
            // take turns wiping each other's index, and each would look like
            // it had simply never been run.
            if version > schema::VERSION {
                return Err(schema_mismatch(format!(
                    "database is schema v{version} but this trekr speaks v{}; \
                     upgrade trekr, or point $TREKR_DB elsewhere",
                    schema::VERSION
                )));
            }
            if version == schema::VERSION {
                return Ok(());
            }
            // No migration, by design: see schema::VERSION. Reindexing costs
            // seconds and cannot leave the store half-converted.
            for table in schema::TABLES {
                tx.execute_batch(&format!("DROP TABLE IF EXISTS {table};"))?;
            }
            tx.execute_batch(schema::SCHEMA)?;
            if version != 0 {
                tx.execute(
                    "INSERT INTO upgrade (from_version, at) VALUES (?1, unixepoch())",
                    params![version],
                )?;
            }
            tx.pragma_update(None, "user_version", schema::VERSION)?;
            tx.commit()
        })();
        self.conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        rebuilt.map_err(terse)
    }

    /// Refuse to write into a store another binary has since rebuilt for a
    /// different schema. Asked inside the write's own transaction, so the
    /// answer holds until it commits.
    fn check_schema(conn: &Connection) -> Result<()> {
        let version = schema_version(conn)?;
        if version == schema::VERSION {
            return Ok(());
        }
        Err(schema_mismatch(format!(
            "the store is now schema v{version} and this trekr writes v{}; \
             it was rebuilt by another trekr, so this one must restart",
            schema::VERSION
        )))
    }

    /// The schema the store was last rebuilt from, when that rebuild threw
    /// an older index away — so "not indexed" can say why.
    pub(crate) fn upgraded_from(&self) -> Result<Option<i64>> {
        self.conn
            .query_row(
                "SELECT from_version FROM upgrade ORDER BY at DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()
    }

    /// Every blob OID this machine has already read.
    ///
    /// Loaded whole rather than probed per OID: at 100k blobs it is a few MB
    /// and one query, where the probe is 100k round trips. An index loads it
    /// once and adds what it writes — reloading it per gem rescanned the
    /// table ~300 times on a cold bundle.
    pub(crate) fn blob_oids(&self) -> Result<HashSet<Oid>> {
        let mut stmt = self.conn.prepare("SELECT oid FROM blob")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0).map(Oid))?;
        rows.collect()
    }

    /// Has this machine read this one blob? One probe of the OID index, for
    /// a caller that refreshes a single file.
    pub(crate) fn has_blob(&self, oid: &Oid) -> Result<bool> {
        self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM blob WHERE oid = ?1)",
            params![oid.0],
            |r| r.get(0),
        )
    }

    /// Record one checkout's file map and any facts it brought with it.
    ///
    /// One savepoint: an interrupted index leaves the previous state intact
    /// rather than a half-mapped checkout. A savepoint rather than a
    /// transaction so that it nests inside `batch`.
    pub(crate) fn write(
        &mut self,
        root: &str,
        files: &Files,
        facts: impl IntoIterator<Item = (Oid, Facts)>,
        git_state: i64,
    ) -> Result<Indexed> {
        self.write_with(root, files, facts, git_state, false)
    }

    /// `write`, for a load that will more than double the store: the fact
    /// tables' secondary indexes are dropped, the rows inserted, and the
    /// indexes rebuilt by sorting (DEC-057). Inserting into them row by row is
    /// random I/O once they outgrow the cache, and made a cold index grow
    /// faster than the repo did. All of it is one savepoint, so a reader never
    /// sees the store without its indexes and an interrupted load rolls the
    /// drop back with everything else.
    pub(crate) fn write_bulk(
        &mut self,
        root: &str,
        files: &Files,
        facts: impl IntoIterator<Item = (Oid, Facts)>,
        git_state: i64,
    ) -> Result<Indexed> {
        self.write_with(root, files, facts, git_state, true)
    }

    fn write_with(
        &mut self,
        root: &str,
        files: &Files,
        facts: impl IntoIterator<Item = (Oid, Facts)>,
        git_state: i64,
        bulk: bool,
    ) -> Result<Indexed> {
        // On its own, the write is its own immediate transaction: a deferred
        // one that reads first cannot wait for the lock, it fails.
        if self.autocommit() {
            return self.batch(|store| store.write_with(root, files, facts, git_state, bulk));
        }
        let tx = self.conn.savepoint()?;
        Store::check_schema(&tx)?;
        let mut counts = Indexed {
            files: files.len(),
            ..Indexed::default()
        };
        if bulk {
            for (name, _) in schema::BULK_INDEXES {
                tx.execute_batch(&format!("DROP INDEX IF EXISTS {name};"))?;
            }
        }

        for (oid, f) in facts {
            let (oid, f) = (&oid, &f);
            counts.parsed += 1;
            counts.defs += f.defs.len();
            counts.refs += f.const_refs.len();
            counts.calls += f.calls.len();
            insert_facts(&tx, oid, f)?;
        }
        if bulk {
            // The sort spills to disk: `temp_store` is MEMORY for queries, and
            // there a 30× monorepo's sort held 1.5 GB.
            tx.execute_batch("PRAGMA temp_store=FILE;")?;
            for (_, create) in schema::BULK_INDEXES {
                tx.execute_batch(create)?;
            }
            tx.execute_batch("PRAGMA temp_store=MEMORY;")?;
        }

        tx.execute(
            "INSERT OR IGNORE INTO checkout (root, indexed_at, surface_key, map_key, git_state)
             VALUES (?1, unixepoch(), 0, 0, 0)",
            params![root],
        )?;
        tx.execute(
            "UPDATE checkout SET indexed_at = unixepoch() WHERE root = ?1",
            params![root],
        )?;
        let checkout_id: i64 = tx.query_row(
            "SELECT id FROM checkout WHERE root = ?1",
            params![root],
            |r| r.get(0),
        )?;

        // What the map *would* be, folded before any of it is written. When it
        // matches what is stored the map is identical and the rewrite below is
        // pure cost — which on a no-op index is the only cost left, and the one
        // that grows with the repo.
        let map_key = map_key(files);
        // `EXISTS` rather than `COUNT`: the question is whether the map was
        // ever written, and counting it would put an O(files) scan back into
        // the path this whole change exists to make O(1).
        let stored: (i64, bool) = tx.query_row(
            "SELECT map_key, EXISTS(SELECT 1 FROM file WHERE checkout_id = ?1)
               FROM checkout WHERE id = ?1",
            params![checkout_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        // A stored key of 0 against a map with no rows is the initial state,
        // not a match — an empty checkout must still be written once.
        if stored.0 == map_key && stored.1 {
            counts.blobs = files.values().collect::<HashSet<&Oid>>().len();
            // Still record git's view. The map did not move, but git's index
            // may have — a commit touching no Ruby file, for instance — and
            // leaving the old fingerprint would make every later query probe
            // stale forever.
            tx.execute(
                "UPDATE checkout SET git_state = ?2 WHERE id = ?1",
                params![checkout_id, git_state],
            )?;
            tx.commit()?;
            return Ok(counts);
        }

        // Only the rows that moved are written. The stored map is read whole —
        // one query — and diffed here: a path whose blob is unchanged costs
        // nothing, a vanished path is deleted, and anything new or edited is
        // upserted. Rewriting every row was most of a one-file reindex.
        let mut stored: HashMap<String, (i64, String, i64)> = HashMap::new();
        {
            let mut read = tx.prepare(
                "SELECT f.path, f.blob_id, b.oid, b.surface
                   FROM file f JOIN blob b ON b.id = f.blob_id
                  WHERE f.checkout_id = ?1",
            )?;
            let rows = read.query_map(params![checkout_id], |r| {
                Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?, r.get(3)?)))
            })?;
            for row in rows {
                let (path, found) = row?;
                stored.insert(path, found);
            }
        }
        let mut surface_key: i64 = 0;
        {
            let mut ids: HashMap<&Oid, (i64, i64)> = HashMap::new();
            let mut lookup = tx.prepare("SELECT id, surface FROM blob WHERE oid = ?1")?;
            let mut upsert = tx.prepare(
                "INSERT OR REPLACE INTO file (checkout_id, path, blob_id) VALUES (?1, ?2, ?3)",
            )?;
            for (path, oid) in files {
                let (id, surface) = match stored.remove(path) {
                    Some((id, known, surface)) if known == oid.0 => (id, surface),
                    _ => {
                        let found = match ids.get(oid) {
                            Some(found) => *found,
                            None => lookup.query_row(params![oid.0], |r| {
                                Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
                            })?,
                        };
                        upsert.execute(params![checkout_id, path, found.0])?;
                        found
                    }
                };
                ids.insert(oid, (id, surface));
                // Order-independent, so the map's iteration order cannot
                // change the key; the path is mixed in because a rename moves
                // where an answer points even when no blob changed.
                surface_key = surface_key.wrapping_add(path_hash(path) ^ surface);
            }
            counts.blobs = ids.len();
            // What is left was stored and is no longer in the checkout.
            let mut delete = tx.prepare("DELETE FROM file WHERE checkout_id = ?1 AND path = ?2")?;
            for path in stored.keys() {
                delete.execute(params![checkout_id, path])?;
            }
        }

        tx.execute(
            "UPDATE checkout SET surface_key = ?2, map_key = ?3, git_state = ?4 WHERE id = ?1",
            params![checkout_id, surface_key, map_key, git_state],
        )?;

        tx.commit()?;
        Ok(counts)
    }

    /// Is this exactly the map already stored for `root`? The same test
    /// `write` makes before skipping the rewrite, asked before anything is
    /// read — a no-op index then never loads the known blobs at all.
    pub(crate) fn map_unchanged(&self, root: &str, files: &Files) -> Result<bool> {
        let stored: Option<(i64, bool)> = self
            .conn
            .query_row(
                "SELECT map_key, EXISTS(SELECT 1 FROM file WHERE checkout_id = checkout.id)
                   FROM checkout WHERE root = ?1",
                params![root],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(stored.is_some_and(|(key, written)| written && key == map_key(files)))
    }

    /// Outside any transaction — so a write here commits on its own rather
    /// than inside a `batch`.
    pub(crate) fn autocommit(&self) -> bool {
        self.conn.is_autocommit()
    }

    /// Run `work` as one transaction, so every `write` inside it commits once.
    ///
    /// For many small writes in a row — a bundle's gems. A commit rewrites
    /// every index page the transaction touched, and the name indexes are
    /// keyed randomly, so each small commit rewrote most of them (DEC-041).
    ///
    /// Immediate, so it waits for the write lock up front. A deferred
    /// transaction that reads before it writes gets `SQLITE_BUSY` at the
    /// write, without the busy handler, whenever another process wrote since.
    pub(crate) fn batch<T, E: From<rusqlite::Error>>(
        &mut self,
        work: impl FnOnce(&mut Store) -> std::result::Result<T, E>,
    ) -> std::result::Result<T, E> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        match work(self) {
            Ok(value) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(value)
            }
            Err(error) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// One row per indexed checkout, plus the totals a caller wants to see.
    pub(crate) fn status(&self) -> Result<Vec<Checkout>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.root, c.indexed_at, COUNT(f.path), COUNT(DISTINCT f.blob_id)
               FROM checkout c LEFT JOIN file f ON f.checkout_id = c.id
              GROUP BY c.id ORDER BY c.root",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Checkout {
                repo: r.get(0)?,
                indexed_at: r.get(1)?,
                files: r.get(2)?,
                blobs: r.get(3)?,
            })
        })?;
        rows.collect()
    }

    /// Totals across every checkout — the shared cost, counted once.
    pub(crate) fn totals(&self) -> Result<Totals> {
        let one = |sql: &str| -> Result<i64> { self.conn.query_row(sql, [], |r| r.get(0)) };
        Ok(Totals {
            blobs: one("SELECT COUNT(*) FROM blob")?,
            defs: one("SELECT COUNT(*) FROM def")?,
            const_refs: one("SELECT COUNT(*) FROM const_ref")?,
            calls: one("SELECT COUNT(*) FROM call_site")?,
        })
    }

    /// Every mention of a name in one checkout: definitions, constant
    /// references, and call sites, in source order.
    ///
    /// **Name-level, not resolved.** Two unrelated classes called `Config` both
    /// answer here, and so does every `#save` on every receiver. Each row says
    /// what sort of mention it is and — for a call — what shape the receiver
    /// had, which is what the resolve layer will narrow on. Saying that plainly
    /// is better than a number that implies more than it knows.
    pub(crate) fn refs(&self, root: &str, name: &str) -> Result<Vec<Ref>> {
        let mut stmt = self.conn.prepare(
            "SELECT f.path, x.line, x.col, x.role, x.kind, x.recv, x.recv_text, x.nesting
               FROM (
                 SELECT blob_id, line, col, 'definition' AS role, kind,
                        NULL AS recv, NULL AS recv_text, nesting
                   FROM def WHERE name = ?2
                 UNION ALL
                 SELECT blob_id, line, col, 'constant', NULL, NULL, NULL, nesting
                   FROM const_ref WHERE name = ?2
                 UNION ALL
                 SELECT blob_id, line, col, 'call', NULL, recv, recv_text, nesting
                   FROM call_site WHERE name = ?2
               ) x
               JOIN file f ON f.blob_id = x.blob_id
               JOIN checkout c ON c.id = f.checkout_id
              WHERE c.root = ?1
              ORDER BY f.path, x.line, x.col",
        )?;
        let rows = stmt.query_map(params![root, name], |r| {
            Ok(Ref {
                path: r.get(0)?,
                line: r.get(1)?,
                col: r.get(2)?,
                role: r.get(3)?,
                kind: r.get(4)?,
                recv: r.get(5)?,
                recv_text: r.get(6)?,
                nesting: split_nesting(&r.get::<_, String>(7)?),
                tier: None,
                owner: None,
            })
        })?;
        rows.collect()
    }

    /// Every class, module, and constant declared in a checkout, in a stable
    /// order (by path, then line) so that reopening a class reads the same way
    /// on every rebuild.
    ///
    /// This and [`Store::ancestry`] are the tree layer's whole input. Note what
    /// is *not* here: no resolution, no ordering by significance. The blob
    /// layer hands over facts and stops.
    /// Paths come back **absolute**. A tree spans several checkouts — the
    /// repo and every gem it resolves — so a checkout-relative path stops
    /// meaning anything the moment it leaves this query, and a caller that
    /// joined one onto the repo it happened to be asking about fabricated
    /// files that do not exist.
    pub(crate) fn declarations(&self, roots: &[String]) -> Result<Vec<DeclRow>> {
        // Ordered here rather than by `ORDER BY c.id, f.path, d.line, d.col`:
        // SQLite's sorter carried every row's absolute path through a temp
        // b-tree and was a third of this query. Same keys, same byte order.
        let mut stmt = self.conn.prepare(&format!(
            "SELECT d.name, d.kind, d.nesting, d.target, c.root || '/' || f.path, d.line, d.col,
                    c.id
               FROM def d
               JOIN file f ON f.blob_id = d.blob_id
               JOIN checkout c ON c.id = f.checkout_id
              WHERE c.root IN ({}) AND d.kind IN ('class','module','constant')",
            placeholders(roots.len())
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(roots), |r| {
            Ok((
                r.get::<_, i64>(7)?,
                DeclRow {
                    name: r.get(0)?,
                    kind: r.get(1)?,
                    nesting: split_nesting(&r.get::<_, String>(2)?),
                    target: r.get(3)?,
                    path: r.get(4)?,
                    line: r.get(5)?,
                    col: r.get(6)?,
                },
            ))
        })?;
        let mut rows = rows.collect::<Result<Vec<_>>>()?;
        // Within one checkout the root is a shared prefix, so comparing the
        // absolute path orders exactly as the relative one would.
        rows.sort_by(|(a_id, a), (b_id, b)| {
            (a_id, &a.path, a.line, a.col).cmp(&(b_id, &b.path, b.line, b.col))
        });
        Ok(rows.into_iter().map(|(_, row)| row).collect())
    }

    /// Every method a checkout defines, in a stable order.
    ///
    /// Deferred in session 2 because nothing read it; the method ladder is the
    /// consumer that earns it.
    pub(crate) fn methods(&self, roots: &[String]) -> Result<Vec<MethodRow>> {
        self.method_rows(roots, None)
    }

    /// Just the methods with this name, for a tree that loads on demand.
    ///
    /// The whole point of the demand-loading design: nothing needs all 84,052
    /// of rails' methods, and `def(name)` is indexed, so one name is a few rows
    /// instead of a table scan and 137 ms of indexing.
    pub(crate) fn methods_named(&self, roots: &[String], name: &str) -> Result<Vec<MethodRow>> {
        self.method_rows(roots, Some(name))
    }

    /// Every method, in `methods`' order, handed over one row at a time
    /// rather than collected — for a caller that keeps only part of each.
    pub(crate) fn each_method(&self, roots: &[String], visit: impl FnMut(MethodRow)) -> Result<()> {
        self.visit_method_rows(roots, None, visit)
    }

    fn method_rows(&self, roots: &[String], name: Option<&str>) -> Result<Vec<MethodRow>> {
        let mut rows = Vec::new();
        self.visit_method_rows(roots, name, |row| rows.push(row))?;
        Ok(rows)
    }

    fn visit_method_rows(
        &self,
        roots: &[String],
        name: Option<&str>,
        mut visit: impl FnMut(MethodRow),
    ) -> Result<()> {
        // Insert order is load-bearing: `lookup` takes the last definition, so
        // a reopened class must arrive after the class it reopens.
        let filter = if name.is_some() { "AND d.name = ?" } else { "" };
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT d.name, d.nesting, d.singleton, d.visibility, d.params, d.via,
                    d.target, d.sig_returns, c.root || '/' || f.path, d.line, d.col,
                    d.target_line, d.target_col
               FROM def d
               JOIN file f ON f.blob_id = d.blob_id
               JOIN checkout c ON c.id = f.checkout_id
              WHERE c.root IN ({}) AND d.kind = 'method' {filter}
              ORDER BY c.id, f.path, d.line, d.col",
            placeholders(roots.len())
        ))?;
        let mut values: Vec<&dyn rusqlite::ToSql> =
            roots.iter().map(|r| r as &dyn rusqlite::ToSql).collect();
        if let Some(name) = name.as_ref() {
            values.push(name as &dyn rusqlite::ToSql);
        }
        let mut rows = stmt.query(values.as_slice())?;
        while let Some(r) = rows.next()? {
            let params: String = r.get(4)?;
            visit(MethodRow {
                name: r.get(0)?,
                nesting: split_nesting(&r.get::<_, String>(1)?),
                singleton: r.get::<_, i64>(2)? != 0,
                visibility: r.get(3)?,
                params: decode_params(&params),
                via: r.get(5)?,
                target: r.get(6)?,
                sig_returns: r.get(7)?,
                sig_overloads: Vec::new(),
                path: r.get(8)?,
                line: r.get(9)?,
                col: r.get(10)?,
                target_pos: match (r.get::<_, Option<u32>>(11)?, r.get::<_, Option<u32>>(12)?) {
                    (Some(line), Some(col)) => Some(crate::core::Pos { line, col }),
                    _ => None,
                },
            });
        }
        Ok(())
    }

    /// Every ancestry edge in a checkout, in source order — which is the order
    /// Ruby applies them in, and therefore the order linearization reverses.
    pub(crate) fn ancestry(&self, roots: &[String]) -> Result<Vec<EdgeRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT a.owner, a.relation, a.target, c.root || '/' || f.path
               FROM ancestry a
               JOIN file f ON f.blob_id = a.blob_id
               JOIN checkout c ON c.id = f.checkout_id
              WHERE c.root IN ({})
              ORDER BY c.id, f.path, a.line, a.col",
            placeholders(roots.len())
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(roots), |r| {
            Ok(EdgeRow {
                owner: split_nesting(&r.get::<_, String>(0)?),
                relation: r.get(1)?,
                target: r.get(2)?,
                path: r.get(3)?,
            })
        })?;
        rows.collect()
    }

    /// Files in a checkout that call a method of this name.
    ///
    /// The tiering reparses each of these rather than reading the stored call
    /// rows, so an edit since the last index is still tiered correctly — and
    /// the ladder needs the file's assignments anyway, which are not stored
    /// (DEC-012). The index's job here is to say which files are worth opening.
    ///
    /// The plan is pinned, as `files_calling_page`'s is and for the same
    /// reason: with statistics saying a name is everywhere, the bundled SQLite
    /// chose to walk every file of the checkout and sort all their calls.
    /// Deduplicated and ordered here rather than by a temp B-tree.
    pub(crate) fn files_calling(&self, root: &str, name: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare_cached(FILES_CALLING)?;
        let rows = stmt.query_map(params![root, name], |r| r.get(0))?;
        let mut paths = rows.collect::<Result<Vec<String>>>()?;
        paths.sort_unstable();
        paths.dedup();
        Ok(paths)
    }

    /// A page of the files in a checkout that call `name`, in the index's
    /// own order: the call rows after `after`, at most `rows` of them, as
    /// (the last row read, each row's file). Pass the last row back for the
    /// next page; an empty page is the end.
    ///
    /// For a question that may stop long before the last file. `files_calling`
    /// must read every call of the name to sort and deduplicate — 2.5 million
    /// rows for `to` on a monorepo thirty times discourse — where this reads
    /// only as far as it is asked to. A file with several calls appears once
    /// per call; the caller deduplicates.
    ///
    /// The plan is pinned: from the name's index, then its files. Left to
    /// itself, with statistics saying the name is everywhere, the bundled
    /// SQLite walks every file of the checkout and sorts all their calls —
    /// the whole listing again, 0.7 s a page at ten times discourse — or scans
    /// the call table in row order, which for a rare name reads all of it.
    pub(crate) fn files_calling_page(
        &self,
        root: &str,
        name: &str,
        after: i64,
        rows: i64,
    ) -> Result<(i64, Vec<String>)> {
        let mut stmt = self.conn.prepare_cached(FILES_CALLING_PAGE)?;
        let mut last = after;
        let mut paths = Vec::new();
        let mut found = stmt.query(params![root, name, after, rows])?;
        while let Some(row) = found.next()? {
            last = row.get(0)?;
            paths.push(row.get(1)?);
        }
        Ok((last, paths))
    }

    /// How often each of these names is written as a call in the checkout at
    /// `root`, counting up to `cap` — a name handed to a macro as a symbol is
    /// not counted.
    ///
    /// The cheap half of the dead-code filter (DEC-038). A name with hundreds
    /// of call sites is not a candidate and must never cost a receiver-narrowed
    /// pass to find that out; a name with none or a few is worth the expensive
    /// question. Counting by name is deliberately *generous* — every same-named
    /// call, whatever its receiver — so it only ever skips that pass.
    ///
    /// The checkout only, because the expensive pass reads only the checkout:
    /// counting the whole store let another repository's calls decide (DEC-074).
    ///
    /// Capped because the only question is "more than a few?": counting every
    /// call of a name like `id` to answer it was a third of a `--dead` run.
    pub(crate) fn written_calls(
        &self,
        root: &str,
        names: &[String],
        cap: i64,
    ) -> Result<HashMap<String, i64>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT COUNT(*) FROM
               (SELECT 1 FROM call_site s INDEXED BY call_site_name
                  CROSS JOIN file f
                 WHERE s.name = ?1 AND s.recv <> 'symbol'
                   AND f.blob_id = s.blob_id
                   AND f.checkout_id = (SELECT id FROM checkout WHERE root = ?2)
                 LIMIT ?3)",
        )?;
        let mut found = HashMap::new();
        for name in names {
            if !found.contains_key(name) {
                let count: i64 = stmt.query_row(params![name, root, cap], |r| r.get(0))?;
                found.insert(name.clone(), count);
            }
        }
        Ok(found)
    }

    /// Definitions whose name contains `query`, for `workspaceSymbol`.
    ///
    /// Substring, case-insensitive, capped. rq's scorer would rank these
    /// better; this is the LSP contract's shape, and a client that wants
    /// ranking can ask rq (PLAN §3).
    ///
    /// `root` of `None` searches every checkout — for a client whose workspace
    /// is not one of them, where the alternative is answering nothing.
    pub(crate) fn symbols_named(
        &self,
        root: Option<&str>,
        query: &str,
        limit: i64,
    ) -> Result<Vec<Symbol>> {
        let mut stmt = self.conn.prepare(
            "SELECT d.name, d.kind, d.nesting, d.singleton, d.visibility, d.params,
                    d.via, d.target, d.sig_returns, d.line, d.col, d.end_line, f.path, c.root
               FROM def d
               JOIN file f ON f.blob_id = d.blob_id
               JOIN checkout c ON c.id = f.checkout_id
              WHERE (?1 IS NULL OR c.root = ?1) AND d.name LIKE ?2 ESCAPE '\\'
              ORDER BY LENGTH(d.name), d.name
              LIMIT ?3",
        )?;
        let pattern = format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"));
        let rows = stmt.query_map(params![root, pattern, limit], |r| {
            let encoded: String = r.get(5)?;
            Ok(Symbol {
                name: r.get(0)?,
                kind: r.get(1)?,
                nesting: split_nesting(&r.get::<_, String>(2)?),
                singleton: r.get::<_, i64>(3)? != 0,
                visibility: r.get(4)?,
                params: decode_params(&encoded)
                    .into_iter()
                    .map(|p| format!("{}:{}", p.kind.as_str(), p.name))
                    .collect(),
                via: r.get(6)?,
                target: r.get(7)?,
                sig_returns: r.get(8)?,
                line: r.get(9)?,
                col: r.get(10)?,
                end_line: r.get(11)?,
                path: r.get(12)?,
                root: r.get(13)?,
            })
        })?;
        rows.collect()
    }

    /// The store's schema version, which DEC-013 makes cover the extractor too.
    /// Half of a resident front's staleness check.
    pub(crate) fn schema_version(&self) -> Result<i64> {
        schema_version(&self.conn)
    }

    /// The checkout's whole file map, folded into one number at index time.
    ///
    /// The other half of a resident front's staleness check, and the reason it
    /// is a *content* key: the file **count** does not move when a file is
    /// edited, so a session keyed on it went on answering from a tree
    /// assembled before the edit. This moves whenever any answer would.
    #[cfg(test)]
    pub(crate) fn surface_key(&self, root: &str) -> Result<i64> {
        self.conn
            .query_row(
                "SELECT surface_key FROM checkout WHERE root = ?1",
                params![root],
                |r| r.get(0),
            )
            .or(Ok(0))
    }

    /// Where each program in these checkouts starts: every root, and every
    /// directory in them holding a `.gemspec` — rails' `activemodel/` is a
    /// gem of its own inside the rails checkout (DEC-075). Absolute.
    pub(crate) fn program_roots(&self, roots: &[String]) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT c.root, f.path FROM file f JOIN checkout c ON c.id = f.checkout_id
              WHERE c.root IN ({}) AND f.path LIKE '%.gemspec'",
            placeholders(roots.len())
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(roots), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut found = roots.to_vec();
        for row in rows {
            let (root, path) = row?;
            if let Some((dir, _)) = path.rsplit_once('/') {
                found.push(format!("{root}/{dir}"));
            }
        }
        found.sort();
        found.dedup();
        Ok(found)
    }

    /// `surface_key` for each root in turn, in one query. A root the store
    /// has never indexed is 0, as there.
    pub(crate) fn surface_keys(&self, roots: &[String]) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT root, surface_key FROM checkout WHERE root IN ({})",
            placeholders(roots.len())
        ))?;
        let known: HashMap<String, i64> = stmt
            .query_map(rusqlite::params_from_iter(roots), |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect::<Result<_>>()?;
        Ok(roots
            .iter()
            .map(|root| known.get(root).copied().unwrap_or(0))
            .collect())
    }

    /// Every checkout's root.
    pub(crate) fn roots(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT root FROM checkout ORDER BY root")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        rows.collect()
    }

    /// Record that this checkout's bundle resolves these gems.
    ///
    /// Rewritten wholesale on every index, so a gem dropped
    /// from a Gemfile.lock stops being claimed.
    ///
    /// Also where a checkout becomes a gem, and where a gem is last seen: each
    /// one named is stamped `kind = 'gem'` and its `indexed_at` moved to now,
    /// inside the index's own transaction, so `--gc` costs a query nothing.
    pub(crate) fn set_gems_used(&mut self, root: &str, gem_roots: &[String]) -> Result<()> {
        let tx = self.conn.savepoint()?;
        let id: i64 = tx.query_row(
            "SELECT id FROM checkout WHERE root = ?1",
            params![root],
            |r| r.get(0),
        )?;
        tx.execute("DELETE FROM gem_use WHERE checkout_id = ?1", params![id])?;
        {
            let mut insert = tx
                .prepare("INSERT OR IGNORE INTO gem_use (checkout_id, gem_root) VALUES (?1, ?2)")?;
            let mut seen = tx.prepare(
                "UPDATE checkout SET kind = 'gem', indexed_at = unixepoch() WHERE root = ?1",
            )?;
            for gem in gem_roots {
                insert.execute(params![id, gem])?;
                seen.execute(params![gem])?;
            }
        }
        tx.commit()
    }

    /// The gem roots this checkout's bundle resolves, in a stable order.
    pub(crate) fn gems_used(&self, root: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT g.gem_root FROM gem_use g
               JOIN checkout c ON c.id = g.checkout_id
              WHERE c.root = ?1
              ORDER BY g.gem_root",
        )?;
        let rows = stmt.query_map(params![root], |r| r.get(0))?;
        rows.collect()
    }

    /// The app to answer a question about this gem's source from.
    ///
    /// **Most recently indexed wins.** Several apps can resolve one gem
    /// version, so the pick has to be deterministic; of the candidates — widest
    /// bundle, first registrant, most recent — only the last follows the work.
    /// Reindexing the app you are in makes it the context, which is the
    /// behaviour a person expects and the one that self-heals when the pick is
    /// wrong (DEC-029).
    pub(crate) fn app_for_gem(&self, gem_root: &str) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT c.root FROM gem_use g
                   JOIN checkout c ON c.id = g.checkout_id
                  WHERE g.gem_root = ?1
                  ORDER BY c.indexed_at DESC, c.root
                  LIMIT 1",
                params![gem_root],
                |r| r.get(0),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
    }

    /// The indexed checkout that contains this path, longest root first.
    ///
    /// A gem is a checkout but not a git repository, so `repo_root` cannot
    /// place a file inside one — and following a definition into gem code and
    /// then asking about a position there is exactly what an agent does next.
    /// The store already knows where every indexed root begins, so it answers.
    pub(crate) fn checkout_containing(&self, path: &str) -> Result<Option<String>> {
        self.conn
            .query_row(
                // Not LIKE: a root is a path, and `_` is a LIKE wildcard, so
                // `widget_shop` would also match `widgetXshop`. substr is an
                // exact prefix test, and the `/` keeps `/a/repo` from claiming
                // a file in `/a/repo2`.
                "SELECT root FROM checkout
                  WHERE substr(?1, 1, length(root) + 1) = root || '/'
                  ORDER BY LENGTH(root) DESC LIMIT 1",
                params![path],
                |r| r.get(0),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
    }

    /// Has this root been indexed before?
    ///
    /// For a gem this is the whole incremental story: a gem's bytes never
    /// change, so having seen it once is having seen it.
    /// Bring one file's facts up to date, and nothing else (DEC-035).
    ///
    /// The query-biased half of the refresh policy: when the probe says the
    /// checkout may have moved, the file being asked about is re-read and
    /// re-parsed, and the rest of the index is left alone and disclosed as
    /// possibly stale. Bounded by construction — one file, whatever the repo —
    /// which is what lets it sit on a query path that a 6-second scan cannot.
    ///
    /// Returns whether anything actually changed, or an error that
    /// [`is_busy`] recognizes when another process holds the write lock. It
    /// never waits for that lock: the transaction reads before it writes, and
    /// SQLite refuses a read transaction's upgrade at once rather than calling
    /// the busy handler. A caller answers from the committed index instead.
    /// Both keys are updated incrementally: they are order-independent folds of one XOR term per
    /// file, so removing the old term and adding the new one is exact rather
    /// than an approximation of a full re-fold.
    pub(crate) fn refresh_file(
        &mut self,
        root: &str,
        relative: &str,
        oid: &Oid,
        facts: Option<&Facts>,
    ) -> Result<bool> {
        // Deferred on purpose: meeting a writer, it fails at once rather than
        // waiting out the timeout, and the caller retries (DEC-066).
        let tx = self.conn.transaction()?;
        Store::check_schema(&tx)?;
        let Some((checkout_id, surface_key, map_key)) = tx
            .query_row(
                "SELECT id, surface_key, map_key FROM checkout WHERE root = ?1",
                params![root],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?
        else {
            return Ok(false);
        };

        let old: Option<(i64, String, i64)> = tx
            .query_row(
                "SELECT b.id, b.oid, b.surface FROM file f JOIN blob b ON b.id = f.blob_id
                  WHERE f.checkout_id = ?1 AND f.path = ?2",
                params![checkout_id, relative],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if old.as_ref().is_some_and(|(_, known, _)| known == &oid.0) {
            return Ok(false);
        }

        // `insert_facts` writes the blob row too, so a never-before-seen blob
        // becomes known here exactly as it would during a full index.
        if let Some(facts) = facts {
            insert_facts(&tx, oid, facts)?;
        }
        let Some((blob_id, surface)) = tx
            .query_row(
                "SELECT id, surface FROM blob WHERE oid = ?1",
                params![oid.0],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
            )
            .optional()?
        else {
            // Nothing to point at: the caller had no facts and this blob has
            // never been seen. Leave the map as it was rather than break it.
            return Ok(false);
        };

        tx.execute(
            "INSERT OR REPLACE INTO file (checkout_id, path, blob_id) VALUES (?1, ?2, ?3)",
            params![checkout_id, relative, blob_id],
        )?;

        let hashed = path_hash(relative);
        let (mut surface_key, mut map_key) = (surface_key, map_key);
        if let Some((_, known, old_surface)) = &old {
            surface_key = surface_key.wrapping_sub(hashed ^ old_surface);
            map_key = map_key.wrapping_sub(hashed ^ path_hash(known));
        }
        surface_key = surface_key.wrapping_add(hashed ^ surface);
        map_key = map_key.wrapping_add(hashed ^ path_hash(&oid.0));
        tx.execute(
            "UPDATE checkout SET surface_key = ?2, map_key = ?3 WHERE id = ?1",
            params![checkout_id, surface_key, map_key],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// git's view of this checkout when it was last indexed (DEC-035).
    ///
    /// `None` when the checkout is unknown, which the caller must not confuse
    /// with `Some(0)` — a gem, or a checkout indexed before this column
    /// existed, both of which are legitimately unprobeable.
    pub(crate) fn git_state(&self, root: &str) -> Result<Option<i64>> {
        self.conn
            .query_row(
                "SELECT git_state FROM checkout WHERE root = ?1",
                params![root],
                |r| r.get(0),
            )
            .optional()
    }

    pub(crate) fn has_checkout(&self, root: &str) -> Result<bool> {
        self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM checkout WHERE root = ?1)",
            params![root],
            |r| r.get::<_, i64>(0).map(|n| n != 0),
        )
    }

    /// Forget a checkout's file map. Its blobs stay: another worktree may
    /// share them, and re-reading bytes we have already parsed is the one cost
    /// this design exists to avoid.
    pub(crate) fn drop_checkout(&self, root: &str) -> Result<usize> {
        self.conn
            .execute("DELETE FROM checkout WHERE root = ?1", params![root])
    }
}

impl Store {
    /// Gather statistics in full, after an index has changed the shape of the
    /// database.
    ///
    /// `PRAGMA optimize` on close is the cheap version and it is not always
    /// enough: it re-analyses a table whose size has moved *since the last
    /// analysis*, which never fired across 633 checkouts accumulated a few at a
    /// time. The stale plan drove `workspaceSymbol` from the `checkout` table
    /// — every checkout, then every file — instead of the name index, and cost
    /// **1.28 s against 0.33 s**.
    ///
    /// Best effort, and only when the database has outgrown its statistics.
    /// A full `ANALYZE` reads every index whatever changed, so it cost the same
    /// after a one-file edit as after a cold index — 1.9 s of a 2.3 s reindex.
    /// Statistics steer the planner by orders of magnitude, so they are
    /// regathered once `blob` or `checkout` has grown a tenth past the count
    /// they were taken at (DEC-042). Measured against the last analysis rather
    /// than the last index, so many small indexes still add up to one.
    pub(crate) fn analyze_if_outgrown(&self) -> bool {
        if !self.outgrown_statistics().unwrap_or(true) {
            return false;
        }
        let _ = self.conn.execute_batch("ANALYZE;");
        true
    }

    fn outgrown_statistics(&self) -> Result<bool> {
        for table in ["blob", "checkout"] {
            // The first field of a `sqlite_stat1` row is the table's row count
            // when it was analysed. No row, or no table yet, means never.
            let analyzed: Option<String> = self
                .conn
                .query_row(
                    "SELECT stat FROM sqlite_stat1 WHERE tbl = ?1 LIMIT 1",
                    params![table],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(rows) = analyzed.and_then(|stat| stat.split(' ').next()?.parse::<i64>().ok())
            else {
                return Ok(true);
            };
            // `MAX(id)` rather than `COUNT(*)`: O(1), and ids only grow.
            let now: i64 = self.conn.query_row(
                &format!("SELECT COALESCE(MAX(id), 0) FROM {table}"),
                [],
                |r| r.get(0),
            )?;
            if now * 10 > rows * 11 {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

impl Drop for Store {
    /// `PRAGMA optimize` runs `ANALYZE` on tables whose size has moved enough
    /// to matter, and does nothing otherwise. Without statistics SQLite plans
    /// `refs` as a nested scan of the checkout's files — 90 s for a name as
    /// common as `new`, against 45 ms with them. Best effort: a failure here
    /// must not fail a command that already produced its answer.
    ///
    /// Never waits (DEC-066). Once a connection has planned with statistics
    /// for two tables, `optimize` takes the write lock even when it then has
    /// nothing to analyze, so under the 5 s timeout every read command sat
    /// out a running index at exit. `--index` keeps statistics current itself
    /// (DEC-042); this is the backstop, and skipping it while another process
    /// writes costs nothing that process's own analysis does not cover.
    fn drop(&mut self) {
        let _ = self.conn.busy_timeout(std::time::Duration::ZERO);
        let _ = self.conn.execute_batch("PRAGMA optimize;");
    }
}

/// How long a writer waits for another's lock before giving up.
const BUSY: std::time::Duration = std::time::Duration::from_secs(5);

/// Put the store in WAL mode, which it then stays in.
///
/// The switch takes an exclusive lock without consulting the busy handler, so
/// two processes creating the store at once failed one of them outright. It is
/// retried with jitter, so two waiters do not keep colliding in step.
fn wal(conn: &Connection) -> Result<()> {
    let deadline = std::time::Instant::now() + BUSY;
    loop {
        match conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get::<_, String>(0)) {
            Err(error) if is_busy(&error) && std::time::Instant::now() < deadline => {
                let jitter = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.subsec_nanos() % 20);
                std::thread::sleep(std::time::Duration::from_millis(5 + u64::from(jitter)));
            }
            other => return other.map(drop),
        }
    }
}

fn schema_version(conn: &Connection) -> Result<i64> {
    conn.pragma_query_value(None, "user_version", |r| r.get(0))
}

fn schema_mismatch(message: String) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_MISMATCH),
        Some(message),
    )
}

/// The store belongs to a different schema than this binary's.
pub(crate) fn is_schema_mismatch(error: &rusqlite::Error) -> bool {
    matches!(error, rusqlite::Error::SqliteFailure(e, _) if e.extended_code == rusqlite::ffi::SQLITE_MISMATCH)
}

/// An error without the SQL it came from: a failed `execute_batch` quotes the
/// whole batch, which for the schema is a screenful.
fn terse(error: rusqlite::Error) -> rusqlite::Error {
    match error {
        rusqlite::Error::SqlInputError { error, msg, .. } => {
            rusqlite::Error::SqliteFailure(error, Some(msg))
        }
        other => other,
    }
}

/// Another process holds the write lock — or committed under a read this
/// connection meant to upgrade, which is the same answer: not now.
pub(crate) fn is_busy(error: &rusqlite::Error) -> bool {
    matches!(error, rusqlite::Error::SqliteFailure(e, _) if e.code == rusqlite::ErrorCode::DatabaseBusy)
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct Checkout {
    pub(crate) repo: String,
    pub(crate) indexed_at: i64,
    pub(crate) files: i64,
    pub(crate) blobs: i64,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct Totals {
    pub(crate) blobs: i64,
    pub(crate) defs: i64,
    pub(crate) const_refs: i64,
    pub(crate) calls: i64,
}

/// A class, module, or constant declaration, as the blob layer recorded it —
/// name as written, nesting unresolved.
#[derive(Debug)]
pub(crate) struct DeclRow {
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) nesting: Vec<String>,
    pub(crate) target: Option<String>,
    pub(crate) path: String,
    pub(crate) line: u32,
    pub(crate) col: u32,
}

#[derive(Debug)]
pub(crate) struct MethodRow {
    pub(crate) name: String,
    pub(crate) nesting: Vec<String>,
    pub(crate) singleton: bool,
    pub(crate) visibility: String,
    pub(crate) params: Vec<Param>,
    pub(crate) via: Option<String>,
    pub(crate) target: Option<String>,
    pub(crate) sig_returns: Option<String>,
    /// Never read from SQL: only core has them (`Def::sig_overloads`).
    pub(crate) sig_overloads: Vec<crate::core::Overload>,
    pub(crate) path: String,
    pub(crate) line: u32,
    pub(crate) col: u32,
    /// An alias's body, when it was written above it in the same file.
    pub(crate) target_pos: Option<crate::core::Pos>,
}

#[derive(Debug)]
pub(crate) struct EdgeRow {
    /// Scope stack including the receiving class or module, innermost first.
    pub(crate) owner: Vec<String>,
    pub(crate) relation: String,
    pub(crate) target: String,
    /// The file that wrote it, absolute like a site's path: which of two
    /// conflicting declarations of the owner it belongs to (DEC-072).
    pub(crate) path: String,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct Ref {
    pub(crate) path: String,
    pub(crate) line: u32,
    pub(crate) col: u32,
    /// definition | constant | call
    pub(crate) role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) kind: Option<String>,
    /// `receiver`/`receiver_text` on the wire, the names every other answer
    /// gives the same facts (DEC-080).
    #[serde(rename = "receiver", skip_serializing_if = "Option::is_none")]
    pub(crate) recv: Option<String>,
    #[serde(rename = "receiver_text", skip_serializing_if = "Option::is_none")]
    pub(crate) recv_text: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) nesting: Vec<String>,
    /// Filled in for call rows once the ladder has run: `confirmed` when the
    /// receiver's type resolves, `possible` when it does not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tier: Option<String>,
    /// Where Ruby's lookup from that receiver lands.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) owner: Option<String>,
}

/// A freshly parsed definition, in the shape the store returns.
///
/// `path`/`root` stay empty: a caller holding the `Def` already knows the file
/// it read, which is exactly the case the stored rows fill those in for.
impl From<&Def> for Symbol {
    fn from(def: &Def) -> Symbol {
        Symbol {
            path: String::new(),
            root: String::new(),
            name: def.name.clone(),
            kind: def.kind.as_str().to_string(),
            nesting: def.nesting.clone(),
            singleton: def.singleton,
            visibility: def.visibility.as_str().to_string(),
            params: def
                .params
                .iter()
                .map(|p| format!("{}:{}", p.kind.as_str(), p.name))
                .collect(),
            via: def.via.clone(),
            target: def.target.clone(),
            sig_returns: def.sig_returns.clone(),
            line: def.pos.line,
            col: def.pos.col,
            end_line: def.end_line,
        }
    }
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct Symbol {
    /// Empty for a freshly parsed definition until the caller names its file.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) path: String,
    /// The checkout `path` is relative to. An internal join key — a
    /// cross-checkout answer needs it to turn `path` back into a real file —
    /// not a fact any command reports.
    #[serde(skip)]
    pub(crate) root: String,
    pub(crate) name: String,
    pub(crate) kind: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) nesting: Vec<String>,
    pub(crate) singleton: bool,
    pub(crate) visibility: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) params: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) via: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) sig_returns: Option<String>,
    pub(crate) line: u32,
    pub(crate) col: u32,
    pub(crate) end_line: u32,
}

/// `?,?,?` for an `IN` clause. Zero roots would be a syntax error, so it
/// degenerates to a literal that matches nothing.
fn placeholders(count: usize) -> String {
    if count == 0 {
        return "NULL".to_string();
    }
    std::iter::repeat_n("?", count)
        .collect::<Vec<_>>()
        .join(",")
}

/// Parameters round-trip through one column as `kind:name` pairs joined by
/// `;`, using Ruby's own `Method#parameters` vocabulary so the encoding needs
/// no glossary of ours.
pub(crate) fn encode_params(params: &[Param]) -> String {
    params
        .iter()
        .map(|p| format!("{}:{}", p.kind.as_str(), p.name))
        .collect::<Vec<_>>()
        .join(";")
}

pub(crate) fn decode_params(s: &str) -> Vec<Param> {
    if s.is_empty() {
        return Vec::new();
    }
    s.split(';')
        .filter_map(|part| {
            let (kind, name) = part.split_once(':')?;
            Some(Param {
                kind: ParamKind::parse(kind)?,
                name: name.to_string(),
            })
        })
        .collect()
}

/// A path's contribution to a checkout's surface key. FNV-1a again — the same
/// reasoning as `Facts::surface`, and the two are mixed with XOR so a file's
/// identity and its contents both have to match.
/// The file map folded into one number: order-independent, and moved by any
/// path or blob changing.
fn map_key(files: &Files) -> i64 {
    files.iter().fold(0i64, |key, (path, oid)| {
        key.wrapping_add(path_hash(path) ^ path_hash(&oid.0))
    })
}

fn path_hash(path: &str) -> i64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash as i64
}

fn insert_facts(tx: &Connection, oid: &Oid, facts: &Facts) -> Result<()> {
    // Another process may have recorded these bytes since this one decided
    // they were new. Their facts are a pure function of the bytes, so the row
    // already there is the answer; replacing it would give the blob a new id
    // under every file that points at the old one.
    let inserted = tx.execute(
        "INSERT INTO blob (oid, lines, parse_errors, surface)
         VALUES (?1, ?2, ?3, ?4) ON CONFLICT (oid) DO NOTHING",
        params![
            oid.0,
            facts.lines as i64,
            facts.parse_errors as i64,
            facts.surface() as i64
        ],
    )?;
    if inserted == 0 {
        return Ok(());
    }
    let blob_id = tx.last_insert_rowid();

    let mut def = tx.prepare_cached(
        "INSERT INTO def (blob_id, name, kind, nesting, singleton, visibility, params,
                          via, target, sig_returns, line, col, end_line,
                          target_line, target_col)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
    )?;
    for d in &facts.defs {
        def.execute(params![
            blob_id,
            d.name,
            d.kind.as_str(),
            join_nesting(&d.nesting),
            d.singleton as i64,
            d.visibility.as_str(),
            encode_params(&d.params),
            d.via,
            d.target,
            d.sig_returns,
            d.pos.line,
            d.pos.col,
            d.end_line,
            d.target_pos.map(|at| at.line),
            d.target_pos.map(|at| at.col),
        ])?;
    }

    let mut ancestry = tx.prepare_cached(
        "INSERT INTO ancestry (blob_id, owner, relation, target, line, col)
         VALUES (?1,?2,?3,?4,?5,?6)",
    )?;
    for a in &facts.ancestry {
        ancestry.execute(params![
            blob_id,
            join_nesting(&a.owner),
            a.relation.as_str(),
            a.target,
            a.pos.line,
            a.pos.col,
        ])?;
    }

    let mut const_ref = tx.prepare_cached(
        "INSERT INTO const_ref (blob_id, name, nesting, line, col) VALUES (?1,?2,?3,?4,?5)",
    )?;
    for r in &facts.const_refs {
        const_ref.execute(params![
            blob_id,
            r.name,
            join_nesting(&r.nesting),
            r.pos.line,
            r.pos.col,
        ])?;
    }

    let mut call = tx.prepare_cached(
        "INSERT INTO call_site
             (blob_id, name, recv, recv_text, nesting, singleton, argc, block, line, col)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
    )?;
    for c in &facts.calls {
        call.execute(params![
            blob_id,
            c.name,
            c.recv.as_str(),
            c.recv_text,
            join_nesting(&c.nesting),
            c.singleton as i64,
            c.argc,
            c.block as i64,
            c.pos.line,
            c.pos.col,
        ])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn indexed(store: &mut Store, root: &str, path: &str, src: &str) -> Indexed {
        let oid = crate::scan::hash_blob(src.as_bytes());
        let files = Files::from([(path.to_string(), oid.clone())]);
        let facts = if store.has_blob(&oid).unwrap() {
            Vec::new()
        } else {
            vec![(oid, crate::extract::extract(src.as_bytes()))]
        };
        store.write(root, &files, facts, 0).unwrap()
    }

    #[test]
    fn params_round_trip_through_one_column() {
        let params = vec![
            Param {
                kind: ParamKind::Req,
                name: "a".into(),
            },
            Param {
                kind: ParamKind::Keyrest,
                name: "opts".into(),
            },
        ];
        assert_eq!(decode_params(&encode_params(&params)), params);
        assert!(decode_params("").is_empty());
    }

    #[test]
    fn files_calling_pages_through_every_call_in_the_checkout_only() {
        let mut store = Store::open_in_memory().unwrap();
        let sources = [
            ("a.rb", "x.go\nx.go\n"),
            ("b.rb", "y.go\n"),
            ("c.rb", "z.stop\n"),
        ];
        let files: Files = sources
            .iter()
            .map(|(path, src)| (path.to_string(), crate::scan::hash_blob(src.as_bytes())))
            .collect();
        let facts: Vec<_> = sources
            .iter()
            .map(|(_, src)| {
                (
                    crate::scan::hash_blob(src.as_bytes()),
                    crate::extract::extract(src.as_bytes()),
                )
            })
            .collect();
        store.write("/a", &files, facts, 0).unwrap();
        indexed(&mut store, "/other", "d.rb", "w.go\n");

        let mut seen = Vec::new();
        let mut after = 0;
        loop {
            let (last, page) = store.files_calling_page("/a", "go", after, 2).unwrap();
            if page.is_empty() {
                break;
            }
            assert!(page.len() <= 2);
            seen.extend(page);
            after = last;
        }
        seen.sort();
        assert_eq!(
            seen,
            ["a.rb", "a.rb", "b.rb"],
            "one row per call, this checkout's"
        );

        // Driven from the name, in row order, so nothing past the page is
        // read — even when the statistics say the name is everywhere, which
        // is when the planner once chose to read every file instead.
        let common = "x.go\n".repeat(2000);
        indexed(&mut store, "/big", "big.rb", &common);
        store.conn.execute_batch("ANALYZE;").unwrap();
        let plan: Vec<String> = store
            .conn
            .prepare(&format!("EXPLAIN QUERY PLAN {FILES_CALLING_PAGE}"))
            .unwrap()
            .query_map(params!["/a", "go", 0, 2], |r| r.get(3))
            .unwrap()
            .collect::<Result<_>>()
            .unwrap();
        assert!(plan[0].contains("call_site_name"), "{plan:?}");
        assert!(
            !plan.iter().any(|step| step.contains("TEMP B-TREE")),
            "{plan:?}"
        );
    }

    /// `files_calling` is driven from the name, whatever the statistics say
    /// about how common it is, and sorts nothing in SQLite.
    #[test]
    fn files_calling_is_driven_from_the_name() {
        let mut store = Store::open_in_memory().unwrap();
        indexed(&mut store, "/a", "a.rb", "x.go\ny.stop\n");
        indexed(&mut store, "/big", "big.rb", &"x.go\n".repeat(2000));
        store.conn.execute_batch("ANALYZE;").unwrap();
        assert_eq!(store.files_calling("/a", "go").unwrap(), ["a.rb"]);
        let plan: Vec<String> = store
            .conn
            .prepare(&format!("EXPLAIN QUERY PLAN {FILES_CALLING}"))
            .unwrap()
            .query_map(params!["/a", "go"], |r| r.get(3))
            .unwrap()
            .collect::<Result<_>>()
            .unwrap();
        assert!(
            plan.iter()
                .any(|step| step.contains("SEARCH s USING INDEX call_site_name")),
            "{plan:?}"
        );
        assert!(
            !plan.iter().any(|step| step.contains("TEMP B-TREE")),
            "{plan:?}"
        );
    }

    #[test]
    fn reindexing_unchanged_bytes_parses_nothing() {
        let mut store = Store::open_in_memory().unwrap();
        let src = "class Widget\n  def go\n  end\nend\n";
        assert_eq!(indexed(&mut store, "/a", "w.rb", src).parsed, 1);
        assert_eq!(
            indexed(&mut store, "/a", "w.rb", src).parsed,
            0,
            "the same bytes are never parsed twice — that is the whole design"
        );
    }

    #[test]
    fn statistics_are_regathered_only_once_the_store_outgrows_them() {
        let mut store = Store::open_in_memory().unwrap();
        let mut blobs = 0;
        let mut add = |store: &mut Store, n: usize| {
            for _ in 0..n {
                blobs += 1;
                indexed(store, "/a", "w.rb", &format!("class W{blobs}\nend\n"));
            }
        };
        add(&mut store, 10);
        assert!(store.analyze_if_outgrown(), "never analysed");
        add(&mut store, 1);
        assert!(
            !store.analyze_if_outgrown(),
            "a tenth more is within bounds"
        );
        add(&mut store, 1);
        assert!(
            store.analyze_if_outgrown(),
            "past a tenth, measured from the last analysis"
        );
        assert!(!store.analyze_if_outgrown());
    }

    #[test]
    fn written_calls_skip_symbols_and_stop_at_the_cap() {
        let mut store = Store::open_in_memory().unwrap();
        let src = "class W\n  before_save :go\n  def a\n    go\n    go\n    go\n  end\nend\n";
        indexed(&mut store, "/a", "w.rb", src);
        let names = ["go", "go", "absent"].map(String::from);
        let counts = store.written_calls("/a", &names, 2).unwrap();
        assert_eq!(counts["go"], 2, "three written calls, capped");
        assert_eq!(counts["absent"], 0);
        assert_eq!(
            store.written_calls("/a", &names, 10).unwrap()["go"],
            3,
            "the symbol is not a call"
        );
    }

    #[test]
    fn written_calls_count_only_the_checkout_asking() {
        let mut store = Store::open_in_memory().unwrap();
        indexed(&mut store, "/a", "a.rb", "x.go\n");
        indexed(&mut store, "/other", "b.rb", &"y.go\n".repeat(5));
        let names = ["go".to_string()];
        assert_eq!(store.written_calls("/a", &names, 10).unwrap()["go"], 1);
    }

    #[test]
    fn a_failed_batch_keeps_none_of_its_writes() {
        let mut store = Store::open_in_memory().unwrap();
        indexed(&mut store, "/app", "a.rb", "class A\nend\n");
        let failed: anyhow::Result<()> = store.batch(|store| {
            indexed(store, "/gem1", "g.rb", "class G\nend\n");
            anyhow::bail!("interrupted")
        });
        assert!(failed.is_err());
        assert!(!store.has_checkout("/gem1").unwrap(), "rolled back whole");
        assert!(store.has_checkout("/app").unwrap(), "earlier commits stand");

        store
            .batch(|store| {
                indexed(store, "/gem1", "g.rb", "class G\nend\n");
                indexed(store, "/gem2", "h.rb", "class H\nend\n");
                Ok::<(), rusqlite::Error>(())
            })
            .unwrap();
        assert!(store.has_checkout("/gem1").unwrap() && store.has_checkout("/gem2").unwrap());
    }

    fn index_names(store: &Store) -> Vec<String> {
        let mut stmt = store
            .conn
            .prepare("SELECT name FROM sqlite_schema WHERE type = 'index' AND sql IS NOT NULL ORDER BY name")
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_>>()
            .unwrap()
    }

    fn one_file(src: &str) -> (Files, Vec<(Oid, Facts)>) {
        let oid = crate::scan::hash_blob(src.as_bytes());
        let files = Files::from([("a.rb".to_string(), oid.clone())]);
        (files, vec![(oid, crate::extract::extract(src.as_bytes()))])
    }

    /// A bulk load leaves the same rows and the same indexes a plain write
    /// does — it only gets there by sorting.
    #[test]
    fn a_bulk_write_leaves_what_a_plain_one_does() {
        let src = "class Widget\n  def save; helper(1); Other::X; end\nend\n";
        let mut plain = Store::open_in_memory().unwrap();
        let mut bulk = Store::open_in_memory().unwrap();
        let (files, facts) = one_file(src);
        plain.write("/r", &files, facts, 0).unwrap();
        let (files, facts) = one_file(src);
        bulk.write_bulk("/r", &files, facts, 0).unwrap();
        assert_eq!(index_names(&plain), index_names(&bulk));
        let t = |s: &Store| s.totals().unwrap();
        assert_eq!(
            (t(&plain).defs, t(&plain).calls, t(&plain).const_refs),
            (t(&bulk).defs, t(&bulk).calls, t(&bulk).const_refs)
        );
    }

    /// A load interrupted after its indexes were dropped rolls the drop back
    /// with its rows: the store is exactly as it was.
    #[test]
    fn an_interrupted_bulk_write_keeps_its_indexes() {
        let mut store = Store::open_in_memory().unwrap();
        indexed(&mut store, "/r", "a.rb", "class Widget\nend\n");
        let before = (index_names(&store), store.totals().unwrap().defs);
        let src = "class Gadget\n  def go; end\nend\n";
        let oid = crate::scan::hash_blob(src.as_bytes());
        let files = Files::from([("b.rb".to_string(), oid.clone())]);
        let facts = (0..2).map(|i| {
            assert!(i == 0, "interrupted mid-load");
            (oid.clone(), crate::extract::extract(src.as_bytes()))
        });
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            store.write_bulk("/r", &files, facts, 0)
        }));
        assert!(outcome.is_err());
        assert_eq!((index_names(&store), store.totals().unwrap().defs), before);
        assert!(index_names(&store).contains(&"call_site_name".to_string()));
    }

    #[test]
    fn the_bulk_index_list_is_the_schema_s() {
        let squash = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let schema = squash(schema::SCHEMA);
        for (name, create) in schema::BULK_INDEXES {
            assert!(
                schema.contains(&squash(create)),
                "{name} drifted from SCHEMA"
            );
        }
    }

    /// Written as a delta, a map ends up exactly as a first write of the same
    /// files would leave it: a vanished path gone, an edit and a rename
    /// pointing at their new blobs, and the surface key agreeing.
    #[test]
    fn a_map_updated_in_place_matches_one_written_fresh() {
        fn write(store: &mut Store, root: &str, files: &[(&str, &str)]) {
            let mut map = Files::new();
            let mut facts = Vec::new();
            for (path, src) in files {
                let oid = crate::scan::hash_blob(src.as_bytes());
                map.insert(path.to_string(), oid.clone());
                if !store.has_blob(&oid).unwrap() {
                    facts.push((oid, crate::extract::extract(src.as_bytes())));
                }
            }
            store.write(root, &map, facts, 0).unwrap();
        }
        fn map(store: &Store, root: &str) -> Vec<(String, String)> {
            let mut stmt = store
                .conn
                .prepare(
                    "SELECT f.path, b.oid FROM file f JOIN blob b ON b.id = f.blob_id
                       JOIN checkout c ON c.id = f.checkout_id WHERE c.root = ?1 ORDER BY f.path",
                )
                .unwrap();
            stmt.query_map(params![root], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .collect::<Result<_>>()
                .unwrap()
        }
        let mut store = Store::open_in_memory().unwrap();
        let (a, b, c) = ("class A\nend\n", "class B\nend\n", "class C\nend\n");
        write(
            &mut store,
            "/moved",
            &[("a.rb", a), ("b.rb", b), ("keep.rb", c)],
        );
        // b.rb deleted, a.rb edited, keep.rb renamed to kept.rb.
        let after = [("a.rb", b), ("kept.rb", c), ("new.rb", a)];
        write(&mut store, "/moved", &after);
        write(&mut store, "/fresh", &after);

        assert_eq!(map(&store, "/moved"), map(&store, "/fresh"));
        assert_eq!(map(&store, "/moved").len(), 3);
        assert_eq!(
            store.surface_key("/moved").unwrap(),
            store.surface_key("/fresh").unwrap()
        );
    }

    #[test]
    fn a_second_checkout_of_the_same_content_reuses_the_facts() {
        let mut store = Store::open_in_memory().unwrap();
        let src = "class Widget\nend\n";
        indexed(&mut store, "/a", "w.rb", src);
        let second = indexed(&mut store, "/b", "w.rb", src);
        assert_eq!(second.parsed, 0, "a new worktree is a map, not a parse");
        assert_eq!(store.totals().unwrap().blobs, 1);
        assert_eq!(store.status().unwrap().len(), 2);
    }

    #[test]
    fn dropping_a_checkout_keeps_blobs_another_one_may_share() {
        let mut store = Store::open_in_memory().unwrap();
        let src = "class Widget\nend\n";
        indexed(&mut store, "/a", "w.rb", src);
        indexed(&mut store, "/b", "w.rb", src);
        store.drop_checkout("/a").unwrap();
        assert_eq!(store.status().unwrap().len(), 1);
        assert_eq!(store.totals().unwrap().blobs, 1);
    }

    #[test]
    fn refs_report_every_mention_and_what_sort_it_is() {
        let mut store = Store::open_in_memory().unwrap();
        indexed(
            &mut store,
            "/a",
            "w.rb",
            "class Widget\n  def save\n  end\n  def go\n    save\n    other.save\n  end\nend\n",
        );
        let refs = store.refs("/a", "save").unwrap();
        let seen: Vec<_> = refs
            .iter()
            .map(|r| (r.role.as_str(), r.recv.as_deref()))
            .collect();
        assert_eq!(
            seen,
            [
                ("definition", None),
                ("call", Some("implicit")),
                ("call", Some("other")),
            ],
            "a name-level answer discloses the receiver rather than guessing"
        );
        assert!(store.refs("/a", "absent").unwrap().is_empty());
    }

    /// A tree spans a repo *and* every gem it resolves, so a site's path has
    /// to say which checkout it came from. It did not, and both fronts joined
    /// a gem's relative path onto the repo being asked about — naming files
    /// that do not exist, which an agent then tried to read.
    #[test]
    fn a_site_from_another_checkout_keeps_its_own_root() {
        let mut store = Store::open_in_memory().unwrap();
        indexed(&mut store, "/app", "lib/job.rb", "class Job\nend\n");
        indexed(&mut store, "/gem", "lib/helper.rb", "class Helper\nend\n");

        let roots = vec!["/gem".to_string(), "/app".to_string()];
        let paths: Vec<String> = store
            .declarations(&roots)
            .unwrap()
            .into_iter()
            .map(|d| d.path)
            .collect();
        assert!(
            paths.contains(&"/gem/lib/helper.rb".to_string())
                && paths.contains(&"/app/lib/job.rb".to_string()),
            "each site is absolute and rooted where it really lives: {paths:?}"
        );
        assert!(
            store
                .methods(&roots)
                .unwrap()
                .iter()
                .all(|m| m.path.starts_with('/')),
            "and method sites too, which is what a call resolves to"
        );
    }

    /// An outline is now parsed rather than queried, so source order is the
    /// extractor's guarantee to keep, not SQL's `ORDER BY`.
    #[test]
    fn an_outline_follows_the_source_and_carries_what_the_rows_did() {
        let facts =
            crate::extract::extract(b"class Widget\n  def b\n  end\n  attr_reader :a\nend\n");
        let symbols: Vec<Symbol> = facts.defs.iter().map(Into::into).collect();
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Widget", "b", "a"]);

        let generated = symbols.last().expect("attr_reader's method");
        assert_eq!(generated.via.as_deref(), Some("attr_reader"));
        assert_eq!(generated.nesting, ["Widget"]);
        assert!(
            generated.path.is_empty(),
            "a parsed row leaves the path to the caller that read the file"
        );
    }
}

#[cfg(test)]
mod checkout_containing_tests {
    use super::*;

    /// A checkout root is a path, not a pattern, and not a bare prefix.
    #[test]
    fn a_root_claims_only_files_genuinely_inside_it() {
        let mut store = Store::open_in_memory().unwrap();
        for root in ["/code/widget_shop", "/code/widget_shop-nosorbet"] {
            super::tests::indexed(&mut store, root, "app.rb", "class A\nend\n");
        }
        let of = |p: &str| store.checkout_containing(p).unwrap();

        assert_eq!(
            of("/code/widget_shop/app/models/widget.rb").as_deref(),
            Some("/code/widget_shop")
        );
        // The longest genuine container wins, not the shortest prefix match.
        assert_eq!(
            of("/code/widget_shop-nosorbet/app/models/widget.rb").as_deref(),
            Some("/code/widget_shop-nosorbet")
        );
        // `_` is a LIKE wildcard; a path is not a pattern.
        assert_eq!(of("/code/widgetXshop/app.rb"), None);
        assert_eq!(of("/elsewhere/app.rb"), None);
    }
}

/// A query path meets another process writing: it answers without waiting.
#[cfg(test)]
mod lock_tests {
    use super::*;

    /// A store on disk with one indexed file, and a second connection holding
    /// the write lock the way a running `--index` does.
    fn locked(label: &str) -> (Store, Connection, std::path::PathBuf) {
        let path =
            std::env::temp_dir().join(format!("trekr-lock-{label}-{}.db", std::process::id()));
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
        let mut store = Store::open(&path).unwrap();
        super::tests::indexed(&mut store, "/app", "a.rb", "class A\nend\n");
        let writer = Connection::open(&path).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        (store, writer, path)
    }

    #[test]
    fn a_refresh_meeting_a_writer_says_busy_at_once_and_changes_nothing() {
        let (mut store, writer, path) = locked("refresh");
        let src = b"class A\n  def moved\n  end\nend\n";
        let oid = crate::scan::hash_blob(src);
        let facts = crate::extract::extract(src);

        let started = std::time::Instant::now();
        let error = store
            .refresh_file("/app", "a.rb", &oid, Some(&facts))
            .expect_err("the lock is held");
        assert!(is_busy(&error), "{error}");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "must not sit out the busy timeout"
        );
        assert!(!store.has_blob(&oid).unwrap(), "nothing half-written");

        writer.execute_batch("ROLLBACK").unwrap();
        assert!(
            store
                .refresh_file("/app", "a.rb", &oid, Some(&facts))
                .unwrap()
        );
        drop(store);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn closing_a_store_does_not_wait_for_a_writer() {
        let (store, writer, path) = locked("close");
        // What a query does: plan with statistics over more than one table,
        // which is what makes `optimize` reach for the write lock.
        store.status().unwrap();
        store.has_checkout("/app").unwrap();
        let started = std::time::Instant::now();
        drop(store);
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        drop(writer);
        let _ = std::fs::remove_file(&path);
    }

    fn scratch(label: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("trekr-schema-{label}-{}.db", std::process::id()));
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
        path
    }

    #[test]
    fn a_write_refuses_a_store_another_binary_rebuilt() {
        let path = scratch("rebuilt");
        let mut store = Store::open(&path).unwrap();
        super::tests::indexed(&mut store, "/app", "a.rb", "class A\nend\n");
        Connection::open(&path)
            .unwrap()
            .pragma_update(None, "user_version", schema::VERSION + 1)
            .unwrap();

        let src = b"class A\n  def moved\n  end\nend\n";
        let oid = crate::scan::hash_blob(src);
        let facts = crate::extract::extract(src);
        let error = store
            .refresh_file("/app", "a.rb", &oid, Some(&facts))
            .expect_err("the schema moved");
        assert!(is_schema_mismatch(&error), "{error}");
        let files = Files::from([("a.rb".to_string(), oid.clone())]);
        let error = store
            .write("/app", &files, [(oid.clone(), facts)], 0)
            .expect_err("the schema moved");
        assert!(is_schema_mismatch(&error), "{error}");
        assert!(!store.has_blob(&oid).unwrap(), "nothing written");
        drop(store);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_blob_another_writer_recorded_first_keeps_its_row() {
        let path = scratch("shared-blob");
        let (mut first, mut second) = (Store::open(&path).unwrap(), Store::open(&path).unwrap());
        let src = b"class A\n  def one\n  end\nend\n";
        let oid = crate::scan::hash_blob(src);
        let files = Files::from([("a.rb".to_string(), oid.clone())]);
        // Both decided the blob was new before either wrote it.
        let facts = crate::extract::extract(src);
        first
            .write("/one", &files, [(oid.clone(), facts.clone())], 0)
            .unwrap();
        second
            .write("/two", &files, [(oid.clone(), facts)], 0)
            .unwrap();

        let count = |sql: &str| -> i64 { first.conn.query_row(sql, [], |r| r.get(0)).unwrap() };
        assert_eq!(count("SELECT COUNT(*) FROM blob"), 1);
        assert_eq!(
            count("SELECT COUNT(*) FROM def"),
            2,
            "class and method, once"
        );
        assert_eq!(
            count("SELECT COUNT(*) FROM file WHERE blob_id NOT IN (SELECT id FROM blob)"),
            0
        );
        drop((first, second));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn opening_an_old_store_rebuilds_it_and_remembers_why() {
        let path = scratch("old");
        let old = Connection::open(&path).unwrap();
        old.execute_batch("CREATE TABLE checkout (x); PRAGMA user_version = 3;")
            .unwrap();
        drop(old);
        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), schema::VERSION);
        assert_eq!(store.upgraded_from().unwrap(), Some(3));
        assert!(store.status().unwrap().is_empty());
        drop(store);
        let fresh = scratch("fresh");
        assert_eq!(Store::open(&fresh).unwrap().upgraded_from().unwrap(), None);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&fresh);
    }

    #[test]
    fn a_failed_batch_is_reported_without_its_sql() {
        let conn = Connection::open_in_memory().unwrap();
        let error = conn
            .execute_batch("CREATE TABLE t (x); CREATE TABLE t (x);")
            .unwrap_err();
        let error = terse(error).to_string();
        assert!(error.contains("already exists"), "{error}");
        assert!(!error.contains("CREATE"), "{error}");
    }
}
