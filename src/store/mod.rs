//! SQLite, WAL, and nothing clever.
//!
//! Facts are keyed by blob OID, so two worktrees of one repo store one copy and
//! a branch switch reparses only what is genuinely new. The store's job is to
//! make that diff cheap and to stay out of the way otherwise.
//!
//! Conventions (pragmas, `user_version` as the migration marker, `$TREKR_DB`)
//! follow rq's `src/store/`.

pub(crate) mod early;
mod gc;
#[cfg(test)]
mod golden;
mod overlay;
mod recover;
mod schema;
mod warming;

pub(crate) use overlay::Overlays;
pub(crate) use recover::{Kept, in_use, kept, remove_kept};
pub(crate) use schema::VERSION;
pub(crate) use warming::Warming;

use crate::core::*;
use crate::scan::Files;
use rusqlite::{Connection, OptionalExtension, Result, params};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

pub(crate) struct Store {
    conn: Connection,
    /// Where this store lives, so a second connection to it can be opened.
    /// `None` for an in-memory store, which cannot be reached twice.
    path: Option<std::path::PathBuf>,
    /// The file `path` named when this store opened it, to notice it moved.
    file: Option<(u64, u64)>,
    /// Opened only as it was found (`open_existing`), and so reopened.
    existing: bool,
    /// Where the writes since the last `take_timing` spent their time, for
    /// `--index --profile`.
    timing: WriteTiming,
    /// Each checkout `overlay` answers for on this connection: how far that
    /// moves its keys, and the files, for a `reopen` to answer the same.
    overlaid: overlay::Overlays,
}

/// What a query reads in place of a checkout's map (`Store::overlay`): each
/// path at its blob, or absent for `None`.
pub(crate) type Overlay = Vec<(String, Option<Oid>)>;

/// How a write lays a checkout's map down.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// The whole map, replacing what was stored.
    Whole,
    /// The whole map, its fact indexes rebuilt by sorting (DEC-057).
    Bulk,
    /// Some of the map, added to what is stored (DEC-322).
    Part,
}

/// The parts of a write that happen after its rows are in, timed apart so the
/// profile's phases still sum to the whole.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct WriteTiming {
    /// Rebuilding the fact indexes after a bulk load (DEC-057).
    pub(crate) rebuild: std::time::Duration,
    /// Diffing and writing the file map (DEC-048).
    pub(crate) map: std::time::Duration,
    /// `COMMIT`, which is where the WAL is written and checkpointed.
    pub(crate) commit: std::time::Duration,
}

/// The checkouts a tree is assembled from — the Ruby's stdlib, the bundle's
/// gems, the checkout itself, which is always last — and the stdlib files
/// this app does not see (DEC-180).
#[derive(Clone, Debug, Default)]
pub(crate) struct Roots {
    pub(crate) list: Vec<String>,
    /// Absolute paths of stdlib files a default gem owns that the app bundles
    /// its own copy of. Its copy answers; the stdlib's would be a second one.
    pub(crate) hidden: HashSet<String>,
    /// The Ruby's stdlib among `list`, when the app runs on one it indexed.
    pub(crate) stdlib: Option<String>,
}

impl Roots {
    /// Just these roots, hiding nothing.
    #[cfg(test)]
    pub(crate) fn of(list: Vec<String>) -> Roots {
        Roots {
            list,
            ..Roots::default()
        }
    }

    fn shows(&self, path: &str) -> bool {
        !self.hidden.contains(path)
    }

    /// Where a checkout's rows sit in the layering: its Ruby's stdlib, then
    /// the gems, then the checkout itself — Ruby's load order, so each layer
    /// reopens the ones before it whatever order they were indexed in.
    fn layer(&self, kind: &str, root: &str) -> u8 {
        if kind == "stdlib" {
            0
        } else if self.list.last().is_some_and(|app| app == root) {
            2
        } else {
            1
        }
    }
}

