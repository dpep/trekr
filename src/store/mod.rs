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
use rusqlite::{Connection, Result, params};
use std::collections::{HashMap, HashSet};
use std::path::Path;

pub(crate) struct Store {
    conn: Connection,
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
        Store::init(Connection::open(path)?)
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
        Ok(Store { conn })
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
            "INSERT OR IGNORE INTO checkout (root, indexed_at, surface_key)
             VALUES (?1, unixepoch(), 0)",
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
            "UPDATE checkout SET surface_key = ?2 WHERE id = ?1",
            params![checkout_id, surface_key],
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
    pub(crate) fn declarations(&self, roots: &[String]) -> Result<Vec<DeclRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT d.name, d.kind, d.nesting, d.target, f.path, d.line, d.col
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
        let mut stmt = self.conn.prepare(&format!(
            "SELECT d.name, d.nesting, d.singleton, d.visibility, d.params, d.via,
                    d.target, d.sig_returns, f.path, d.line, d.col
               FROM def d
               JOIN file f ON f.blob_id = d.blob_id
               JOIN checkout c ON c.id = f.checkout_id
              WHERE c.root IN ({}) AND d.kind = 'method'
              ORDER BY c.id, f.path, d.line, d.col",
            placeholders(roots.len())
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(roots), |r| {
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

    /// Has this root been indexed before?
    ///
    /// For a gem this is the whole incremental story: a gem's bytes never
    /// change, so having seen it once is having seen it.
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

    fn indexed(store: &mut Store, root: &str, path: &str, src: &str) -> Indexed {
        let oid = crate::scan::hash_blob(src.as_bytes());
        let files = Files::from([(path.to_string(), oid.clone())]);
        let wanted = HashSet::from([oid.clone()]);
        let known = store.known(&wanted).unwrap();
        let facts = if known.contains(&oid) {
            Vec::new()
        } else {
            vec![(oid, crate::extract::extract(src.as_bytes()))]
        };
        store.write(root, &files, facts).unwrap()
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
