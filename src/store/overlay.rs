//! What a query reads in place of a checkout's map (DEC-035): the map as the
//! working tree has it, never written to the store.
//!
//! A copy of the whole map with the query's changes applied, in a database
//! of its own attached to the connection, behind a temporary view named
//! `file`. SQLite resolves `temp` before `main`, so every query reads the
//! view unchanged, and flattens a one-table view, so every query keeps its
//! plan; a `UNION ALL` view over the map cost no copy, but a join cannot
//! reach into one, and the tree build's edge query on mastodon went from 17
//! to 133 ms.
//!
//! The copy is a file beside the store, named by the checkouts it overlays
//! and keyed by everything it was built from. A connection attaches it only
//! when the key still answers, so a second connection, a second command, and
//! the next query over the same edits pay for none of the copy — and a store
//! that has moved since is never read through an old one. It is written once
//! and renamed into place, never in place, like a tree snapshot.

use super::{Digests, Overlay, Result, Store, path_hash};
use crate::core::Oid;
use rusqlite::{OptionalExtension, params};
use sha1::{Digest, Sha1};
use std::path::{Path, PathBuf};

/// The attached database's name on each connection.
const SCHEMA: &str = "overlay";

/// Moves when the copy's own tables change shape, so a file an older build
/// wrote is rebuilt rather than read.
const LAYOUT: u32 = 1;

/// One checkout's overlay on a connection.
#[derive(Clone)]
pub(crate) struct Overlaid {
    checkout_id: i64,
    /// How far it moves the checkout's surface and namespace keys.
    pub(super) shift: Digests,
    /// Each path at its blob, or absent for `None`.
    files: Overlay,
    /// The same, by the blob's row.
    rows: Vec<(String, Option<i64>)>,
}

/// Every checkout a connection overlays, by root.
pub(crate) type Overlays = std::collections::BTreeMap<String, Overlaid>;

impl Store {
    /// Answer as if `root`'s map held `files` — each path at its blob, or
    /// absent for `None` — on this connection only, until it closes or the
    /// overlay is replaced; empty takes it away. Nothing is written to the
    /// store. Every blob named must be in it already (`add_blob`). Whether
    /// the connection now answers differently than it did.
    pub(crate) fn overlay(&mut self, root: &str, files: &Overlay) -> Result<bool> {
        if self.overlaid.get(root).map(|o| &o.files) == Some(files) {
            return Ok(false);
        }
        if files.is_empty() {
            if self.overlaid.remove(root).is_none() {
                return Ok(false);
            }
            // The next query has nothing to resume (`resume`).
            if let Some(path) = self.overlay_file(&[root]) {
                let _ = std::fs::remove_file(path);
            }
        } else {
            let overlaid = self.overlaid_of(root, files)?;
            self.overlaid.insert(root.to_string(), overlaid);
        }
        if let Err(error) = self.shadow(true) {
            // Not half-applied: the map, and whatever else was overlaid.
            self.overlaid.remove(root);
            let _ = self.shadow(true);
            return Err(error);
        }
        Ok(true)
    }