/// [`Roots::layer`] as SQL, then insert order within a layer. `app` is the
/// placeholder bound to the checkout's own root, the last of `Roots::list`.
fn layered(app: usize) -> String {
    format!("CASE WHEN c.kind = 'stdlib' THEN 0 WHEN c.root = ?{app} THEN 2 ELSE 1 END, c.id")
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
/// per owner, `trekr.core/rbs-3.8.0-…/String.rb` — and the directory they
/// are in.
///
/// The stubs live in the store, so a definition in one had no location to
/// point at and every `require` or `Array#each` answered nothing — worse than
/// ruby-lsp, which at least sends you to an RBS declaration. Writing them out
/// means "go to definition" lands on a signature a person can read, in a file
/// whose name says whose it is. Each Ruby's go in a directory of their own
/// (DEC-240), in a directory of the store's own, as its tree snapshots are:
/// which of them are stale is that store's to say (DEC-274).
pub(crate) fn core_dir() -> anyhow::Result<std::path::PathBuf> {
    let dir = core_dir_of(&recover::in_use(&default_path()?));
    crate::tree::materialize_core(&dir)?;
    Ok(dir)
}

/// `trekr.db` keeps its core files in `trekr.core/`.
pub(crate) fn core_dir_of(db: &Path) -> std::path::PathBuf {
    db.with_extension("core")
}

/// After an upgrade drops every signature row, each Ruby's directory of
/// core files is stale, and so is whatever a build before a directory per
/// store wrote beside it.
fn sweep_core_after_upgrade(db: &Path, store: &Store) {
    let Ok(live) = store.core_dir_names() else {
        return;
    };
    crate::tree::sweep_core(&core_dir_of(db), &live, false);
    if let Some(beside) = db.parent() {
        crate::tree::sweep_legacy_core(beside, false);
    }
}

/// The database every command uses.
pub(crate) fn open_default() -> anyhow::Result<Store> {
    let path = default_path()?;
    let opened = match path.parent() {
        Some(parent) => std::fs::create_dir_all(parent).map_err(anyhow::Error::from),
        None => Ok(()),
    }
    .and_then(|()| Store::open(&path).map_err(anyhow::Error::from));
    // Said with its reason once, and tagged here: the chain it summarizes is
    // not kept, or `{:#}` would say the reason twice again.
    opened.map_err(|error| {
        crate::failure::Failure::Database.error(format!(
            "trekr store {}: {}",
            path.display(),
            said_once(&error, &path)
        ))
    })
}

/// An error's chain, each part said once. SQLite's open error names the path
/// again, and rusqlite gives each failure its own code's text as its source:
/// `unable to open database file: P: Error code 14: unable to open database
/// file`.
fn said_once(error: &anyhow::Error, path: &Path) -> String {
    let spelled = format!(": {}", path.display());
    let mut said = String::new();
    for part in error.chain() {
        let text = part.to_string().replace(&spelled, "");
        let bare = match text.strip_prefix("Error code ") {
            Some(rest) => rest.split_once(": ").map_or(rest, |(_, text)| text),
            None => &text,
        };
        if said.contains(bare) {
            continue;
        }
        if !said.is_empty() {
            said.push_str(": ");
        }
        said.push_str(bare);
    }
    said
}

/// See `Store::files_calling`.
const FILES_CALLING: &str = "SELECT f.path
   FROM call_name s INDEXED BY call_name_name
   CROSS JOIN file f
  WHERE s.name = ?2
    AND f.blob_id = s.blob_id
    AND f.checkout_id = (SELECT id FROM checkout WHERE root = ?1)";

/// See `Store::files_calling_page`.
const FILES_CALLING_PAGE: &str = "SELECT p.blob_id, f.path
   FROM (SELECT s.blob_id FROM call_name s INDEXED BY call_name_name
          WHERE s.name = ?2 AND s.blob_id > ?3
            AND EXISTS (SELECT 1 FROM file f
                         WHERE f.blob_id = s.blob_id
                           AND f.checkout_id = (SELECT id FROM checkout WHERE root = ?1))
          ORDER BY s.blob_id
          LIMIT ?4) p
   CROSS JOIN file f
  WHERE f.blob_id = p.blob_id
    AND +f.checkout_id = (SELECT id FROM checkout WHERE root = ?1)";

/// Turn off SQLite's memory statistics, before any connection opens. They
/// are kept under one process-wide mutex that every allocation takes, which
/// connections on several threads then queue on (DEC-233); nothing reads them.
pub(crate) fn untracked_memory() {
    // SAFETY: sqlite3_config is only valid before SQLite initializes, which
    // the first connection does; this runs before any is opened, and a call
    // made too late is refused with SQLITE_MISUSE rather than acted on.
    unsafe {
        rusqlite::ffi::sqlite3_config(rusqlite::ffi::SQLITE_CONFIG_MEMSTATUS, 0);
    }
}

impl Store {
    /// Open the store at `path`. One this trekr can't use is set aside and
    /// rebuilt, or left to the newer trekr that wrote it (see [`recover`]).
    pub(crate) fn open(path: &Path) -> Result<Store> {
        recover::open(path, &schema::LAYOUT)
    }

    /// Open the store at `path` only if it is there, as it is: an early store
    /// (DEC-332), which its index removes when done.
    pub(crate) fn open_existing(path: &Path) -> Result<Store> {
        recover::open_existing(path, &schema::LAYOUT)
    }

    /// A second connection to the same database.
    ///
    /// `None` for an in-memory store: there is no path to reach it by, and a
    /// caller that needs its own handle has to fall back to reading everything
    /// through the one it already holds.
    pub(crate) fn reopen(&self) -> Result<Option<Store>> {
        let mut store = match &self.path {
            Some(path) if self.existing => Store::open_existing(path)?,
            Some(path) => Store::open(path)?,
            None => return Ok(None),
        };
        // A second connection answers as this one does.
        store.adopt(self.overlays())?;
        Ok(Some(store))
    }

    /// The database file, or `None` for an in-memory store.
    pub(crate) fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn open_in_memory() -> Result<Store> {
        match Store::init(Connection::open_in_memory()?, &schema::LAYOUT) {
            Ok((store, _)) => Ok(store),
            Err(recover::Refusal::Failed(error)) => Err(error),
            Err(_) => unreachable!("a new in-memory database is always usable"),
        }
    }

    /// The store, and the version whose index this call dropped (`Some(0)`
    /// for a fresh file).
    fn init(
        conn: Connection,
        layout: &schema::Layout,
    ) -> std::result::Result<(Store, Option<i64>), recover::Refusal> {
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
        let mut store = Store {
            conn,
            path: None,
            file: None,
            existing: false,
            timing: WriteTiming::default(),
            overlaid: overlay::Overlays::new(),
        };
        let version = schema_version(&store.conn)?;
        if version > layout.version {
            return Err(recover::Refusal::Newer(version));
        }
        let dropped = match version == layout.version {
            true => None,
            false => store.migrate(layout)?,
        };
        // Also the first read of the schema, where a damaged or truncated
        // file shows itself.
        if let Some(table) = store.missing_table(layout)? {
            return Err(recover::Refusal::Broken {
                from: None,
                reason: format!("no {table} table"),
            });
        }
        Ok((store, dropped))
    }

    /// Bring the schema to `layout`'s, as one transaction (DEC-079).
    ///
    /// The version is read again under the write lock: another process may
    /// have rebuilt the store between the unlocked check and here, and a
    /// second drop-and-create interleaved with the first is what left tables
    /// from two generations side by side.
    /// The version it dropped — `Some(0)` for a fresh file — and `None` when
    /// another process had already rebuilt it.
    fn migrate(
        &mut self,
        layout: &schema::Layout,
    ) -> std::result::Result<Option<i64>, recover::Refusal> {
        use recover::Refusal;
        // A no-op inside a transaction, so it is set around one. Off, the
        // drops are plain drops rather than a cascading delete of every fact.
        self.conn.execute_batch("PRAGMA foreign_keys=OFF;")?;
        // Another trekr may be mid-upgrade: a language server hot-reloaded
        // into this build drops the old index in one long transaction.
        self.conn.busy_handler(Some(upgrade_busy))?;
        let rebuilt = (|| {
            let tx = self
                .conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let version = schema_version(&tx)?;
            // An *older* binary must not drop a newer database. Two trekrs on
            // one machine — one installed, one freshly built — would otherwise
            // take turns wiping each other's index (DEC-300).
            if version > layout.version {
                return Err(Refusal::Newer(version));
            }
            if version == layout.version {
                return Ok(None);
            }
            // No migration, by design: see schema::VERSION. Reindexing costs
            // seconds and cannot leave the store half-converted.
            let rebuild = || -> Result<()> {
                for table in layout.tables.iter().chain(&schema::RETIRED) {
                    tx.execute_batch(&format!("DROP TABLE IF EXISTS {table};"))?;
                }
                tx.execute_batch(layout.sql)?;
                if version != 0 {
                    tx.execute(
                        "INSERT INTO upgrade (from_version, at) VALUES (?1, unixepoch())",
                        params![version],
                    )?;
                }
                tx.execute(
                    "INSERT INTO meta (key, value) VALUES ('schema_by', ?1)",
                    params![env!("CARGO_PKG_VERSION")],
                )?;
                tx.pragma_update(None, "user_version", layout.version)
            };
            rebuild().map_err(|e| Refusal::upgrade(version, terse(e)))?;
            tx.commit()?;
            Ok(Some(version))
        })();
        self.conn.busy_timeout(BUSY)?;
        self.conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        rebuilt
    }

    /// A table `layout` needs that the store doesn't have.
    fn missing_table(&self, layout: &schema::Layout) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")?;
        let have = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<HashSet<_>>>()?;
        Ok(layout
            .tables
            .iter()
            .find(|t| !have.contains(**t) && !schema::OPTIONAL.contains(t))
            .map(|t| t.to_string()))
    }

    /// Record that the store was rebuilt from `from`, so "not indexed" and
    /// the editor's refill say why (DEC-275). A rebuild after damage records
    /// this version: the format didn't change, the file did.
    fn record_rebuild(&self, from: i64) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO upgrade (from_version, at) VALUES (?1, unixepoch())",
                params![from],
            )
            .map(drop)
    }

    /// Why this store is no longer the one at its path, if it isn't: the file
    /// was set aside and replaced, or another trekr rebuilt it for another
    /// schema. A long-lived reader (the language server) reopens on this.
    pub(crate) fn replaced(&self) -> Option<String> {
        let path = self.path.as_ref()?;
        if recover::identity(path) != self.file {
            return Some(format!("{} was replaced", path.display()));
        }
        match schema_version(&self.conn) {
            Ok(v) if v != schema::VERSION => Some(format!(
                "the store was rebuilt as v{v}; this trekr is v{}",
                schema::VERSION
            )),
            _ => None,
        }
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
             it was rebuilt by another trekr, so this one must reopen it",
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
        self.write_with(root, files, facts, git_state, Mode::Whole)
    }

    /// `write` for some of a checkout's files, during its first index: these
    /// paths are added to the map and none are taken out, and the keys fold
    /// what the map holds after it (DEC-322). The index's last write is a
    /// whole one, which leaves the store as one whole write would have.
    pub(crate) fn write_part(
        &mut self,
        root: &str,
        files: &Files,
        facts: impl IntoIterator<Item = (Oid, Facts)>,
    ) -> Result<Indexed> {
        self.write_with(root, files, facts, 0, Mode::Part)
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
        self.write_with(root, files, facts, git_state, Mode::Bulk)
    }

    fn write_with(
        &mut self,
        root: &str,
        files: &Files,
        facts: impl IntoIterator<Item = (Oid, Facts)>,
        git_state: i64,
        mode: Mode,
    ) -> Result<Indexed> {
        // On its own, the write is its own immediate transaction: a deferred
        // one that reads first cannot wait for the lock, it fails.
        if self.autocommit() {
            return self.batch(|store| store.write_with(root, files, facts, git_state, mode));
        }
        let bulk = mode == Mode::Bulk;
        let part = mode == Mode::Part;
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
            let started = std::time::Instant::now();
            tx.execute_batch("PRAGMA temp_store=FILE;")?;
            for (_, create) in schema::BULK_INDEXES {
                tx.execute_batch(create)?;
            }
            tx.execute_batch("PRAGMA temp_store=MEMORY;")?;
            self.timing.rebuild += started.elapsed();
        }
        let map_started = std::time::Instant::now();

        tx.execute(
            "INSERT OR IGNORE INTO checkout
               (root, indexed_at, surface_key, namespace_key, map_key, git_state)
             VALUES (?1, unixepoch(), 0, 0, 0, 0)",
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
        let mut map_key = map_key(files);
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
        if !part && stored.0 == map_key && stored.1 {
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
            self.timing.map += map_started.elapsed();
            return Ok(counts);
        }

        // Only the rows that moved are written. The stored map is read whole —
        // one query — and diffed here: a path whose blob is unchanged costs
        // nothing, a vanished path is deleted, and anything new or edited is
        // upserted. Rewriting every row was most of a one-file reindex.
        let mut stored: HashMap<String, (i64, String, Digests)> = HashMap::new();
        {
            let mut read = tx.prepare(
                "SELECT f.path, f.blob_id, b.oid, b.surface, b.namespace
                   FROM file f JOIN blob b ON b.id = f.blob_id
                  WHERE f.checkout_id = ?1",
            )?;
            let rows = read.query_map(params![checkout_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    (r.get(1)?, r.get(2)?, Digests(r.get(3)?, r.get(4)?)),
                ))
            })?;
            for row in rows {
                let (path, found) = row?;
                stored.insert(path, found);
            }
        }
        let mut keys = Digests::default();
        {
            let mut ids: HashMap<&Oid, (i64, Digests)> = HashMap::new();
            let mut lookup =
                tx.prepare("SELECT id, surface, namespace FROM blob WHERE oid = ?1")?;
            // An insert for a new path and an update for an edited one, not
            // `INSERT OR REPLACE`: a statement that may delete as well as
            // insert opens a statement journal inside the savepoint, and its
            // cost grows with everything the transaction already wrote (DEC-191).
            let mut insert =
                tx.prepare("INSERT INTO file (checkout_id, path, blob_id) VALUES (?1, ?2, ?3)")?;
            let mut update =
                tx.prepare("UPDATE file SET blob_id = ?3 WHERE checkout_id = ?1 AND path = ?2")?;
            for (path, oid) in files {
                let (id, digests) = match stored.remove(path) {
                    Some((id, known, digests)) if known == oid.0 => (id, digests),
                    was => {
                        let found = match ids.get(oid) {
                            Some(found) => *found,
                            None => lookup.query_row(params![oid.0], |r| {
                                Ok((r.get::<_, i64>(0)?, Digests(r.get(1)?, r.get(2)?)))
                            })?,
                        };
                        let row = params![checkout_id, path, found.0];
                        match was {
                            Some(_) => update.execute(row)?,
                            None => insert.execute(row)?,
                        };
                        found
                    }
                };
                ids.insert(oid, (id, digests));
                // Order-independent, so the map's iteration order cannot
                // change the key; the path is mixed in because a rename moves
                // where an answer points even when no blob changed.
                keys = keys.add(path_hash(path), digests);
            }
            counts.blobs = ids.len();
            if part {
                // Written before, by an earlier part: still in the map.
                for (path, (_, oid, digests)) in &stored {
                    let hashed = path_hash(path);
                    keys = keys.add(hashed, *digests);
                    map_key = map_key.wrapping_add(hashed ^ path_hash(oid));
                }
            } else {
                // What is left was stored and is no longer in the checkout.
                let mut delete =
                    tx.prepare("DELETE FROM file WHERE checkout_id = ?1 AND path = ?2")?;
                for path in stored.keys() {
                    delete.execute(params![checkout_id, path])?;
                }
            }
        }

        tx.execute(
            "UPDATE checkout SET surface_key = ?2, namespace_key = ?3, map_key = ?4, git_state = ?5
              WHERE id = ?1",
            params![checkout_id, keys.0, keys.1, map_key, git_state],
        )?;

        tx.commit()?;
        self.timing.map += map_started.elapsed();
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

    /// Where the writes since the last call spent their time.
    pub(crate) fn take_timing(&mut self) -> WriteTiming {
        std::mem::take(&mut self.timing)
    }

    /// Outside any transaction — so a write here commits on its own rather
    /// than inside a `batch`.
    pub(crate) fn autocommit(&self) -> bool {
        self.conn.is_autocommit()
    }

    /// Run `work` as one transaction, so every `write` inside it commits once.
    ///
    /// Wait for another writer's turn as long as a writer takes, not as long
    /// as a query would (DEC-139), telling `notice` how long it has waited
    /// each time the lock is still held (DEC-171).
    pub(crate) fn wait_as_writer(&self, notice: WaitNotice) -> Result<()> {
        let _ = WAIT_NOTICE.set(notice);
        self.conn.busy_handler(Some(writer_busy))
    }

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
        // Every way out but a commit rolls back — an error, a COMMIT that
        // failed, a panic the LSP survives — or each later write on this
        // connection joins a transaction nobody finishes. Not a drop guard:
        // `work` needs the `&mut Store` the guard would borrow.
        let worked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(self)));
        let failed = match worked {
            Ok(Ok(value)) => {
                let started = std::time::Instant::now();
                match self.conn.execute_batch("COMMIT") {
                    Ok(()) => {
                        self.timing.commit += started.elapsed();
                        return Ok(value);
                    }
                    Err(error) => Err(error.into()),
                }
            }
            Ok(Err(error)) => Err(error),
            Err(panic) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                std::panic::resume_unwind(panic);
            }
        };
        let _ = self.conn.execute_batch("ROLLBACK");
        failed
    }

    /// One row per indexed checkout, plus the totals a caller wants to see.
    pub(crate) fn status(&self) -> Result<Vec<Checkout>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.root, c.kind, c.indexed_at, COUNT(f.path), COUNT(DISTINCT f.blob_id)
               FROM checkout c LEFT JOIN file f ON f.checkout_id = c.id
              GROUP BY c.id ORDER BY c.root",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Checkout {
                repo: r.get(0)?,
                kind: r.get(1)?,
                indexed_at: r.get(2)?,
                files: r.get(3)?,
                blobs: r.get(4)?,
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
            calls: one("SELECT COALESCE(SUM(calls), 0) FROM call_name")?,
        })
    }

    /// The definitions and constant references of a name in one checkout, in
    /// source order. Its call sites are read from the files that call it
    /// (`files_calling`): the index keeps which files, not where (DEC-193).
    ///
    /// **Name-level, not resolved.** Two unrelated classes called `Config` both
    /// answer here. Each row says what sort of mention it is, which is what the
    /// resolve layer will narrow on. Saying that plainly is better than a
    /// number that implies more than it knows.
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

    /// The mixins one checkout sends to a constant (`Widget.prepend self`,
    /// DEC-097), with the path relative to the checkout: `--dead` reads each
    /// as a use of the module sent (DEC-620).
    pub(crate) fn sent_mixins(&self, root: &str) -> Result<Vec<EdgeRow>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT a.owner, a.relation, a.target, f.path, a.line
               FROM ancestry a
               JOIN file f ON f.blob_id = a.blob_id
              WHERE f.checkout_id = (SELECT id FROM checkout WHERE root = ?1)
                AND substr(a.owner, 1, length(?2)) = ?2
                AND a.relation IN ('include', 'prepend', 'extend', 'singleton_prepend')
              ORDER BY f.path, a.line",
        )?;
        let rows = stmt.query_map(params![root, runtime::SENT], edge_row)?;
        rows.collect()
    }

    /// The constant references in one checkout written as any of `names`,
    /// unresolved: `--dead` resolves each against the tree (DEC-420).
    pub(crate) fn const_refs_named(
        &self,
        root: &str,
        names: &[String],
    ) -> Result<Vec<ConstRefRow>> {
        let mut found = Vec::new();
        for chunk in names.chunks(500) {
            let mut stmt = self.conn.prepare(&format!(
                "SELECT f.path, r.name, r.nesting, r.line
                   FROM const_ref r INDEXED BY const_ref_name
                   CROSS JOIN file f
                  WHERE r.name IN ({})
                    AND f.blob_id = r.blob_id
                    AND f.checkout_id = (SELECT id FROM checkout WHERE root = ?1)",
                (0..chunk.len())
                    .map(|i| format!("?{}", i + 2))
                    .collect::<Vec<_>>()
                    .join(",")
            ))?;
            let params = std::iter::once(root).chain(chunk.iter().map(String::as_str));
            let rows = stmt.query_map(rusqlite::params_from_iter(params), |r| {
                Ok(ConstRefRow {
                    path: r.get(0)?,
                    name: r.get(1)?,
                    nesting: split_nesting(&r.get::<_, String>(2)?),
                    line: r.get(3)?,
                })
            })?;
            for row in rows {
                found.push(row?);
            }
        }
        Ok(found)
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
    pub(crate) fn declarations(&self, roots: &Roots) -> Result<Vec<DeclRow>> {
        // Ordered here rather than by `ORDER BY c.id, f.path, d.line, d.col`:
        // SQLite's sorter carried every row's absolute path through a temp
        // b-tree and was a third of this query. Same keys, same byte order.
        let mut stmt = self.conn.prepare(&format!(
            "SELECT d.name, d.kind, d.nesting, d.target, c.root || '/' || f.path, d.line, d.col,
                    c.kind, c.root, c.id
               FROM def d
               JOIN file f ON f.blob_id = d.blob_id
               JOIN checkout c ON c.id = f.checkout_id
              WHERE c.root IN ({}) AND d.kind IN ('class','module','constant')",
            placeholders(roots.list.len())
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(&roots.list), |r| {
            let layer = roots.layer(&r.get::<_, String>(7)?, &r.get::<_, String>(8)?);
            Ok((
                (layer, r.get::<_, i64>(9)?),
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
        rows.retain(|(_, row)| roots.shows(&row.path));
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
    pub(crate) fn methods(&self, roots: &Roots) -> Result<Vec<MethodRow>> {
        self.method_rows(roots, None)
    }

    /// Just the methods with this name, for a tree that loads on demand.
    ///
    /// The whole point of the demand-loading design: nothing needs all 84,052
    /// of rails' methods, and `def(name)` is indexed, so one name is a few rows
    /// instead of a table scan and 137 ms of indexing.
    pub(crate) fn methods_named(&self, roots: &Roots, name: &str) -> Result<Vec<MethodRow>> {
        self.method_rows(roots, Some(name))
    }

    /// Every method, in `methods`' order, handed over one row at a time
    /// rather than collected — for a caller that keeps only part of each.
    pub(crate) fn each_method(&self, roots: &Roots, visit: impl FnMut(MethodRow)) -> Result<()> {
        self.visit_method_rows(roots, None, visit)
    }

    fn method_rows(&self, roots: &Roots, name: Option<&str>) -> Result<Vec<MethodRow>> {
        let mut rows = Vec::new();
        self.visit_method_rows(roots, name, |row| rows.push(row))?;
        Ok(rows)
    }

    fn visit_method_rows(
        &self,
        roots: &Roots,
        name: Option<&str>,
        mut visit: impl FnMut(MethodRow),
    ) -> Result<()> {
        // Layering is load-bearing: `lookup` takes the last definition, so a
        // reopened class must arrive after the class it reopens.
        let count = roots.list.len();
        let filter = if name.is_some() {
            format!("AND d.name = ?{}", count + 1)
        } else {
            String::new()
        };
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT d.name, d.nesting, d.singleton, d.visibility, d.params, d.via,
                    d.target, d.sig_returns, c.root || '/' || f.path, d.line, d.col,
                    d.target_line, d.target_col
               FROM def d
               JOIN file f ON f.blob_id = d.blob_id
               JOIN checkout c ON c.id = f.checkout_id
              WHERE c.root IN ({}) AND d.kind = 'method' {filter}
              ORDER BY {}, f.path, d.line, d.col",
            numbered(1, count),
            layered(count)
        ))?;
        let mut values: Vec<&dyn rusqlite::ToSql> = roots
            .list
            .iter()
            .map(|r| r as &dyn rusqlite::ToSql)
            .collect();
        if let Some(name) = name.as_ref() {
            values.push(name as &dyn rusqlite::ToSql);
        }
        let mut rows = stmt.query(values.as_slice())?;
        while let Some(r) = rows.next()? {
            let path: String = r.get(8)?;
            if !roots.shows(&path) {
                continue;
            }
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
                path,
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
    pub(crate) fn ancestry(&self, roots: &Roots) -> Result<Vec<EdgeRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT a.owner, a.relation, a.target, c.root || '/' || f.path, a.line
               FROM ancestry a
               JOIN file f ON f.blob_id = a.blob_id
               JOIN checkout c ON c.id = f.checkout_id
              WHERE c.root IN ({}) AND a.relation NOT IN ('dynamic', 'macro')
              ORDER BY {}, f.path, a.line, a.col",
            numbered(1, roots.list.len()),
            layered(roots.list.len())
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(&roots.list), edge_row)?;
        shown(roots, rows)
    }

    /// The mixins class macros send to whichever class body calls them: the
    /// `macro` edges alone (DEC-313), few, placed once a tree is built.
    pub(crate) fn macro_mixins(&self, roots: &Roots) -> Result<Vec<EdgeRow>> {
        let count = roots.list.len();
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT a.owner, a.relation, a.target, c.root || '/' || f.path, a.line
               FROM ancestry a
               JOIN file f ON f.blob_id = a.blob_id
               JOIN checkout c ON c.id = f.checkout_id
              WHERE c.root IN ({}) AND a.relation = 'macro'
              ORDER BY {}, f.path, a.line, a.col",
            numbered(1, count),
            layered(count)
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(&roots.list), edge_row)?;
        shown(roots, rows)
    }

    /// Which classes run each `on_load` hook: the `load_hooks` edges alone
    /// (DEC-098), read when a call in a hook's block is typed (DEC-214).
    pub(crate) fn load_hooks(&self, roots: &Roots) -> Result<Vec<EdgeRow>> {
        let count = roots.list.len();
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT a.owner, a.relation, a.target, c.root || '/' || f.path, a.line
               FROM ancestry a
               JOIN file f ON f.blob_id = a.blob_id
               JOIN checkout c ON c.id = f.checkout_id
              WHERE c.root IN ({}) AND a.relation = 'load_hooks'
              ORDER BY {}, f.path, a.line, a.col",
            numbered(1, count),
            layered(count)
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(&roots.list), edge_row)?;
        shown(roots, rows)
    }

    /// The scopes that define methods the source does not name (DEC-130):
    /// few, and read only when an answer is about to say a method is absent.
    pub(crate) fn dynamic_markers(&self, roots: &Roots) -> Result<Vec<EdgeRow>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT a.owner, a.relation, a.target, c.root || '/' || f.path, a.line
               FROM ancestry a
               JOIN file f ON f.blob_id = a.blob_id
               JOIN checkout c ON c.id = f.checkout_id
              WHERE c.root IN ({0}) AND a.relation = 'dynamic'
             UNION ALL
             -- Each class a partly compiled stdlib file opens makes methods
             -- no Ruby names: its extension's (DEC-181).
             SELECT CASE d.nesting WHEN '' THEN d.name ELSE d.name || ';' || d.nesting END,
                    'dynamic', '{COMPILED} ' || x.feature,
                    c.root || '/' || f.path, MIN(d.line)
               FROM compiled x
               JOIN checkout c ON c.id = x.checkout_id
               JOIN file f ON f.checkout_id = c.id AND f.path = x.path
               JOIN def d ON d.blob_id = f.blob_id AND d.kind IN ('class', 'module')
              WHERE c.root IN ({0})
              GROUP BY 1, 4
              ORDER BY 4, 5",
            placeholders(roots.list.len())
        ))?;
        let doubled: Vec<&String> = roots.list.iter().chain(&roots.list).collect();
        let rows = stmt.query_map(rusqlite::params_from_iter(doubled), edge_row)?;
        shown(roots, rows)
    }

    /// Where a class or module body calls `name` on itself, outside any
    /// method: the classes a macro of that name runs on (DEC-162), by the
    /// scope stack each call is written in, with the literal names it is
    /// handed.
    pub(crate) fn body_calls(&self, roots: &Roots, name: &str) -> Result<Vec<BodyCallRow>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT DISTINCT c.nesting, c.args, k.root || '/' || f.path, c.line
               FROM body_call c
               JOIN file f ON f.blob_id = c.blob_id
               JOIN checkout k ON k.id = f.checkout_id
              WHERE c.name = ?1 AND k.root IN ({})",
            numbered(2, roots.list.len())
        ))?;
        let params = std::iter::once(name.to_string()).chain(roots.list.iter().cloned());
        let rows = stmt.query_map(rusqlite::params_from_iter(params), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, u32>(3)?,
            ))
        })?;
        let mut seen: HashSet<(String, String)> = HashSet::new();
        let mut found = Vec::new();
        for row in rows {
            let (nesting, args, path, line) = row?;
            if !roots.shows(&path) || !seen.insert((nesting.clone(), args.clone())) {
                continue;
            }
            found.push(BodyCallRow {
                nesting: split_nesting(&nesting),
                args: args
                    .split('\t')
                    .map(|arg| (!arg.is_empty()).then(|| arg.to_string()))
                    .collect(),
                path,
                line,
            });
        }
        Ok(found)
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

    /// The files calling any of `names`, sorted and deduplicated.
    pub(crate) fn files_calling_any(&self, root: &str, names: &[&str]) -> Result<Vec<String>> {
        let mut paths = Vec::new();
        for name in names {
            paths.extend(self.files_calling(root, name)?);
        }
        paths.sort_unstable();
        paths.dedup();
        Ok(paths)
    }

    /// A page of the files in a checkout that call `name`, in the index's
    /// own order: the blobs after `after` calling it, at most `rows` of them,
    /// as (the last blob read, each of their files). Pass the last blob back
    /// for the next page; an empty page is the end. A page holds every file
    /// of its blobs, so none is split across two.
    ///
    /// For a question that may stop long before the last file. `files_calling`
    /// must read every file calling the name to sort and deduplicate, where
    /// this reads only as far as it is asked to. A blob two paths share is
    /// listed under each.
    ///
    /// The plan is pinned: from the name's index, then its files. Left to
    /// itself, with statistics saying the name is everywhere, the bundled
    /// SQLite walks every file of the checkout and sorts all their calls —
    /// the whole listing again, 0.7 s a page at ten times discourse — or scans
    /// the call table in row order, which for a rare name reads all of it.
    /// The files are reached by blob because `+` takes the checkout's column
    /// off the table: `INDEXED BY` cannot name an index through the view an
    /// overlay puts in front of the map.
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

    /// Whether any file of the checkout at `root` calls `name`.
    pub(crate) fn calls_name(&self, root: &str, name: &str) -> Result<bool> {
        self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM call_name s INDEXED BY call_name_name
                             CROSS JOIN file f
                            WHERE s.name = ?2
                              AND f.blob_id = s.blob_id
                              AND f.checkout_id = (SELECT id FROM checkout WHERE root = ?1))",
            params![root, name],
            |r| r.get(0),
        )
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
            "SELECT MIN(?3, COALESCE(SUM(s.calls - s.symbols), 0))
               FROM call_name s INDEXED BY call_name_name
               CROSS JOIN file f
              WHERE s.name = ?1
                AND f.blob_id = s.blob_id
                AND f.checkout_id = (SELECT id FROM checkout WHERE root = ?2)",
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

    /// The gem of `root`'s bundle that writes the most calls of `name`, with
    /// how many: a name a gem calls on an object it is handed, which
    /// `--dead` cannot see reach the app (DEC-367). The bundle is what the
    /// app's own index recorded, so no other app's index changes it.
    pub(crate) fn bundle_calls(&self, root: &str, name: &str) -> Result<Option<(String, i64)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT k.root, SUM(s.calls - s.symbols) AS n
               FROM call_name s INDEXED BY call_name_name
               CROSS JOIN file f
               CROSS JOIN checkout k
              WHERE s.name = ?1
                AND f.blob_id = s.blob_id
                AND k.id = f.checkout_id
                AND k.root IN (SELECT g.gem_root FROM gem_use g
                                 JOIN checkout c ON c.id = g.checkout_id
                                WHERE c.root = ?2 AND g.name IS NOT NULL)
              GROUP BY k.root
             HAVING n > 0
              ORDER BY n DESC, k.root
              LIMIT 1",
        )?;
        let mut rows = stmt.query(params![name, root])?;
        Ok(match rows.next()? {
            Some(row) => Some((row.get(0)?, row.get(1)?)),
            None => None,
        })
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

    /// Moves whenever another connection commits to the store, and never
    /// for this one's own writes: what a resident front checks before it
    /// re-reads anything it derived from the store (DEC-333).
    pub(crate) fn data_version(&self) -> Result<i64> {
        self.conn.query_row("PRAGMA data_version", [], |r| r.get(0))
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
    pub(crate) fn program_roots(&self, roots: &Roots) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT c.root, f.path FROM file f JOIN checkout c ON c.id = f.checkout_id
              WHERE c.root IN ({}) AND f.path LIKE '%.gemspec'",
            placeholders(roots.list.len())
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(&roots.list), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut found = roots.list.clone();
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
        self.checkout_keys("surface_key", roots)
    }

    /// `namespace_key` for each root in turn: what a tree snapshot is a
    /// function of, where `surface_keys` is what a whole tree is (DEC-194).
    pub(crate) fn namespace_keys(&self, roots: &[String]) -> Result<Vec<i64>> {
        self.checkout_keys("namespace_key", roots)
    }

    fn checkout_keys(&self, column: &str, roots: &[String]) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT root, {column} FROM checkout WHERE root IN ({})",
            placeholders(roots.len())
        ))?;
        let known: HashMap<String, i64> = stmt
            .query_map(rusqlite::params_from_iter(roots), |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect::<Result<_>>()?;
        // A key is a sum over files, so an overlay moves it by its own.
        let shift = |root: &String| {
            let shift = self
                .overlaid
                .get(root)
                .map(|overlaid| overlaid.shift)
                .unwrap_or_default();
            match column {
                "surface_key" => shift.0,
                _ => shift.1,
            }
        };
        Ok(roots
            .iter()
            .map(|root| match known.get(root) {
                Some(key) => key.wrapping_add(shift(root)),
                None => 0,
            })
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

    /// Record that this checkout's bundle resolves these gems, each
    /// `(root, name)`, and that it runs on this stdlib.
    ///
    /// Rewritten wholesale on every index, so a gem dropped
    /// from a Gemfile.lock stops being claimed.
    ///
    /// Also where a checkout becomes a gem, and where a gem is last seen: each
    /// one named is stamped `kind = 'gem'` (the stdlib `'stdlib'`) and its
    /// `indexed_at` moved to now, inside the index's own transaction, so
    /// `--gc` costs a query nothing.
    pub(crate) fn set_gems_used(
        &mut self,
        root: &str,
        gems: &[(String, String)],
        stdlib: Option<&str>,
    ) -> Result<()> {
        let tx = self.conn.savepoint()?;
        let id: i64 = tx.query_row(
            "SELECT id FROM checkout WHERE root = ?1",
            params![root],
            |r| r.get(0),
        )?;
        tx.execute("DELETE FROM gem_use WHERE checkout_id = ?1", params![id])?;
        {
            let mut insert = tx.prepare(
                "INSERT OR IGNORE INTO gem_use (checkout_id, gem_root, name) VALUES (?1, ?2, ?3)",
            )?;
            let mut seen = tx.prepare(
                "UPDATE checkout SET kind = ?2, indexed_at = unixepoch() WHERE root = ?1",
            )?;
            for (gem, name) in gems {
                insert.execute(params![id, gem, name])?;
                seen.execute(params![gem, "gem"])?;
            }
            if let Some(stdlib) = stdlib {
                insert.execute(params![id, stdlib, None::<String>])?;
                seen.execute(params![stdlib, "stdlib"])?;
            }
        }
        tx.commit()
    }

    /// Record which of a stdlib checkout's files each default gem owns,
    /// as `(name, version, path)` (DEC-180). Written once, with the stdlib's
    /// files: a Ruby's install does not change under it.
    pub(crate) fn set_default_gems<'a>(
        &mut self,
        root: &str,
        owned: impl IntoIterator<Item = (&'a str, &'a str, &'a str)>,
    ) -> Result<()> {
        let tx = self.conn.savepoint()?;
        let id: i64 = tx.query_row(
            "SELECT id FROM checkout WHERE root = ?1",
            params![root],
            |r| r.get(0),
        )?;
        tx.execute(
            "DELETE FROM default_gem WHERE checkout_id = ?1",
            params![id],
        )?;
        {
            let mut insert = tx.prepare(
                "INSERT OR IGNORE INTO default_gem (checkout_id, name, version, path)
                 VALUES (?1, ?2, ?3, ?4)",
            )?;
            for (name, version, path) in owned {
                insert.execute(params![id, name, version, path])?;
            }
        }
        tx.commit()
    }

    /// The signatures a stdlib checkout is served with (DEC-240).
    pub(crate) fn rbs(&self, stdlib: &str) -> Result<Option<Rbs>> {
        self.conn
            .query_row(
                "SELECT r.key, r.version, r.dir, r.core, r.stdlib, r.sigs
                   FROM rbs r
                   JOIN rbs_use u ON u.rbs_id = r.id
                   JOIN checkout c ON c.id = u.checkout_id
                  WHERE c.root = ?1",
                params![stdlib],
                |r| {
                    Ok(Rbs {
                        key: r.get(0)?,
                        version: r.get(1)?,
                        dir: r.get(2)?,
                        core: r.get(3)?,
                        stdlib: r.get(4)?,
                        sigs: r.get(5)?,
                    })
                },
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
    }

    /// Which signatures a stdlib checkout is served with, and why that
    /// gem, without reading them.
    pub(crate) fn rbs_about(&self, stdlib: &str) -> Result<Option<RbsAbout>> {
        self.conn
            .query_row(
                "SELECT r.key, r.version, r.dir, u.chosen FROM rbs r
                   JOIN rbs_use u ON u.rbs_id = r.id
                   JOIN checkout c ON c.id = u.checkout_id
                  WHERE c.root = ?1",
                params![stdlib],
                |r| {
                    Ok(RbsAbout {
                        key: r.get(0)?,
                        version: r.get(1)?,
                        dir: r.get(2)?,
                        chosen: r.get(3)?,
                    })
                },
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
    }

    /// The directory each stored set of signatures is written under
    /// (`rbs-3.8.0-1a2b3c4d`).
    pub(crate) fn core_dir_names(&self) -> Result<HashSet<String>> {
        let mut stmt = self.conn.prepare("SELECT version, key FROM rbs")?;
        stmt.query_map([], |r| {
            Ok(crate::tree::core_dir_name(
                &r.get::<_, String>(0)?,
                &r.get::<_, String>(1)?,
            ))
        })?
        .collect()
    }

    /// Whether signatures under this key are already stored.
    pub(crate) fn has_rbs(&self, key: &str) -> Result<bool> {
        self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM rbs WHERE key = ?1)",
            params![key],
            |r| r.get(0),
        )
    }

    /// Serve a stdlib checkout with the signatures under `key` — stored
    /// first when `new` holds them — or with none. Signatures no checkout
    /// is served with any more are dropped.
    pub(crate) fn set_rbs(
        &mut self,
        stdlib: &str,
        key: Option<(&str, &str)>,
        new: Option<&Rbs>,
    ) -> Result<()> {
        let tx = self.conn.savepoint()?;
        let id: i64 = tx.query_row(
            "SELECT id FROM checkout WHERE root = ?1",
            params![stdlib],
            |r| r.get(0),
        )?;
        if let Some(rbs) = new {
            tx.execute(
                "INSERT INTO rbs (key, version, dir, core, stdlib, sigs)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT (key) DO NOTHING",
                params![
                    rbs.key,
                    rbs.version,
                    rbs.dir,
                    rbs.core,
                    rbs.stdlib,
                    rbs.sigs
                ],
            )?;
        }
        tx.execute("DELETE FROM rbs_use WHERE checkout_id = ?1", params![id])?;
        if let Some((key, chosen)) = key {
            tx.execute(
                "INSERT INTO rbs_use (checkout_id, rbs_id, chosen)
                 SELECT ?1, id, ?3 FROM rbs WHERE key = ?2",
                params![id, key, chosen],
            )?;
        }
        tx.execute(
            "DELETE FROM rbs WHERE id NOT IN (SELECT rbs_id FROM rbs_use)",
            [],
        )?;
        tx.commit()
    }

    /// Record which of a stdlib checkout's files are partly compiled, as
    /// `(path, feature)` (DEC-181).
    pub(crate) fn set_compiled(&mut self, root: &str, files: &[(String, String)]) -> Result<()> {
        let tx = self.conn.savepoint()?;
        let id: i64 = tx.query_row(
            "SELECT id FROM checkout WHERE root = ?1",
            params![root],
            |r| r.get(0),
        )?;
        tx.execute("DELETE FROM compiled WHERE checkout_id = ?1", params![id])?;
        {
            let mut insert = tx.prepare(
                "INSERT OR IGNORE INTO compiled (checkout_id, path, feature) VALUES (?1, ?2, ?3)",
            )?;
            for (path, feature) in files {
                insert.execute(params![id, path, feature])?;
            }
        }
        tx.commit()
    }

    /// The checkouts a tree for `root` is assembled from, in layering order,
    /// and the stdlib files it does not see: those of each default gem the
    /// app bundles a copy of by name (DEC-180). Read from what the app's own
    /// index recorded, so no other app's index can change it.
    pub(crate) fn tree_roots(&self, root: &str) -> Result<Roots> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT g.gem_root, g.name IS NULL FROM gem_use g
               JOIN checkout c ON c.id = g.checkout_id
              WHERE c.root = ?1
              ORDER BY g.name IS NOT NULL, g.gem_root",
        )?;
        let used = stmt
            .query_map(params![root], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?))
            })?
            .collect::<Result<Vec<_>>>()?;
        let stdlib = used
            .iter()
            .find(|(_, stdlib)| *stdlib)
            .map(|(root, _)| root.clone());
        let mut list: Vec<String> = used.into_iter().map(|(root, _)| root).collect();
        list.push(root.to_string());
        let mut stmt = self.conn.prepare_cached(
            "SELECT s.root || '/' || d.path
               FROM checkout c
               JOIN gem_use u ON u.checkout_id = c.id
               JOIN checkout s ON s.root = u.gem_root AND s.kind = 'stdlib'
               JOIN default_gem d ON d.checkout_id = s.id
              WHERE c.root = ?1
                AND d.name IN (SELECT name FROM gem_use
                                WHERE checkout_id = c.id AND name IS NOT NULL)",
        )?;
        let hidden = stmt
            .query_map(params![root], |r| r.get(0))?
            .collect::<Result<HashSet<String>>>()?;
        Ok(Roots {
            list,
            hidden,
            stdlib,
        })
    }

    /// The default gems an app sees hidden in its stdlib, by name.
    pub(crate) fn hidden_default_gems(&self, root: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT d.name
               FROM checkout c
               JOIN gem_use u ON u.checkout_id = c.id
               JOIN checkout s ON s.root = u.gem_root AND s.kind = 'stdlib'
               JOIN default_gem d ON d.checkout_id = s.id
              WHERE c.root = ?1
                AND d.name IN (SELECT name FROM gem_use
                                WHERE checkout_id = c.id AND name IS NOT NULL)
              ORDER BY d.name",
        )?;
        stmt.query_map(params![root], |r| r.get(0))?.collect()
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

    /// The gem holding this path: the deepest indexed checkout containing it,
    /// when that checkout is a gem — or a Ruby's stdlib, which answers from
    /// an app that runs on it as a gem does (DEC-180).
    ///
    /// Asked before git is: bundler's checkout of a git gem is a clone with a
    /// `.git` of its own, so git's toplevel for a file in it is the clone —
    /// never indexed, and for a monorepo not even the gem (DEC-150).
    pub(crate) fn gem_containing(&self, path: &str) -> Result<Option<String>> {
        let Some(root) = self.checkout_containing(path)? else {
            return Ok(None);
        };
        let kind: String = self.conn.query_row(
            "SELECT kind FROM checkout WHERE root = ?1",
            params![root],
            |r| r.get(0),
        )?;
        Ok((kind == "gem" || kind == "stdlib").then_some(root))
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
        let Some((checkout_id, keys, map_key)) = tx
            .query_row(
                "SELECT id, surface_key, namespace_key, map_key FROM checkout WHERE root = ?1",
                params![root],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        Digests(r.get(1)?, r.get(2)?),
                        r.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()?
        else {
            return Ok(false);
        };

        let old: Option<(i64, String, Digests)> = tx
            .query_row(
                "SELECT b.id, b.oid, b.surface, b.namespace
                   FROM file f JOIN blob b ON b.id = f.blob_id
                  WHERE f.checkout_id = ?1 AND f.path = ?2",
                params![checkout_id, relative],
                |r| Ok((r.get(0)?, r.get(1)?, Digests(r.get(2)?, r.get(3)?))),
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
        let Some((blob_id, digests)) = tx
            .query_row(
                "SELECT id, surface, namespace FROM blob WHERE oid = ?1",
                params![oid.0],
                |r| Ok((r.get::<_, i64>(0)?, Digests(r.get(1)?, r.get(2)?))),
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
        let (mut keys, mut map_key) = (keys, map_key);
        if let Some((_, known, old_digests)) = &old {
            keys = keys.sub(hashed, *old_digests);
            map_key = map_key.wrapping_sub(hashed ^ path_hash(known));
        }
        keys = keys.add(hashed, digests);
        map_key = map_key.wrapping_add(hashed ^ path_hash(&oid.0));
        tx.execute(
            "UPDATE checkout SET surface_key = ?2, namespace_key = ?3, map_key = ?4 WHERE id = ?1",
            params![checkout_id, keys.0, keys.1, map_key],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// `root`'s map as the index wrote it, path → blob oid, whatever this
    /// connection overlays.
    pub(crate) fn file_map(&self, root: &str) -> Result<HashMap<String, String>> {
        let mut stmt = self.conn.prepare(
            "SELECT f.path, b.oid FROM main.file f JOIN blob b ON b.id = f.blob_id
              WHERE f.checkout_id = (SELECT id FROM checkout WHERE root = ?1)",
        )?;
        let rows = stmt.query_map(params![root], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect()
    }

    /// Whether `root` is a repository's checkout, not a gem's or a Ruby's.
    pub(crate) fn is_repo(&self, root: &str) -> Result<bool> {
        self.conn
            .query_row(
                "SELECT kind = 'repo' FROM checkout WHERE root = ?1",
                params![root],
                |r| r.get(0),
            )
            .optional()
            .map(|repo| repo.unwrap_or(false))
    }

    /// Record a blob's facts. Content-addressed, so a query may: the next
    /// index finds the blob known, and no map points at it until one does.
    /// Like `refresh_file`, it fails at once when another process is writing.
    pub(crate) fn add_blob(&mut self, oid: &Oid, facts: &Facts) -> Result<()> {
        let tx = self.conn.transaction()?;
        Store::check_schema(&tx)?;
        insert_facts(&tx, oid, facts)?;
        tx.commit()
    }

    /// Whether `root`'s map holds `path`, relative to it.
    pub(crate) fn maps(&self, root: &str, path: &str) -> Result<bool> {
        self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM file f JOIN checkout c ON c.id = f.checkout_id
                            WHERE c.root = ?1 AND f.path = ?2)",
            params![root, path],
            |r| r.get::<_, i64>(0).map(|n| n != 0),
        )
    }

    pub(crate) fn has_checkout(&self, root: &str) -> Result<bool> {
        self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM checkout WHERE root = ?1)",
            params![root],
            |r| r.get::<_, i64>(0).map(|n| n != 0),
        )
    }

    /// How many files these checkouts map, together.
    pub(crate) fn file_count(&self, roots: &[String]) -> Result<u64> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT COUNT(*) FROM file f JOIN checkout c ON c.id = f.checkout_id WHERE c.root = ?1",
        )?;
        let mut total = 0;
        for root in roots {
            total += stmt.query_row(params![root], |r| r.get::<_, i64>(0))? as u64;
        }
        Ok(total)
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
        // Statistics are the store's to gather, not an overlay's copy.
        self.unshadow();
        let _ = self.conn.execute_batch("PRAGMA optimize;");
    }
}

