//! Where tree snapshots live on disk, and when one is replaced (DEC-060,
//! DEC-065).
//!
//! One file per checkout and key, beside the store, never written in place:
//! a new snapshot is written to a temporary name, synced, and renamed over
//! its final one. A process that already mapped a file keeps reading it
//! after it is replaced or unlinked — that is POSIX, not something this code
//! arranges — and moves to a new one when the checkout's key moves, as the
//! LSP already did for its trees.

use super::snapshot::{self, Bytes, Invalid, Key, Snapshot};
use crate::store::Store;
use sha1::{Digest, Sha1};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

const SUFFIX: &str = ".tree";
const TEMP: &str = ".tmp";
/// A temporary file older than this was left by a writer that died; a live
/// one takes seconds.
const ABANDONED: std::time::Duration = std::time::Duration::from_secs(3600);

/// The directory beside the store: `trekr.db` keeps its trees in
/// `trekr.trees/`. `None` for an in-memory store.
pub(super) fn dir(store: &Store) -> Option<PathBuf> {
    store.path().map(|db| db.with_extension("trees"))
}

/// Everything a tree is assembled from, folded into one key.
///
/// Each root's surface key, in tree order, is what the store contributes;
/// the paths themselves, because sites are absolute; and the code that
/// assembles and encodes, by its own source text. The last is what keeps a
/// rebuilt binary — a release, or a dev build between two — from reading a
/// namespace an older assembly produced under the same format number.
pub(super) fn key(store: &Store, roots: &[String]) -> anyhow::Result<Key> {
    let mut hash = Sha1::new();
    let mut eat = |bytes: &[u8]| {
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    };
    eat(code());
    eat(&store.schema_version()?.to_le_bytes());
    for (root, surface) in roots.iter().zip(store.surface_keys(roots)?) {
        eat(root.as_bytes());
        eat(&surface.to_le_bytes());
    }
    Ok(hash.finalize().into())
}

/// This binary's assembly, as a digest: the part of every key that never
/// changes while the process runs.
fn code() -> &'static [u8] {
    static CODE: std::sync::OnceLock<Key> = std::sync::OnceLock::new();
    CODE.get_or_init(|| {
        let mut hash = Sha1::new();
        hash.update(b"trekr tree snapshot");
        hash.update(snapshot::FORMAT.to_le_bytes());
        hash.update(env!("CARGO_PKG_VERSION"));
        for source in [
            include_str!("mod.rs"),
            include_str!("snapshot.rs"),
            include_str!("core.rb"),
            include_str!("../store/mod.rs"),
        ] {
            hash.update((source.len() as u64).to_le_bytes());
            hash.update(source);
        }
        hash.finalize().into()
    })
}

/// `<checkout>-<key>.tree`. The checkout part is what lets a new snapshot
/// retire the checkout's previous one without reading anything.
pub(super) fn name(root: &str, key: &Key) -> String {
    format!("{}-{}{SUFFIX}", tag(root), hex(key))
}

fn tag(root: &str) -> String {
    hex(&Sha1::digest(root.as_bytes())[..8])
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Why no usable snapshot was found — reported under `--profile`.
#[derive(Debug)]
pub(super) enum Miss {
    Absent,
    Unreadable(std::io::Error),
    Invalid(Invalid),
}

impl std::fmt::Display for Miss {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Miss::Absent => write!(f, "none for this key"),
            Miss::Unreadable(e) => write!(f, "unreadable: {e}"),
            Miss::Invalid(why) => write!(f, "refused: {why:?}"),
        }
    }
}

/// Map a snapshot and check it answers to `key`.
pub(super) fn open(path: &Path, key: &Key) -> Result<Snapshot, Miss> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(Miss::Absent),
        Err(e) => return Err(Miss::Unreadable(e)),
    };
    // SAFETY: a mapped file's bytes must not change underneath the mapping.
    // No snapshot is ever written in place: a new one is renamed over the
    // name and an old one unlinked, and both leave an existing mapping's
    // pages as they were. What remains is someone truncating the file by
    // hand, which is outside the contract exactly as it is for the store's
    // own mapping (DEC-051).
    let map = unsafe { memmap2::Mmap::map(&file) }.map_err(Miss::Unreadable)?;
    Snapshot::parse(Bytes::Mapped(map), key).map_err(Miss::Invalid)
}