    /// Overlay `root` as the last query that read it did, when the store has
    /// not moved since: the guess a query builds its tree over while it
    /// checks what changed (DEC-035). Never builds a copy; whether one was
    /// attached. A guess that no longer answers is removed.
    pub(crate) fn resume(&mut self, root: &str) -> Result<bool> {
        let Some(path) = self.overlay_file(&[root]) else {
            return Ok(false);
        };
        let Some(files) = remembered(&path, root) else {
            return Ok(false);
        };
        let overlaid = match self.overlaid_of(root, &files) {
            Ok(overlaid) => overlaid,
            // A blob it named is gone: a collection ran since.
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                let _ = std::fs::remove_file(path);
                return Ok(false);
            }
            Err(error) => return Err(error),
        };
        let before = self.overlaid.insert(root.to_string(), overlaid);
        if self.shadow(false).unwrap_or(false) {
            return Ok(true);
        }
        match before {
            Some(before) => self.overlaid.insert(root.to_string(), before),
            None => self.overlaid.remove(root),
        };
        let _ = std::fs::remove_file(path);
        self.shadow(true).map(|_| false)
    }

    /// The same overlays as `overlays`, from another connection's.
    pub(crate) fn adopt(&mut self, overlays: Overlays) -> Result<()> {
        if overlays.is_empty() {
            return Ok(());
        }
        self.overlaid = overlays;
        self.shadow(true).map(drop)
    }

    /// Remove the copy that `root`'s next query would resume, for `--drop`.
    pub(crate) fn forget_overlay(&self, root: &str) {
        if let Some(path) = self.overlay_file(&[root]) {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Remove every copy, for `--gc`: each is only a guess and a cache, and
    /// one a dropped checkout or a past `--dead` across two left is read by
    /// nothing. The files and bytes removed, or — on a dry run — that would be.
    pub(crate) fn sweep_overlays(&self, dry_run: bool) -> (usize, u64) {
        let Some(dir) = self.overlay_dir() else {
            return (0, 0);
        };
        let mut swept = (0, 0);
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            swept.0 += 1;
            swept.1 += entry.metadata().map_or(0, |m| m.len());
            if !dry_run {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        swept
    }

    /// What this connection overlays, for another connection to `adopt`.
    pub(crate) fn overlays(&self) -> Overlays {
        self.overlaid.clone()
    }

    /// Whether an overlay on this connection moves a checkout's namespace —
    /// a class, module or constant edited — rather than only its methods.
    pub(crate) fn overlays_namespace(&self) -> bool {
        self.overlaid.values().any(|o| o.shift.1 != 0)
    }

    fn overlaid_of(&self, root: &str, files: &Overlay) -> Result<Overlaid> {
        let checkout_id: i64 = self.conn.query_row(
            "SELECT id FROM checkout WHERE root = ?1",
            params![root],
            |r| r.get(0),
        )?;
        let mut shift = Digests::default();
        let mut rows = Vec::with_capacity(files.len());
        for (path, oid) in files {
            let hashed = path_hash(path);
            let old: Option<Digests> = self
                .conn
                .query_row(
                    "SELECT b.surface, b.namespace FROM main.file f JOIN blob b ON b.id = f.blob_id
                      WHERE f.checkout_id = ?1 AND f.path = ?2",
                    params![checkout_id, path],
                    |r| Ok(Digests(r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some(old) = old {
                shift = shift.sub(hashed, old);
            }
            let blob_id = match oid {
                Some(oid) => {
                    let (id, new) = self.conn.query_row(
                        "SELECT id, surface, namespace FROM blob WHERE oid = ?1",
                        params![oid.0],
                        |r| Ok((r.get::<_, i64>(0)?, Digests(r.get(1)?, r.get(2)?))),
                    )?;
                    shift = shift.add(hashed, new);
                    Some(id)
                }
                None => None,
            };
            rows.push((path.clone(), blob_id));
        }
        Ok(Overlaid {
            checkout_id,
            shift,
            files: files.clone(),
            rows,
        })
    }

    /// Put `file` in front of the map as `overlaid` says, replacing what was
    /// there. Attaches the copy whose key answers, or — when `build` — makes
    /// one; whether `file` is now the overlay.
    fn shadow(&mut self, build: bool) -> Result<bool> {
        self.unshadow();
        if self.overlaid.is_empty() {
            return Ok(true);
        }
        let key = self.overlay_key()?;
        let roots: Vec<&str> = self.overlaid.keys().map(String::as_str).collect();
        let attached = match self.overlay_file(&roots) {
            Some(path) => {
                if self.attach(&path, &key) {
                    true
                } else if build {
                    // Somewhere it can be written, or else on this
                    // connection alone.
                    self.publish(&path, &key) || self.fill_in_memory()?
                } else {
                    false
                }
            }
            None => build && self.fill_in_memory()?,
        };
        if attached {
            self.conn.execute_batch(&format!(
                "CREATE TEMP VIEW file AS SELECT checkout_id, path, blob_id FROM {SCHEMA}.file;"
            ))?;
        }
        Ok(attached)
    }

    /// Take the overlay away: `file` is the map again.
    pub(super) fn unshadow(&mut self) {
        let _ = self.conn.execute_batch("DROP VIEW IF EXISTS temp.file;");
        let _ = self.conn.execute_batch(&format!("DETACH {SCHEMA};"));
    }

    /// Everything the copy is a function of: the map of every checkout —
    /// which an index moves, `indexed_at` and `map_key` with it — and each
    /// overlaid path's blob row. A blob's row does not change while a map
    /// names it, so these pin the copy's every row.
    fn overlay_key(&self) -> Result<String> {
        let mut hash = Sha1::new();
        let mut eat = |bytes: &[u8]| {
            hash.update((bytes.len() as u64).to_le_bytes());
            hash.update(bytes);
        };
        eat(&LAYOUT.to_le_bytes());
        eat(&self.schema_version()?.to_le_bytes());
        let mut stmt = self
            .conn
            .prepare_cached("SELECT id, root, map_key, indexed_at FROM checkout ORDER BY id")?;
        let checkouts = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        for checkout in checkouts {
            let (id, root, map_key, indexed_at) = checkout?;
            eat(&id.to_le_bytes());
            eat(root.as_bytes());
            eat(&map_key.to_le_bytes());
            eat(&indexed_at.to_le_bytes());
        }
        for (root, overlaid) in &self.overlaid {
            eat(root.as_bytes());
            for (path, blob_id) in &overlaid.rows {
                eat(path.as_bytes());
                eat(&blob_id.unwrap_or(-1).to_le_bytes());
            }
        }
        Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
    }

    /// `trekr.db` keeps its copies in `trekr.overlays/`. `None` for an
    /// in-memory store.
    fn overlay_dir(&self) -> Option<PathBuf> {
        Some(self.path()?.with_extension("overlays"))
    }

    /// Where the copy for these checkouts lives.
    fn overlay_file(&self, roots: &[&str]) -> Option<PathBuf> {
        let dir = self.overlay_dir()?;
        let mut hash = Sha1::new();
        for root in roots {
            hash.update(root.as_bytes());
            hash.update([0]);
        }
        let name: String = hash.finalize()[..8]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        Some(dir.join(format!("{name}.db")))
    }

    /// Attach the copy at `path` read-only if it answers to `key`.
    fn attach(&self, path: &Path, key: &str) -> bool {
        if !path.exists() {
            return false;
        }
        let uri = format!("file:{}?mode=ro", uri_path(path));
        if self
            .conn
            .execute(&format!("ATTACH ?1 AS {SCHEMA}"), params![uri])
            .is_err()
        {
            return false;
        }
        let found: Option<String> = self
            .conn
            .query_row(&format!("SELECT key FROM {SCHEMA}.meta"), [], |r| r.get(0))
            .ok();
        if found.as_deref() == Some(key) {
            return true;
        }
        let _ = self.conn.execute_batch(&format!("DETACH {SCHEMA};"));
        false
    }

    /// Write the copy to a temporary name, rename it to `path`, and attach
    /// it if it answers to `key`. Best effort: false when it could not be
    /// written, or a write since `key` moved the map it copied.
    fn publish(&mut self, path: &Path, key: &str) -> bool {
        let Some(dir) = path.parent() else {
            return false;
        };
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let temp = path.with_extension(format!("{}.{nanos}.tmp", std::process::id()));
        let written = std::fs::create_dir_all(dir).is_ok()
            && self
                .conn
                .execute(
                    &format!("ATTACH ?1 AS {SCHEMA}"),
                    params![temp.to_string_lossy()],
                )
                .is_ok()
            && {
                // A cache: a torn file fails its key or its parse, and is
                // rebuilt.
                let filled = self
                    .conn
                    .execute_batch(&format!(
                        "PRAGMA {SCHEMA}.journal_mode = OFF; PRAGMA {SCHEMA}.synchronous = OFF;"
                    ))
                    .and_then(|()| self.fill());
                let detached = self.conn.execute_batch(&format!("DETACH {SCHEMA};"));
                filled.is_ok() && detached.is_ok()
            }
            && std::fs::rename(&temp, path).is_ok();
        if !written {
            let _ = std::fs::remove_file(&temp);
            return false;
        }
        self.attach(path, key)
    }

    /// Make the copy in memory, on this connection alone.
    fn fill_in_memory(&mut self) -> Result<bool> {
        self.conn
            .execute_batch(&format!("ATTACH ':memory:' AS {SCHEMA};"))?;
        self.fill()?;
        Ok(true)
    }

    /// The map with every overlay applied, its index, its statistics — the
    /// plans the map's own choose, not the planner's guesses — and what it
    /// was made from, into the attached database. The key is read in the
    /// transaction that copies the map, so a write between the two cannot
    /// label one map with another's key.
    fn fill(&self) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let key = self.overlay_key()?;
        tx.execute_batch(&format!(
            "CREATE TABLE {SCHEMA}.file (
               checkout_id INTEGER NOT NULL,
               path        TEXT    NOT NULL,
               blob_id     INTEGER NOT NULL,
               PRIMARY KEY (checkout_id, path)
             ) WITHOUT ROWID;
             INSERT INTO {SCHEMA}.file SELECT checkout_id, path, blob_id FROM main.file;
             CREATE TABLE {SCHEMA}.meta (key TEXT NOT NULL);
             CREATE TABLE {SCHEMA}.change (root TEXT NOT NULL, path TEXT NOT NULL, oid TEXT);"
        ))?;
        {
            let mut put = tx.prepare(&format!(
                "INSERT OR REPLACE INTO {SCHEMA}.file (checkout_id, path, blob_id) VALUES (?1, ?2, ?3)"
            ))?;
            let mut take = tx.prepare(&format!(
                "DELETE FROM {SCHEMA}.file WHERE checkout_id = ?1 AND path = ?2"
            ))?;
            let mut said = tx.prepare(&format!(
                "INSERT INTO {SCHEMA}.change (root, path, oid) VALUES (?1, ?2, ?3)"
            ))?;
            for (root, overlaid) in &self.overlaid {
                for (path, blob_id) in &overlaid.rows {
                    match blob_id {
                        Some(blob_id) => {
                            put.execute(params![overlaid.checkout_id, path, blob_id])?
                        }
                        None => take.execute(params![overlaid.checkout_id, path])?,
                    };
                }
                for (path, oid) in &overlaid.files {
                    said.execute(params![root, path, oid.as_ref().map(|o| &o.0)])?;
                }
            }
            tx.execute(
                &format!("INSERT INTO {SCHEMA}.meta (key) VALUES (?1)"),
                params![key],
            )?;
        }
        tx.execute_batch(&format!(
            "CREATE INDEX {SCHEMA}.file_blob ON file(blob_id); ANALYZE {SCHEMA};"
        ))?;
        tx.commit()
    }
}

/// The overlay the copy at `path` was made with for `root`, when it is one.
fn remembered(path: &Path, root: &str) -> Option<Overlay> {
    let conn =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()?;
    let mut stmt = conn
        .prepare("SELECT path, oid FROM change WHERE root = ?1 ORDER BY rowid")
        .ok()?;
    let rows = stmt
        .query_map(params![root], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?.map(Oid),
            ))
        })
        .ok()?;
    let files: Overlay = rows.collect::<Result<_>>().ok()?;
    (!files.is_empty()).then_some(files)
}