impl Store {
    /// Copy the write-ahead log into the store and empty it, as the last
    /// connection to close does — which an index a query spawned is not:
    /// the query holds one open, and the log stayed tens of megabytes.
    /// Best effort, and briefly: a reader mid-transaction keeps it.
    pub(crate) fn truncate_wal(&self) {
        let _ = self.conn.busy_timeout(std::time::Duration::ZERO);
        for _ in 0..10 {
            let done: rusqlite::Result<i64> =
                self.conn
                    .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0));
            if matches!(done, Ok(0)) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
}

/// How long a connection waits for another's lock before giving up.
pub(super) const BUSY: std::time::Duration = std::time::Duration::from_secs(5);

/// How long an index, a drop or a collection waits for another writer: one
/// cold bundle's gems are one transaction and take far longer than `BUSY`
/// (DEC-139). A query never waits at all (DEC-066).
const WRITER_WAIT: std::time::Duration = std::time::Duration::from_secs(600);

/// `WRITER_WAIT`, or the e2e suite's shorter one: a test of what an index
/// does once the wait runs out cannot sit through ten minutes.
pub(crate) fn writer_wait() -> std::time::Duration {
    static WAIT: std::sync::OnceLock<std::time::Duration> = std::sync::OnceLock::new();
    *WAIT.get_or_init(|| {
        std::env::var("TREKR_TEST_WRITER_WAIT_MS")
            .ok()
            .and_then(|ms| ms.parse().ok())
            .map_or(WRITER_WAIT, std::time::Duration::from_millis)
    })
}

