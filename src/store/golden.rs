//! The guard on "bump the store version when extraction changes"
//! (`schema::VERSION`'s second half). Facts are cached by blob OID, so an
//! extractor change shipped without a bump is dead on every blob already
//! known — and nothing failed to say so.
//!
//! Every testbed source is extracted and written as the store writes it, and
//! its rows hash to the line `tests/extraction.golden` recorded for it, at the
//! version recorded there. An input the golden has not seen yet fails too,
//! asking only for a regeneration: adding a case needs no bump, but an
//! unrecorded input is an unguarded one. It sees only what the testbed
//! exercises.

use super::{Store, insert_facts, schema};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const GOLDEN: &str = "tests/extraction.golden";
const REGENERATE: &str = "UPDATE_GOLDEN=1 cargo test --lib extraction_matches_its_golden";

/// Which extractor reads a file, by the name the golden records it under.
fn kind(path: &str) -> Option<&'static str> {
    use crate::scan::Reader;
    crate::scan::is_indexed(path).then(|| match Reader::of(path) {
        Reader::Ruby => "ruby",
        Reader::Erb => "erb",
        Reader::Rabl => "rabl",
        Reader::StructureSql => "sql",
    })
}

fn sources(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("readable").flatten() {
        let path = entry.path();
        if path.is_dir() {
            sources(&path, found);
        } else {
            found.push(path);
        }
    }
}

/// FNV-1a: stable across Rust releases, unlike `DefaultHasher`.
fn fnv(lines: &[String]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in lines.iter().flat_map(|line| line.bytes().chain(*b"\n")) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// Each blob's rows as the store keeps them, as sorted text: every column
/// of every fact table but the row ids and `written_by`, which is the
/// version itself. Keyed by oid.
fn stored(files: &[(String, Vec<u8>)]) -> BTreeMap<String, Vec<String>> {
    let store = Store::open_in_memory().expect("an in-memory store");
    for (path, bytes) in files {
        let facts = crate::extract::extract_file(path, bytes);
        insert_facts(&store.conn, &crate::scan::hash_blob(bytes), &facts).expect("facts insert");
    }
    let mut rows: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for table in fact_tables(&store) {
        let oid = match table.as_str() {
            "blob" => "t.oid",
            _ => "(SELECT oid FROM blob WHERE id = t.blob_id)",
        };
        let mut stmt = store
            .conn
            .prepare(&format!("SELECT {oid}, t.* FROM {table} t"))
            .expect("a fact table");
        let names: Vec<String> = stmt.column_names().into_iter().map(String::from).collect();
        let found = stmt
            .query_map([], |row| {
                let mut line = table.clone();
                for (at, name) in names.iter().enumerate().skip(1) {
                    if !matches!(name.as_str(), "id" | "blob_id" | "oid" | "written_by") {
                        let value: rusqlite::types::Value = row.get(at)?;
                        line.push_str(&format!("\t{name}={value:?}"));
                    }
                }
                Ok((row.get::<_, String>(0)?, line))
            })
            .expect("rows")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("rows read");
        for (oid, line) in found {
            rows.entry(oid).or_default().push(line);
        }
    }
    rows.values_mut().for_each(|lines| lines.sort());
    rows
}

/// `blob` and every table keyed by one: a fact table added to the schema is
/// guarded without being listed here.
fn fact_tables(store: &Store) -> Vec<String> {
    let mut stmt = store
        .conn
        .prepare(
            "SELECT m.name FROM sqlite_master m WHERE m.type = 'table' AND (m.name = 'blob' \
             OR EXISTS (SELECT 1 FROM pragma_table_info(m.name) WHERE name = 'blob_id'))",
        )
        .expect("the schema");
    let tables = stmt
        .query_map([], |row| row.get(0))
        .expect("tables")
        .collect::<rusqlite::Result<Vec<String>>>()
        .expect("tables read");
    assert!(tables.len() > 1, "no fact tables found: {tables:?}");
    tables
}

#[test]
fn extraction_matches_its_golden() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut paths = Vec::new();
    sources(&root.join("tests/testbed"), &mut paths);
    let mut by_kind: BTreeMap<&str, Vec<(String, Vec<u8>)>> = BTreeMap::new();
    for path in paths {
        let relative = path.strip_prefix(root).expect("under the repo");
        let relative = relative.to_string_lossy().into_owned();
        if let Some(kind) = kind(&relative) {
            let bytes = std::fs::read(&path).expect("readable");
            by_kind.entry(kind).or_default().push((relative, bytes));
        }
    }
    // `kind path hash`, one line per input, sorted.
    let mut current: Vec<String> = Vec::new();
    for (kind, files) in &by_kind {
        let rows = stored(files);
        for (path, bytes) in files {
            let oid = crate::scan::hash_blob(bytes).0;
            let hash = fnv(rows.get(&oid).map_or(&[][..], Vec::as_slice));
            current.push(format!("{kind} {path} {hash:016x}"));
        }
    }
    current.sort();

    let path = root.join(GOLDEN);
    let version = schema::VERSION.to_string();
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        let header = format!(
            "# Each testbed input's stored facts, hashed, at the store version below.\n\
             # Regenerate: {REGENERATE}\nversion {version}\n"
        );
        std::fs::write(&path, header + &current.join("\n") + "\n").expect("golden written");
        return;
    }
    let golden = std::fs::read_to_string(&path).unwrap_or_default();
    let recorded_version = golden
        .lines()
        .find_map(|line| line.strip_prefix("version "))
        .unwrap_or_default();
    let recorded: BTreeMap<&str, &str> = golden
        .lines()
        .filter(|line| !line.starts_with('#') && !line.starts_with("version "))
        .filter_map(|line| line.rsplit_once(' '))
        .collect();
    let moved: Vec<&str> = current
        .iter()
        .filter_map(|line| line.rsplit_once(' '))
        .filter(|(input, hash)| recorded.get(input).is_some_and(|was| was != hash))
        .map(|(input, _)| input)
        .collect();
    let mut kinds: Vec<&str> = moved.iter().filter_map(|m| m.split(' ').next()).collect();
    kinds.dedup();
    assert!(
        moved.is_empty() || recorded_version != version,
        "extraction output changed ({}) at store version {version}: bump the \
         store version (schema::VERSION), then regenerate with {REGENERATE}\n\n{}",
        kinds.join(", "),
        moved.join("\n")
    );
    assert!(
        recorded_version == version,
        "the store version is {version}, and {GOLDEN} was taken at \
         {recorded_version}: regenerate with {REGENERATE}"
    );
    let unseen: Vec<&str> = current
        .iter()
        .filter_map(|line| line.rsplit_once(' '))
        .filter(|(input, _)| !recorded.contains_key(input))
        .map(|(input, _)| input)
        .collect();
    assert!(
        unseen.is_empty(),
        "unseen input: regenerate with {REGENERATE} (no store bump needed)\n\n{}",
        unseen.join("\n")
    );
}