/// `path` as a `file:` URI's path: every byte but a URI's unreserved ones
/// percent-escaped, which SQLite decodes back to the same bytes — a name's
/// UTF-8 included.
fn uri_path(path: &Path) -> String {
    use std::fmt::Write;
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::new();
    for &byte in path.as_os_str().as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(byte as char)
            }
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "/repo";

    /// A store on disk holding one checkout of `widget.rb`, and the blob of
    /// an edit to it, recorded but mapped by nothing.
    fn store(name: &str) -> (PathBuf, Store, Oid) {
        let dir = std::env::temp_dir().join(format!("trekr-overlay-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut store = Store::open(&dir.join("t.db")).unwrap();
        index(&mut store, "class Widget\nend\n");
        let edit = "class Widget\n  def fresh\n  end\nend\n";
        let oid = crate::scan::hash_blob(edit.as_bytes());
        store
            .add_blob(&oid, &crate::extract::extract(edit.as_bytes()))
            .unwrap();
        (dir, store, oid)
    }

    fn index(store: &mut Store, source: &str) {
        let oid = crate::scan::hash_blob(source.as_bytes());
        let mut files = crate::scan::Files::new();
        files.insert("widget.rb".to_string(), oid.clone());
        let facts = match store.has_blob(&oid).unwrap() {
            true => Vec::new(),
            false => vec![(oid, crate::extract::extract(source.as_bytes()))],
        };
        store.write(ROOT, &files, facts, 0).unwrap();
    }

    /// Whether `widget.rb` defines `fresh`, as this connection reads it.
    fn fresh(store: &Store) -> bool {
        let roots = crate::store::Roots::of(vec![ROOT.to_string()]);
        !store.methods_named(&roots, "fresh").unwrap().is_empty()
    }

    fn copies(store: &Store) -> Vec<PathBuf> {
        let dir = store.path().unwrap().with_extension("overlays");
        std::fs::read_dir(dir)
            .map(|e| e.flatten().map(|e| e.path()).collect())
            .unwrap_or_default()
    }

    /// One copy, written once: a second connection and the next command
    /// attach it rather than make their own.
    #[test]
    fn an_overlay_is_copied_once_and_attached_after() {
        let (dir, mut store, oid) = store("once");
        let edit = Overlay::from_iter([("widget.rb".to_string(), Some(oid))]);
        assert!(!fresh(&store));
        assert!(store.overlay(ROOT, &edit).unwrap());
        assert!(fresh(&store));
        assert!(
            !store.overlay(ROOT, &edit).unwrap(),
            "the same overlay again"
        );
        let made = copies(&store);
        assert_eq!(made.len(), 1, "{made:?}");
        let written = std::fs::metadata(&made[0]).unwrap().modified().unwrap();

        let other = store.reopen().unwrap().unwrap();
        assert!(fresh(&other), "a second connection answers the same");
        let mut later = Store::open(store.path().unwrap()).unwrap();
        assert!(later.overlay(ROOT, &edit).unwrap());
        assert!(fresh(&later));
        assert_eq!(copies(&store), made);
        assert_eq!(
            std::fs::metadata(&made[0]).unwrap().modified().unwrap(),
            written,
            "attached, not rewritten"
        );
        assert!(
            !fresh(&Store::open(store.path().unwrap()).unwrap()),
            "the map is untouched"
        );

        // Taken away, the map answers again, and nothing is left to resume.
        assert!(store.overlay(ROOT, &Overlay::default()).unwrap());
        assert!(!fresh(&store));
        assert!(copies(&store).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The last overlay is a guess the next command may build over — until
    /// the store moves, when it is dropped rather than read.
    #[test]
    fn a_resumed_overlay_answers_only_while_the_store_has_not_moved() {
        let (dir, mut store, oid) = store("resume");
        store
            .overlay(
                ROOT,
                &Overlay::from_iter([("widget.rb".to_string(), Some(oid))]),
            )
            .unwrap();
        let path = store.path().unwrap().to_path_buf();
        drop(store);

        let mut next = Store::open(&path).unwrap();
        assert!(next.resume(ROOT).unwrap());
        assert!(fresh(&next));
        drop(next);

        let mut store = Store::open(&path).unwrap();
        index(&mut store, "class Widget\n  def other\n  end\nend\n");
        let mut next = Store::open(&path).unwrap();
        assert!(!next.resume(ROOT).unwrap());
        assert!(!fresh(&next));
        assert!(
            copies(&next).is_empty(),
            "a guess that no longer answers is removed"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A copy is keyed by the map it copied: a write landing between the
    /// key a connection asked for and the copy cannot file the new map
    /// under the old key.
    #[test]
    fn a_copy_is_keyed_by_the_map_it_holds() {
        let (dir, mut store, oid) = store("keyed");
        let overlaid = store
            .overlaid_of(
                ROOT,
                &Overlay::from_iter([("widget.rb".to_string(), Some(oid))]),
            )
            .unwrap();
        store.overlaid.insert(ROOT.to_string(), overlaid);
        let asked = store.overlay_key().unwrap();
        let mut other = Store::open(store.path().unwrap()).unwrap();
        index(&mut other, "class Widget\n  def other\n  end\nend\n");

        let path = store.overlay_file(&[ROOT]).unwrap();
        assert!(!store.publish(&path, &asked), "the key asked for moved");
        let conn = rusqlite::Connection::open(&path).unwrap();
        let written: String = conn
            .query_row("SELECT key FROM meta", [], |r| r.get(0))
            .unwrap();
        assert_ne!(written, asked);
        assert_eq!(written, store.overlay_key().unwrap());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A store under a directory whose name is not ASCII attaches its copy
    /// as any other does, rather than writing a new one every connection.
    #[test]
    fn a_copy_beside_a_non_ascii_store_is_attached_not_rewritten() {
        use std::os::unix::fs::MetadataExt;
        let (dir, mut store, oid) = store("caf\u{e9} d%r");
        let edit = Overlay::from_iter([("widget.rb".to_string(), Some(oid))]);
        assert!(store.overlay(ROOT, &edit).unwrap());
        let made = copies(&store);
        assert_eq!(made.len(), 1, "{made:?}");
        let inode = std::fs::metadata(&made[0]).unwrap().ino();

        let mut later = Store::open(store.path().unwrap()).unwrap();
        assert!(later.overlay(ROOT, &edit).unwrap());
        assert!(fresh(&later));
        assert_eq!(copies(&store), made);
        assert_eq!(
            std::fs::metadata(&made[0]).unwrap().ino(),
            inode,
            "attached, not rewritten"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Gathered from a `HashMap`, the same edits come in any order; as an
    /// overlay they are one, or no process resumes another's copy.
    #[test]
    fn an_overlay_is_the_same_in_any_order_it_was_gathered() {
        let oid = |s: &str| Some(Oid(s.to_string()));
        let one = Overlay::from_iter([
            ("b.rb".to_string(), None),
            ("a.rb".to_string(), oid("1")),
            ("c.rb".to_string(), None),
        ]);
        let other = Overlay::from_iter([
            ("c.rb".to_string(), None),
            ("b.rb".to_string(), None),
            ("a.rb".to_string(), oid("1")),
            ("a.rb".to_string(), oid("2")),
        ]);
        assert_eq!(one, other, "sorted, one entry per path, the first kept");
        let paths: Vec<&str> = one.iter().map(|(path, _)| path.as_str()).collect();
        assert_eq!(paths, ["a.rb", "b.rb", "c.rb"]);
    }

    #[test]
    fn a_uri_path_escapes_all_but_the_unreserved() {
        for (path, uri) in [
            ("/a/b-c_d.e~f/t.db", "/a/b-c_d.e~f/t.db"),
            ("/we ird/%#?.db", "/we%20ird/%25%23%3F.db"),
            ("/caf\u{e9}/t.db", "/caf%C3%A9/t.db"),
        ] {
            assert_eq!(uri_path(Path::new(path)), uri, "{path}");
        }
    }

    /// An in-memory store has nowhere to keep a copy, and makes its own.
    #[test]
    fn an_in_memory_store_overlays_on_its_connection() {
        let mut store = Store::open_in_memory().unwrap();
        index(&mut store, "class Widget\nend\n");
        let edit = "class Widget\n  def fresh\n  end\nend\n";
        let oid = crate::scan::hash_blob(edit.as_bytes());
        store
            .add_blob(&oid, &crate::extract::extract(edit.as_bytes()))
            .unwrap();
        assert!(
            store
                .overlay(
                    ROOT,
                    &Overlay::from_iter([("widget.rb".to_string(), Some(oid))])
                )
                .unwrap()
        );
        assert!(fresh(&store));
        assert!(!store.resume(ROOT).unwrap());
        assert!(
            store
                .overlay(ROOT, &Overlay::from_iter([("widget.rb".to_string(), None)]))
                .unwrap()
        );
        assert!(
            !store.maps(ROOT, "widget.rb").unwrap(),
            "a deleted file is gone"
        );
    }
}