/// Told how long a writer has waited so far, each time the lock is still held.
pub(crate) type WaitNotice = fn(std::time::Duration);

static WAIT_NOTICE: std::sync::OnceLock<WaitNotice> = std::sync::OnceLock::new();

/// A writer's busy handler: SQLite's own `busy_timeout`, plus the notice.
/// SQLite takes a plain `fn`, and restarts `attempt` at 0 for each wait.
fn writer_busy(attempt: i32) -> bool {
    thread_local!(static STARTED: std::cell::Cell<std::time::Instant> =
        std::cell::Cell::new(std::time::Instant::now()));
    if attempt == 0 {
        STARTED.set(std::time::Instant::now());
    }
    let waited = STARTED.get().elapsed();
    let Some(pause) = writer_pause(attempt, waited) else {
        return false;
    };
    if let Some(notice) = WAIT_NOTICE.get() {
        notice(waited);
    }
    std::thread::sleep(pause);
    true
}

/// An upgrade's busy handler: as long as a writer waits, and said once, since
/// nothing else tells the person at the prompt why the command stalls.
fn upgrade_busy(attempt: i32) -> bool {
    thread_local!(static STARTED: std::cell::Cell<(std::time::Instant, bool)> =
        std::cell::Cell::new((std::time::Instant::now(), false)));
    if attempt == 0 {
        STARTED.set((std::time::Instant::now(), false));
    }
    let (started, told) = STARTED.get();
    let waited = started.elapsed();
    let Some(pause) = writer_pause(attempt, waited) else {
        return false;
    };
    // Processes opening the store together collide briefly; that is no news.
    if !told && waited > std::time::Duration::from_secs(1) {
        eprintln!("trekr: waiting for another trekr writing to the index (often one upgrading it)");
        STARTED.set((started, true));
    }
    std::thread::sleep(pause);
    true
}