/// Write a snapshot under its final name, retire the checkout's older ones,
/// and hand back the result mapped.
///
/// Best effort: a snapshot that cannot be written (a read-only directory, a
/// full disk) or mapped once written is still a correct tree, held on the
/// heap instead.
pub(super) fn save(dir: &Path, root: &str, key: &Key, bytes: Vec<u8>) -> Snapshot {
    let name = name(root, key);
    let path = dir.join(&name);
    if write(dir, &path, &bytes).is_ok() {
        retire(dir, &tag(root), &name);
        if let Ok(mapped) = open(&path, key) {
            return mapped;
        }
    }
    Snapshot::parse(Bytes::Owned(bytes), key).expect("a namespace just encoded parses")
}

/// Temporary name, sync, rename. Two writers racing on one key write the
/// same bytes, so whichever rename lands second changes nothing.
fn write(dir: &Path, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let temp = path.with_extension(format!("{}.{nanos}{TEMP}", std::process::id()));
    let written = std::fs::File::create_new(&temp).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()
    });
    let renamed = written.and_then(|()| std::fs::rename(&temp, path));
    if renamed.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    renamed
}

/// Remove this checkout's snapshots other than `keep`, and any temporary
/// file of its that a dead writer left. Only this checkout's: another
/// checkout's current snapshot is not this one's to judge.
fn retire(dir: &Path, tag: &str, keep: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(tag) || name == keep {
            continue;
        }
        if name.ends_with(SUFFIX) || (name.ends_with(TEMP) && abandoned(&entry)) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// What `--gc` removed from the snapshot directory, or — on a dry run —
/// would have.
#[derive(Debug, Default, serde::Serialize)]
pub(crate) struct Swept {
    pub(crate) files: usize,
    pub(crate) bytes: u64,
}

/// Remove every snapshot no checkout's current key names, and any temporary
/// file a dead writer left.
///
/// Writing a snapshot already retires its checkout's older ones; what that
/// leaves is a snapshot whose key moved with no query since, and those of a
/// checkout that is gone. `gone` names checkouts this same collection is
/// removing, so that a dry run — which leaves them in the store — reports
/// what the real one would do.
pub(crate) fn sweep(store: &Store, gone: &[&str], dry_run: bool) -> anyhow::Result<Swept> {
    let mut swept = Swept::default();
    let Some(dir) = dir(store) else {
        return Ok(swept);
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(swept);
    };
    let mut live = HashSet::new();
    for root in store.roots()? {
        if gone.contains(&root.as_str()) {
            continue;
        }
        let mut roots = store.gems_used(&root)?;
        roots.push(root.clone());
        live.insert(name(&root, &key(store, &roots)?));
    }
    for entry in entries.flatten() {
        let file = entry.file_name();
        let Some(file) = file.to_str() else { continue };
        let stale = file.ends_with(SUFFIX) && !live.contains(file);
        if !stale && !(file.ends_with(TEMP) && abandoned(&entry)) {
            continue;
        }
        swept.files += 1;
        swept.bytes += entry.metadata().map_or(0, |m| m.len());
        if !dry_run {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Ok(swept)
}

fn abandoned(entry: &std::fs::DirEntry) -> bool {
    entry
        .metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age > ABANDONED)
}

#[cfg(test)]
mod tests {
    use super::super::Tree;
    use super::*;

    const ROOT: &str = "/repo";

    /// A store holding one checkout of these files, in a fresh directory.
    fn store(name: &str, sources: &[(&str, &str)]) -> (PathBuf, Store) {
        let dir = std::env::temp_dir().join(format!("trekr-snap-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut store = Store::open(&dir.join("t.db")).unwrap();
        index(&mut store, sources);
        (dir, store)
    }

    fn index(store: &mut Store, sources: &[(&str, &str)]) {
        let mut files = crate::scan::Files::new();
        let mut facts = Vec::new();
        for (path, source) in sources {
            let oid = crate::scan::hash_blob(source.as_bytes());
            files.insert(path.to_string(), oid.clone());
            if !store.has_blob(&oid).unwrap() {
                facts.push((oid, crate::extract::extract(source.as_bytes())));
            }
        }
        store.write(ROOT, &files, facts, 0).unwrap();
    }

    /// The snapshot files in the store's directory, by name.
    fn snapshots(store: &Store) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir(store).unwrap())
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    fn current(store: &Store) -> PathBuf {
        let key = key(store, &[ROOT.to_string()]).unwrap();
        dir(store).unwrap().join(name(ROOT, &key))
    }

    const WIDGET: (&str, &str) = (
        "widget.rb",
        "module Parts\n  class Widget < Base\n    include Named\n  end\nend\nclass Base\nend\nmodule Named\nend\n",
    );

    fn answers(tree: &Tree) -> (Vec<String>, Option<String>) {
        (
            tree.ancestors("Parts::Widget").chain.clone(),
            tree.resolve("Widget", &["Parts".to_string()]).fqn,
        )
    }

    #[test]
    fn a_tree_is_written_once_and_mapped_after() {
        let (dir, store) = store("once", &[WIDGET]);
        let built = Tree::build(&store, ROOT).unwrap();
        assert_eq!(
            snapshots(&store),
            [name(ROOT, &key(&store, &[ROOT.into()]).unwrap())]
        );
        let written = std::fs::read(current(&store)).unwrap();
        let loaded = Tree::build(&store, ROOT).unwrap();
        assert!(matches!(&loaded.names, super::super::Names::Frozen(s) if s.is_mapped()));
        assert_eq!(answers(&loaded), answers(&built));
        assert_eq!(
            std::fs::read(current(&store)).unwrap(),
            written,
            "read, not rewritten"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Whatever is wrong with the file — cut short, another format, answering
    /// to another key under this one's name — it is rebuilt, never read.
    #[test]
    fn a_file_that_does_not_check_out_is_rebuilt() {
        let (dir, store) = store("damaged", &[WIDGET]);
        let good = answers(&Tree::build(&store, ROOT).unwrap());
        let path = current(&store);
        let bytes = std::fs::read(&path).unwrap();
        type Damage = fn(&mut Vec<u8>);
        let damage: [(&str, Damage); 3] = [
            ("truncated", |b| b.truncate(b.len() / 2)),
            ("another format", |b| b[8] ^= 1),
            ("another key", |b| b[16] ^= 1),
        ];
        for (what, damage) in damage {
            let mut bad = bytes.clone();
            damage(&mut bad);
            std::fs::write(&path, &bad).unwrap();
            let tree = Tree::build(&store, ROOT).unwrap();
            assert_eq!(answers(&tree), good, "{what}");
            assert!(
                std::fs::read(&path).unwrap() == bytes,
                "{what}: rewritten whole"
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_moved_key_writes_a_new_file_and_retires_the_old() {
        let (dir, mut store) = store("moved", &[WIDGET]);
        Tree::build(&store, ROOT).unwrap();
        let before = snapshots(&store);
        index(
            &mut store,
            &[WIDGET, ("gadget.rb", "class Gadget < Base\nend\n")],
        );
        let tree = Tree::build(&store, ROOT).unwrap();
        assert!(tree.is_known("Gadget"), "answers from the new index");
        let after = snapshots(&store);
        assert_eq!(after.len(), 1);
        assert_ne!(after, before, "the stale key's file is gone");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A dry run reports a checkout being collected in the same pass, and a
    /// temporary file is swept only once a live writer could not own it.
    #[test]
    fn a_sweep_keeps_what_a_checkout_names_and_nothing_else() {
        let (tmp, mut store) = store("sweep", &[WIDGET]);
        let mut files = crate::scan::Files::new();
        let other = "class Other\nend\n";
        let oid = crate::scan::hash_blob(other.as_bytes());
        files.insert("other.rb".to_string(), oid.clone());
        let facts = vec![(oid, crate::extract::extract(other.as_bytes()))];
        store.write("/other", &files, facts, 0).unwrap();
        Tree::build(&store, ROOT).unwrap();
        Tree::build(&store, "/other").unwrap();
        let trees = dir(&store).unwrap();
        let temp = |name: &str, age: u64| {
            let file = std::fs::File::create(trees.join(name)).unwrap();
            let then = std::time::SystemTime::now() - std::time::Duration::from_secs(age);
            file.set_modified(then).unwrap();
        };
        temp("dead.1.2.tmp", 2 * 3600);
        temp("live.3.4.tmp", 5);
        assert_eq!(snapshots(&store).len(), 4);

        let dry = sweep(&store, &["/other"], true).unwrap();
        assert_eq!(dry.files, 2, "/other's tree and the dead writer's file");
        assert_eq!(snapshots(&store).len(), 4, "a dry run removes nothing");

        let done = sweep(&store, &[], false).unwrap();
        assert_eq!(done.files, 1, "only the dead writer's file");
        assert!(!snapshots(&store).contains(&"dead.1.2.tmp".to_string()));
        assert_eq!(snapshots(&store).len(), 3);
        let _ = std::fs::remove_dir_all(tmp);
    }

    /// Builders racing on one key each get a tree, and leave one file.
    #[test]
    fn racing_builders_leave_one_file() {
        let (dir, store) = store("race", &[WIDGET]);
        let db = store.path().unwrap().to_path_buf();
        let got: Vec<_> = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..4)
                .map(|_| {
                    let db = db.clone();
                    scope.spawn(move || {
                        answers(&Tree::build(&Store::open(&db).unwrap(), ROOT).unwrap())
                    })
                })
                .collect();
            workers.into_iter().map(|w| w.join().unwrap()).collect()
        });
        assert!(got.windows(2).all(|w| w[0] == w[1]));
        assert_eq!(snapshots(&store).len(), 1, "{:?}", snapshots(&store));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The child half of the test below, in its own process: rewrites the
    /// current snapshot in place of itself, or builds the tree for whatever
    /// the store now holds. A no-op when run on its own.
    #[test]
    fn writer_process() {
        let Ok(db) = std::env::var("TREKR_TEST_SNAPSHOT_WRITER") else {
            return;
        };
        let store = Store::open(Path::new(&db)).unwrap();
        if std::env::var("TREKR_TEST_SNAPSHOT_REWRITE").is_ok() {
            let key = key(&store, &[ROOT.to_string()]).unwrap();
            let bytes = std::fs::read(current(&store)).unwrap();
            save(&dir(&store).unwrap(), ROOT, &key, bytes);
        } else {
            Tree::build(&store, ROOT).unwrap();
        }
    }

    /// Run `writer_process` and keep asking `reader` until it exits.
    fn while_another_process_writes(store: &Store, rewrite: bool, reader: &Tree) -> usize {
        let mut writer = std::process::Command::new(std::env::current_exe().unwrap());
        writer
            .args(["--exact", "tree::files::tests::writer_process", "--quiet"])
            .env("TREKR_TEST_SNAPSHOT_WRITER", store.path().unwrap())
            .stdout(std::process::Stdio::null());
        if rewrite {
            writer.env("TREKR_TEST_SNAPSHOT_REWRITE", "1");
        }
        let mut writer = writer.spawn().unwrap();
        let expected = answers(reader);
        let mut reads = 0;
        while writer.try_wait().unwrap().is_none() {
            assert_eq!(answers(reader), expected);
            reads += 1;
        }
        assert!(writer.wait().unwrap().success());
        reads
    }

    /// A process holding a mapped tree keeps answering from it while another
    /// process writes the checkout's next snapshot and unlinks this one.
    #[test]
    fn a_mapped_tree_outlives_its_file_being_replaced() {
        let (dir, mut store) = store("replaced", &[WIDGET]);
        let reader = Tree::build(&store, ROOT).unwrap();
        let expected = answers(&reader);
        let mapped = current(&store);
        let declared = {
            let mut d = reader.declared();
            d.sort();
            d
        };
        let inode =
            |path: &Path| std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(path).unwrap());

        // The same key written again — a racing builder — lands on the very
        // name this tree maps.
        let before = inode(&mapped);
        while_another_process_writes(&store, true, &reader);
        assert_ne!(inode(&mapped), before, "replaced, not rewritten in place");

        // The key moves, and the next builder unlinks this tree's file.
        index(
            &mut store,
            &[WIDGET, ("gadget.rb", "class Gadget < Base\nend\n")],
        );
        let reads = while_another_process_writes(&store, false, &reader);
        assert!(
            !mapped.exists(),
            "the writer retired the file this tree maps"
        );
        let mut after = reader.declared();
        after.sort();
        assert_eq!(
            after, declared,
            "every name still reads, after {reads} reads during"
        );
        assert_eq!(answers(&reader), expected);

        let next = Tree::build(&store, ROOT).unwrap();
        assert!(next.is_known("Gadget"));
        assert!(matches!(&next.names, super::super::Names::Frozen(s) if s.is_mapped()));
        let _ = std::fs::remove_dir_all(dir);
    }
}
