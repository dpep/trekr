//! SQLite, WAL, and nothing clever.
//!
//! Facts are keyed by blob OID, so two worktrees of one repo store one copy and
//! a branch switch reparses only what is genuinely new. The store's job is to
//! make that diff cheap and to stay out of the way otherwise.
//!
//! Conventions (pragmas, `user_version` as the migration marker, `$TREKR_DB`)
//! follow rq's `src/store/`.

mod schema;

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

/// Ruby core, written out beside the database as a real readable file.
///
/// The stub is compiled into the binary, so a definition in it had no location
/// to point at and every `require` or `Array#each` answered nothing — worse
/// than ruby-lsp, which at least sends you to an RBS declaration. Writing it
/// once means "go to definition" lands on a signature a person can read, and
/// the file says in its header what it is.
pub(crate) fn core_stub_path() -> anyhow::Result<std::path::PathBuf> {
    let path = default_path()?
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join("core.rb");
    let wanted = include_str!("../tree/core.rb");
    // Rewrite only when it differs, so an editor watching the file is not
    // churned on every index.
    let current = std::fs::read_to_string(&path).ok();
    if current.as_deref() != Some(wanted) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, wanted)?;
    }
    Ok(path)
}

/// The database every command uses.
pub(crate) fn open_default() -> anyhow::Result<Store> {
    let path = default_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(Store::open(&path)?)
}

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

    #[cfg(test)]
    pub(crate) fn open_in_memory() -> Result<Store> {
        Store::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Store> {
        // WAL lets a reader answer while an indexer writes; busy_timeout makes
        // a second writer wait rather than fail.
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000; \
             PRAGMA synchronous=NORMAL; PRAGMA temp_store=MEMORY; PRAGMA cache_size=-32768;",
        )?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        // An *older* binary must not drop a newer database. Two trekrs on one
        // machine — one installed, one freshly built — would otherwise take
        // turns wiping each other's index, and each would look like it had
        // simply never been run.
        if version > schema::VERSION {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_MISMATCH),
                Some(format!(
                    "database is schema v{version} but this trekr speaks v{};                      upgrade trekr, or point $TREKR_DB elsewhere",
                    schema::VERSION
                )),
            ));
        }
        if version != schema::VERSION {
            // No migration, by design: see schema::VERSION. Reindexing costs
            // seconds and cannot leave the store half-converted.
            if version != 0 {
                conn.execute_batch("PRAGMA foreign_keys=OFF;")?;
                for table in schema::TABLES {
                    conn.execute_batch(&format!("DROP TABLE IF EXISTS {table};"))?;
                }
                conn.execute_batch("PRAGMA foreign_keys=ON;")?;
            }
            conn.execute_batch(schema::SCHEMA)?;
            conn.pragma_update(None, "user_version", schema::VERSION)?;
        }
        Ok(Store { conn, path: None })
    }

    /// Blob OIDs this machine has already read, of the ones asked about.
    ///
    /// Loaded whole rather than probed per OID: at 100k blobs it is a few MB
    /// and one query, where the probe is 100k round trips.
    pub(crate) fn known(&self, wanted: &HashSet<Oid>) -> Result<HashSet<Oid>> {
        let mut stmt = self.conn.prepare("SELECT oid FROM blob")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut known = HashSet::new();
        for oid in rows {
            let oid = Oid(oid?);
            if wanted.contains(&oid) {
                known.insert(oid);
            }
        }
        Ok(known)
    }

    /// Record one checkout's file map and any facts it brought with it.
    ///
    /// One transaction: an interrupted index leaves the previous state intact
    /// rather than a half-mapped checkout.
    pub(crate) fn write(
        &mut self,
        root: &str,
        files: &Files,
        facts: Vec<(Oid, Facts)>,
        git_state: i64,
    ) -> Result<Indexed> {
        let tx = self.conn.transaction()?;
        let mut counts = Indexed {
            files: files.len(),
            parsed: facts.len(),
            ..Indexed::default()
        };

        for (oid, f) in &facts {
            counts.defs += f.defs.len();
            counts.refs += f.const_refs.len();
            counts.calls += f.calls.len();
            insert_facts(&tx, oid, f)?;
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
        let map_key = files.iter().fold(0i64, |key, (path, oid)| {
            key.wrapping_add(path_hash(path) ^ path_hash(&oid.0))
        });
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

        // The map is rewritten wholesale. It is one row per file, and a
        // delta would have to be right about deletes and renames to save a
        // few milliseconds.
        tx.execute(
            "DELETE FROM file WHERE checkout_id = ?1",
            params![checkout_id],
        )?;
        let mut surface_key: i64 = 0;
        {
            let mut ids: HashMap<&Oid, (i64, i64)> = HashMap::new();
            let mut lookup = tx.prepare("SELECT id, surface FROM blob WHERE oid = ?1")?;
            let mut insert =
                tx.prepare("INSERT INTO file (checkout_id, path, blob_id) VALUES (?1, ?2, ?3)")?;
            for (path, oid) in files {
                let (id, surface) = match ids.get(oid) {
                    Some(found) => *found,
                    None => {
                        let found = lookup.query_row(params![oid.0], |r| {
                            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
                        })?;
                        ids.insert(oid, found);
                        found
                    }
                };
                insert.execute(params![checkout_id, path, id])?;
                // Order-independent, so the map's iteration order cannot
                // change the key; the path is mixed in because a rename moves
                // where an answer points even when no blob changed.
                surface_key = surface_key.wrapping_add(path_hash(path) ^ surface);
            }
            counts.blobs = ids.len();
        }

        tx.execute(
            "UPDATE checkout SET surface_key = ?2, map_key = ?3, git_state = ?4 WHERE id = ?1",
            params![checkout_id, surface_key, map_key, git_state],
        )?;

        tx.commit()?;
        Ok(counts)
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
        let mut stmt = self.conn.prepare(&format!(
            "SELECT d.name, d.kind, d.nesting, d.target, c.root || '/' || f.path, d.line, d.col
               FROM def d
               JOIN file f ON f.blob_id = d.blob_id
               JOIN checkout c ON c.id = f.checkout_id
              WHERE c.root IN ({}) AND d.kind IN ('class','module','constant')
              ORDER BY c.id, f.path, d.line, d.col",
            placeholders(roots.len())
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(roots), |r| {
            Ok(DeclRow {
                name: r.get(0)?,
                kind: r.get(1)?,
                nesting: split_nesting(&r.get::<_, String>(2)?),
                target: r.get(3)?,
                path: r.get(4)?,
                line: r.get(5)?,
                col: r.get(6)?,
            })
        })?;
        rows.collect()
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

    fn method_rows(&self, roots: &[String], name: Option<&str>) -> Result<Vec<MethodRow>> {
        // Insert order is load-bearing: `lookup` takes the last definition, so
        // a reopened class must arrive after the class it reopens.
        let filter = if name.is_some() { "AND d.name = ?" } else { "" };
        let mut stmt = self.conn.prepare(&format!(
            "SELECT d.name, d.nesting, d.singleton, d.visibility, d.params, d.via,
                    d.target, d.sig_returns, c.root || '/' || f.path, d.line, d.col
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
        let rows = stmt.query_map(values.as_slice(), |r| {
            let params: String = r.get(4)?;
            Ok(MethodRow {
                name: r.get(0)?,
                nesting: split_nesting(&r.get::<_, String>(1)?),
                singleton: r.get::<_, i64>(2)? != 0,
                visibility: r.get(3)?,
                params: decode_params(&params),
                via: r.get(5)?,
                target: r.get(6)?,
                sig_returns: r.get(7)?,
                path: r.get(8)?,
                line: r.get(9)?,
                col: r.get(10)?,
            })
        })?;
        rows.collect()
    }

    /// Every ancestry edge in a checkout, in source order — which is the order
    /// Ruby applies them in, and therefore the order linearization reverses.
    pub(crate) fn ancestry(&self, roots: &[String]) -> Result<Vec<EdgeRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT a.owner, a.relation, a.target
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
    pub(crate) fn files_calling(&self, root: &str, name: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT f.path
               FROM call_site s
               JOIN file f ON f.blob_id = s.blob_id
               JOIN checkout c ON c.id = f.checkout_id
              WHERE c.root = ?1 AND s.name = ?2
              ORDER BY f.path",
        )?;
        let rows = stmt.query_map(params![root, name], |r| r.get(0))?;
        rows.collect()
    }

    /// Definitions whose name contains `query`, for `workspaceSymbol`.
    ///
    /// Substring, case-insensitive, capped. rq's scorer would rank these
    /// better; this is the LSP contract's shape, and a client that wants
    /// ranking can ask rq (PLAN §3).
    ///
    /// `root` of `None` searches every checkout — for a client whose workspace
    /// is not one of them, where the alternative is answering nothing.
    /// How often each of these names appears as a call site anywhere, split by
    /// whether it was written as a call or handed to a macro as a symbol.
    ///
    /// The cheap half of the dead-code filter (DEC-038). A name with hundreds
    /// of call sites is not a candidate and must never cost a receiver-narrowed
    /// pass to find that out; a name with none or a few is worth the expensive
    /// question. Counting by name is deliberately *generous* — it counts every
    /// same-named call in the index — because over-counting costs a missed
    /// candidate and under-counting costs a false "nothing uses this".
    pub(crate) fn mention_counts(&self, names: &[String]) -> Result<HashMap<String, (i64, i64)>> {
        let mut found = HashMap::new();
        // Chunked: SQLite's parameter limit is smaller than a big file's
        // method count.
        for chunk in names.chunks(400) {
            let holes = vec!["?"; chunk.len()].join(",");
            let sql = format!(
                "SELECT name,
                        SUM(CASE WHEN recv = 'symbol' THEN 0 ELSE 1 END),
                        SUM(CASE WHEN recv = 'symbol' THEN 1 ELSE 0 END)
                   FROM call_site WHERE name IN ({holes}) GROUP BY name"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(chunk), |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?;
            for row in rows {
                let (name, written, symbol) = row?;
                found.insert(name, (written, symbol));
            }
        }
        Ok(found)
    }

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
        self.conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
    }

    /// The checkout's whole file map, folded into one number at index time.
    ///
    /// The other half of a resident front's staleness check, and the reason it
    /// is a *content* key: the file **count** does not move when a file is
    /// edited, so a session keyed on it went on answering from a tree
    /// assembled before the edit. This moves whenever any answer would.
    pub(crate) fn surface_key(&self, root: &str) -> Result<i64> {
        self.conn
            .query_row(
                "SELECT surface_key FROM checkout WHERE root = ?1",
                params![root],
                |r| r.get(0),
            )
            .or(Ok(0))
    }

    /// Record that this checkout's bundle resolves these gems.
    ///
    /// Rewritten wholesale on every index, like the file map, so a gem dropped
    /// from a Gemfile.lock stops being claimed.
    pub(crate) fn set_gems_used(&mut self, root: &str, gem_roots: &[String]) -> Result<()> {
        let tx = self.conn.transaction()?;
        let id: i64 = tx.query_row(
            "SELECT id FROM checkout WHERE root = ?1",
            params![root],
            |r| r.get(0),
        )?;
        tx.execute("DELETE FROM gem_use WHERE checkout_id = ?1", params![id])?;
        {
            let mut insert = tx
                .prepare("INSERT OR IGNORE INTO gem_use (checkout_id, gem_root) VALUES (?1, ?2)")?;
            for gem in gem_roots {
                insert.execute(params![id, gem])?;
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
    /// Returns whether anything actually changed. Both keys are updated
    /// incrementally: they are order-independent folds of one XOR term per
    /// file, so removing the old term and adding the new one is exact rather
    /// than an approximation of a full re-fold.
    pub(crate) fn refresh_file(
        &mut self,
        root: &str,
        relative: &str,
        oid: &Oid,
        facts: Option<&Facts>,
    ) -> Result<bool> {
        let tx = self.conn.transaction()?;
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
    /// Best effort, and only worth it when something was actually written: it
    /// is ~3 s on a 384 MB database, which is fine once per index and not fine
    /// on a no-op reindex.
    pub(crate) fn analyze(&self) {
        let _ = self.conn.execute_batch("ANALYZE;");
    }
}

impl Drop for Store {
    /// `PRAGMA optimize` runs `ANALYZE` on tables whose size has moved enough
    /// to matter, and does nothing otherwise. Without statistics SQLite plans
    /// `refs` as a nested scan of the checkout's files — 90 s for a name as
    /// common as `new`, against 45 ms with them. Best effort: a failure here
    /// must not fail a command that already produced its answer.
    fn drop(&mut self) {
        let _ = self.conn.execute_batch("PRAGMA optimize;");
    }
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
    pub(crate) path: String,
    pub(crate) line: u32,
    pub(crate) col: u32,
}

#[derive(Debug)]
pub(crate) struct EdgeRow {
    /// Scope stack including the receiving class or module, innermost first.
    pub(crate) owner: Vec<String>,
    pub(crate) relation: String,
    pub(crate) target: String,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) recv: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
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
    /// Empty for `--symbols`, which already knows the file it asked about.
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
fn path_hash(path: &str) -> i64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash as i64
}

fn insert_facts(tx: &rusqlite::Transaction<'_>, oid: &Oid, facts: &Facts) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO blob (oid, lines, parse_errors, surface)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            oid.0,
            facts.lines as i64,
            facts.parse_errors as i64,
            facts.surface() as i64
        ],
    )?;
    let blob_id = tx.last_insert_rowid();

    let mut def = tx.prepare_cached(
        "INSERT INTO def (blob_id, name, kind, nesting, singleton, visibility, params,
                          via, target, sig_returns, line, col, end_line)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
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
        let wanted = HashSet::from([oid.clone()]);
        let known = store.known(&wanted).unwrap();
        let facts = if known.contains(&oid) {
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