/// How long to sleep before trying the lock again — SQLite's `busy_timeout`
/// backoff — or `None` once a writer has waited `WRITER_WAIT`.
fn writer_pause(attempt: i32, waited: std::time::Duration) -> Option<std::time::Duration> {
    const DELAYS_MS: [u64; 12] = [1, 2, 5, 10, 15, 20, 25, 25, 25, 50, 50, 100];
    let delay = DELAYS_MS[attempt.clamp(0, 11) as usize];
    (waited < writer_wait()).then(|| std::time::Duration::from_millis(delay))
}

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
    /// `repo`, `gem` for a checkout some repo's bundle resolves, or `stdlib`
    /// for a Ruby's standard library some repo runs on (DEC-180).
    pub(crate) kind: String,
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

/// Which signatures a stdlib is served with (DEC-240), and why that gem:
/// `bundled`, `installed` or `other` (DEC-242).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RbsAbout {
    pub(crate) key: String,
    pub(crate) version: String,
    pub(crate) dir: String,
    pub(crate) chosen: String,
}

/// A Ruby's signatures, as stubs (DEC-240).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Rbs {
    pub(crate) key: String,
    pub(crate) version: String,
    pub(crate) dir: String,
    pub(crate) core: String,
    pub(crate) stdlib: String,
    pub(crate) sigs: String,
}

