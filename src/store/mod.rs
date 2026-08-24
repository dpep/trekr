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
        if version == 0 {
            conn.execute_batch(schema::SCHEMA)?;
        } else {
            for (v, sql) in schema::MIGRATIONS {
                if version < v {
                    conn.execute_batch(sql)?;
                }
            }
        }
        if version != schema::VERSION {
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
            "INSERT OR IGNORE INTO checkout (root, indexed_at) VALUES (?1, unixepoch())",
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
        {
            let mut ids: HashMap<&Oid, i64> = HashMap::new();
            let mut lookup = tx.prepare("SELECT id FROM blob WHERE oid = ?1")?;
            let mut insert =
                tx.prepare("INSERT INTO file (checkout_id, path, blob_id) VALUES (?1, ?2, ?3)")?;
            for (path, oid) in files {
                let id = match ids.get(oid) {
                    Some(id) => *id,
                    None => {
                        let id: i64 = lookup.query_row(params![oid.0], |r| r.get(0))?;
                        ids.insert(oid, id);
                        id
                    }
                };
                insert.execute(params![checkout_id, path, id])?;
            }
            counts.blobs = ids.len();
        }

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

    /// Every definition in one checkout's file, in the order they are written.
    pub(crate) fn symbols(&self, root: &str, path: &str) -> Result<Vec<Symbol>> {
        let mut stmt = self.conn.prepare(
            "SELECT d.name, d.kind, d.nesting, d.singleton, d.visibility, d.params,
                    d.via, d.target, d.sig_returns, d.line, d.col, d.end_line
               FROM def d
               JOIN file f ON f.blob_id = d.blob_id
               JOIN checkout c ON c.id = f.checkout_id
              WHERE c.root = ?1 AND f.path = ?2
              ORDER BY d.line, d.col",
        )?;
        let rows = stmt.query_map(params![root, path], |r| {
            let params: String = r.get(5)?;
            Ok(Symbol {
                name: r.get(0)?,
                kind: r.get(1)?,
                nesting: split_nesting(&r.get::<_, String>(2)?),
                singleton: r.get::<_, i64>(3)? != 0,
                visibility: r.get(4)?,
                params: decode_params(&params)
                    .into_iter()
                    .map(|p| format!("{}:{}", p.kind.as_str(), p.name))
                    .collect(),
                via: r.get(6)?,
                target: r.get(7)?,
                sig_returns: r.get(8)?,
                line: r.get(9)?,
                col: r.get(10)?,
                end_line: r.get(11)?,
            })
        })?;
        rows.collect()
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
            })
        })?;
        rows.collect()
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
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct Symbol {
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

fn insert_facts(tx: &rusqlite::Transaction<'_>, oid: &Oid, facts: &Facts) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO blob (oid, lines, parse_errors) VALUES (?1, ?2, ?3)",
        params![oid.0, facts.lines as i64, facts.parse_errors as i64],
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
        "INSERT INTO ancestry (blob_id, nesting, relation, target, line, col)
         VALUES (?1,?2,?3,?4,?5,?6)",
    )?;
    for a in &facts.ancestry {
        ancestry.execute(params![
            blob_id,
            join_nesting(&a.nesting),
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
        "INSERT INTO call_site (blob_id, name, recv, recv_text, nesting, argc, block, line, col)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
    )?;
    for c in &facts.calls {
        call.execute(params![
            blob_id,
            c.name,
            c.recv.as_str(),
            c.recv_text,
            join_nesting(&c.nesting),
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

    #[test]
    fn symbols_come_back_in_line_order() {
        let mut store = Store::open_in_memory().unwrap();
        indexed(
            &mut store,
            "/a",
            "w.rb",
            "class Widget\n  def b\n  end\n  def a\n  end\nend\n",
        );
        let names: Vec<_> = store
            .symbols("/a", "w.rb")
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, ["Widget", "b", "a"], "an outline follows the source");
    }
}
