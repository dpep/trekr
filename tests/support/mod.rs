//! What the e2e suites share: where their scratch goes.
#![allow(dead_code, reason = "each test binary uses its own subset")]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

static ROOT: OnceLock<PathBuf> = OnceLock::new();
static FAILED: AtomicBool = AtomicBool::new(false);

/// This run's scratch: one directory under `$TMPDIR`, removed when the test
/// binary exits, or kept and named when a test failed. `--index` reads the
/// directory its store is in, so a `$TMPDIR` cluttered by earlier runs made
/// every index beside it slower.
pub fn root() -> &'static Path {
    ROOT.get_or_init(|| {
        let root = std::env::temp_dir().join(format!("trekr-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            FAILED.store(true, Ordering::SeqCst);
            previous(info);
        }));
        // SAFETY: `atexit` only records the pointer; `remove_root` is a plain
        // `extern "C"` function that takes nothing and cannot unwind.
        unsafe {
            libc::atexit(remove_root);
        }
        root
    })
}

/// Run at exit, after every test: libtest has no teardown of its own.
extern "C" fn remove_root() {
    use std::io::Write;
    let Some(root) = ROOT.get() else { return };
    if FAILED.load(Ordering::SeqCst) {
        let _ = writeln!(
            std::io::stderr(),
            "a test failed: its scratch is kept at {}",
            root.display()
        );
    } else {
        let _ = fs::remove_dir_all(root);
    }
}

/// A fresh, empty directory in this run's scratch.
pub fn fresh(name: &str) -> PathBuf {
    let dir = root().join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A scratch checkout named `label`, and a database in a directory of its
/// own: what is written beside a store — its tree snapshots, core's files,
/// usage counts, early stores — is that store's alone.
pub fn scratch(label: &str) -> (PathBuf, PathBuf) {
    let dir = fresh(label);
    let store = fresh(&format!("{label}.store"));
    (dir, store.join("trekr.db"))
}

/// A home holding one Ruby, 9.8.7, installed as rvm installs one, with an
/// empty stdlib and the rbs fixture as its signatures: what core is served
/// from (DEC-240), whatever Ruby the machine running the suite has.
pub fn fixture_home() -> &'static Path {
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    HOME.get_or_init(|| {
        let home = root().join("ruby-home");
        let lib = home.join(".rvm/rubies/ruby-9.8.7/lib/ruby");
        fs::create_dir_all(lib.join("9.8.0")).unwrap();
        fs::create_dir_all(lib.join("gems/9.8.0/specifications/default")).unwrap();
        fs::create_dir_all(lib.join("gems/9.8.0/gems")).unwrap();
        std::os::unix::fs::symlink(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rbs"),
            lib.join("gems/9.8.0/gems/rbs-9.9.9"),
        )
        .unwrap();
        home
    })
}

/// A directory holding only `git`, for a `PATH` that finds no `ruby`.
pub fn git_only() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let git = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|dir| dir.join("git"))
            .find(|git| git.is_file())
            .expect("git on PATH");
        let dir = root().join("git-only");
        fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink(git, dir.join("git")).unwrap();
        dir
    })
}