/// A class, module, or constant declaration, as the blob layer recorded it —
/// name as written, nesting unresolved.
#[derive(Clone, Debug)]
pub(crate) struct DeclRow {
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) nesting: Vec<String>,
    pub(crate) target: Option<String>,
    pub(crate) path: String,
    pub(crate) line: u32,
    pub(crate) col: u32,
}

#[derive(Clone, Debug)]
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

fn edge_row(r: &rusqlite::Row<'_>) -> Result<EdgeRow> {
    Ok(EdgeRow {
        owner: split_nesting(&r.get::<_, String>(0)?),
        relation: r.get(1)?,
        target: r.get(2)?,
        path: r.get(3)?,
        line: r.get(4)?,
    })
}

/// A body's call on itself, as `body_calls` reads it.
#[derive(Debug)]
pub(crate) struct BodyCallRow {
    pub(crate) nesting: Vec<String>,
    /// Each positional argument's literal name, or `None`.
    pub(crate) args: Vec<Option<String>>,
    /// The file the call is written in, absolute.
    pub(crate) path: String,
    pub(crate) line: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct EdgeRow {
    /// Scope stack including the receiving class or module, innermost first.
    pub(crate) owner: Vec<String>,
    pub(crate) relation: String,
    pub(crate) target: String,
    /// The file that wrote it, absolute like a site's path: which of two
    /// conflicting declarations of the owner it belongs to (DEC-072).
    pub(crate) path: String,
    pub(crate) line: u32,
}

/// A constant reference as written, with the file it is in.
#[derive(Debug)]
pub(crate) struct ConstRefRow {
    pub(crate) path: String,
    pub(crate) name: String,
    pub(crate) nesting: Vec<String>,
    pub(crate) line: u32,
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

impl Ref {
    /// A call site read from its file, as the listing shows one.
    pub(crate) fn call(path: &str, call: &Call) -> Ref {
        Ref {
            path: path.to_string(),
            line: call.pos.line,
            col: call.pos.col,
            role: "call".to_string(),
            kind: None,
            recv: Some(call.recv.as_str().to_string()),
            recv_text: call.recv_text.clone(),
            nesting: call.nesting.clone(),
            tier: None,
            owner: None,
        }
    }
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
/// The edges whose file the app sees.
fn shown(roots: &Roots, rows: impl Iterator<Item = Result<EdgeRow>>) -> Result<Vec<EdgeRow>> {
    let mut kept = Vec::new();
    for row in rows {
        let row = row?;
        if roots.shows(&row.path) {
            kept.push(row);
        }
    }
    Ok(kept)
}

/// `?first, …` for `count` parameters, numbered so that a query can name
/// one of them again.
fn numbered(first: usize, count: usize) -> String {
    if count == 0 {
        return "NULL".to_string();
    }
    (first..first + count)
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ")
}

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

/// A blob's two digests, or a checkout's folded sums of them: its surface
/// (`Facts::surface`, every definition) and its namespace
/// (`Facts::namespace`, what the tree snapshot holds).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Digests(i64, i64);

impl Digests {
    /// A file's contribution, `path ^ digest`, folded in; the sum is
    /// order-independent, so the map's iteration order cannot move a key.
    fn add(self, path: i64, blob: Digests) -> Digests {
        Digests(
            self.0.wrapping_add(path ^ blob.0),
            self.1.wrapping_add(path ^ blob.1),
        )
    }

    fn sub(self, path: i64, blob: Digests) -> Digests {
        Digests(
            self.0.wrapping_sub(path ^ blob.0),
            self.1.wrapping_sub(path ^ blob.1),
        )
    }
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
        "INSERT INTO blob (oid, lines, parse_errors, surface, namespace, written_by)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT (oid) DO NOTHING",
        params![
            oid.0,
            facts.lines as i64,
            facts.parse_errors as i64,
            facts.surface() as i64,
            facts.namespace() as i64,
            schema::VERSION
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
    for d in facts.defs.iter().filter(|d| !d.is_group_member()) {
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

    let mut body_call = tx.prepare_cached(
        "INSERT INTO body_call (blob_id, name, nesting, args, line) VALUES (?1,?2,?3,?4,?5)",
    )?;
    for c in &facts.body_calls {
        let args: Vec<&str> = c.args.iter().map(|a| a.as_deref().unwrap_or("")).collect();
        body_call.execute(params![
            blob_id,
            c.name,
            join_nesting(&c.nesting),
            args.join("\t"),
            c.line,
        ])?;
    }

    let mut named: BTreeMap<&str, (i64, i64)> = BTreeMap::new();
    for c in &facts.calls {
        let counts = named.entry(c.name.as_str()).or_default();
        counts.0 += 1;
        counts.1 += i64::from(c.recv == RecvShape::Symbol);
    }
    let mut call = tx.prepare_cached(
        "INSERT INTO call_name (blob_id, name, calls, symbols) VALUES (?1,?2,?3,?4)",
    )?;
    for (name, (calls, symbols)) in named {
        call.execute(params![blob_id, name, calls, symbols])?;
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
    fn a_writer_waits_longer_than_a_query() {
        let store = Store::open_in_memory().unwrap();
        let waits = store
            .conn
            .pragma_query_value(None, "busy_timeout", |r| r.get::<_, i64>(0))
            .unwrap();
        assert_eq!(waits, BUSY.as_millis() as i64);
        let past = |secs| std::time::Duration::from_secs(secs);
        assert!(writer_pause(0, BUSY + past(1)).is_some());
        assert!(writer_pause(500, WRITER_WAIT - past(1)).is_some());
        assert_eq!(writer_pause(500, WRITER_WAIT), None);
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
        assert_eq!(seen, ["a.rb", "b.rb"], "one row per file, this checkout's");

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
        assert!(
            plan.iter()
                .any(|step| step.contains("USING COVERING INDEX call_name_name")),
            "{plan:?}"
        );
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
                .any(|step| step.contains("SEARCH s USING COVERING INDEX call_name_name")),
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
        assert!(index_names(&store).contains(&"call_name_name".to_string()));
    }

    #[test]
    fn a_batch_that_does_not_commit_leaves_no_transaction_open() {
        let mut store = Store::open_in_memory().unwrap();
        // Checked at COMMIT, so the commit itself is what fails.
        store
            .conn
            .execute_batch(
                "PRAGMA foreign_keys = ON;
                 CREATE TEMP TABLE parent(id INTEGER PRIMARY KEY);
                 CREATE TEMP TABLE child(p REFERENCES parent(id) DEFERRABLE INITIALLY DEFERRED);",
            )
            .unwrap();
        let orphan = |s: &mut Store| s.conn.execute_batch("INSERT INTO child VALUES (1)");
        assert!(store.batch(orphan).is_err(), "the commit fails");
        assert!(store.autocommit(), "after a failed commit");

        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            store.batch(|s| -> rusqlite::Result<()> {
                s.conn.execute_batch("INSERT INTO parent VALUES (1)")?;
                panic!("mid-batch")
            })
        }));
        assert!(panicked.is_err());
        assert!(store.autocommit(), "after a panic");
        let rows: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM parent", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0, "and what it wrote is gone");
    }

    /// What a resident front keys its stamps on: another connection's
    /// commit moves it, this connection's own does not (DEC-333).
    #[test]
    fn data_version_moves_for_another_connection_s_commit_only() {
        let dir = std::env::temp_dir().join(format!("trekr-version-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let reader = Store::open(&dir.join("t.db")).unwrap();
        let writer = reader.reopen().unwrap().unwrap();
        let before = reader.data_version().unwrap();
        reader.set_warming("/own", 0, 1).unwrap();
        assert_eq!(reader.data_version().unwrap(), before);
        writer.set_warming("/other", 0, 1).unwrap();
        assert_ne!(reader.data_version().unwrap(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A first index written in parts, then whole, ends as one whole write
    /// would: the same map, and the same keys a tree is stamped by. Each
    /// part's keys fold what the map holds after it (DEC-322).
    #[test]
    fn parts_then_a_whole_write_leave_what_one_whole_write_does() {
        let sources = [
            ("a.rb", "class A\n  def go; end\nend\n"),
            ("b.rb", "class B < A\nend\n"),
            ("c.rb", "module C\nend\n"),
        ];
        let map = |names: &[&str]| -> (Files, Vec<(Oid, Facts)>) {
            let mut files = Files::new();
            let mut facts = Vec::new();
            for (path, src) in sources.iter().filter(|(p, _)| names.contains(p)) {
                let oid = crate::scan::hash_blob(src.as_bytes());
                files.insert(path.to_string(), oid.clone());
                facts.push((oid, crate::extract::extract(src.as_bytes())));
            }
            (files, facts)
        };
        let keys = |store: &Store| -> (i64, i64, i64, i64) {
            store
                .conn
                .query_row(
                    "SELECT surface_key, namespace_key, map_key, COUNT(f.path)
                       FROM checkout c JOIN file f ON f.checkout_id = c.id GROUP BY c.id",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .unwrap()
        };
        let mut whole = Store::open_in_memory().unwrap();
        let (files, facts) = map(&["a.rb", "b.rb", "c.rb"]);
        whole.write("/r", &files, facts, 0).unwrap();

        let mut parts = Store::open_in_memory().unwrap();
        let (files, facts) = map(&["b.rb"]);
        parts.write_part("/r", &files, facts).unwrap();
        let (files, facts) = map(&["a.rb"]);
        parts.write_part("/r", &files, facts).unwrap();
        let (_, _, part_map, _) = keys(&parts);
        let (two, _) = map(&["a.rb", "b.rb"]);
        assert_eq!(
            part_map,
            map_key(&two),
            "a part's key folds the map it leaves"
        );
        let (files, _) = map(&["a.rb", "b.rb", "c.rb"]);
        let (_, facts) = map(&["c.rb"]);
        parts.write("/r", &files, facts, 0).unwrap();
        assert_eq!(keys(&parts), keys(&whole));
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
    fn refs_report_what_the_index_places_and_calls_by_file() {
        let mut store = Store::open_in_memory().unwrap();
        indexed(
            &mut store,
            "/a",
            "w.rb",
            "class Widget\n  def save\n  end\n  def go\n    save\n    other.save\n  end\nend\n",
        );
        let refs = store.refs("/a", "save").unwrap();
        let seen: Vec<_> = refs.iter().map(|r| (r.role.as_str(), r.line)).collect();
        assert_eq!(seen, [("definition", 2)]);
        // A call's place is its file's to say: the index keeps the file.
        assert!(store.calls_name("/a", "save").unwrap());
        assert_eq!(store.files_calling("/a", "save").unwrap(), ["w.rb"]);
        assert!(!store.calls_name("/other", "save").unwrap());
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

        let roots = Roots::of(vec!["/gem".to_string(), "/app".to_string()]);
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
        remove(&path);
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
        remove(&path);
    }

    /// A store and everything SQLite and the lock keep beside it.
    fn remove(path: &Path) {
        for suffix in ["", "-wal", "-shm", ".lock"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
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
        remove(&path);
    }

    #[test]
    fn an_older_writers_blob_insert_fails() {
        let store = Store::open_in_memory().unwrap();
        // 0.2's statement, then 0.3's: neither rechecks the schema.
        for insert in [
            "INSERT OR REPLACE INTO blob (oid, lines, parse_errors, surface) VALUES ('a', 1, 0, 0)",
            "INSERT INTO blob (oid, lines, parse_errors, surface) VALUES ('b', 1, 0, 0) \
             ON CONFLICT (oid) DO NOTHING",
        ] {
            assert!(store.conn.execute(insert, []).is_err(), "{insert}");
        }
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
        remove(&path);
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
        remove(&path);
        remove(&fresh);
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
