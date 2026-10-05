//! What the e2e suites share: where their scratch goes, and the environment
//! the binary under test runs in.
#![allow(dead_code, reason = "each test binary uses its own subset")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
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

/// Variables of the caller's that reach past the scratch: git's (a gate run
/// under `git rebase --exec` exports `GIT_DIR`, and a fixture's `git init`
/// then writes into the real repository), and a Ruby's or a bundle's.
fn outside(name: &std::ffi::OsStr) -> bool {
    name.to_str().is_some_and(|name| {
        ["GIT_", "GEM_", "BUNDLE_"]
            .iter()
            .any(|p| name.starts_with(p))
    })
}

fn strip_outside(command: &mut Command) -> &mut Command {
    for (name, _) in std::env::vars_os().filter(|(name, _)| outside(name)) {
        command.env_remove(name);
    }
    command
}

/// `git`, run in a scratch directory and nowhere else.
pub fn git(dir: &Path, args: &[&str]) {
    let out = strip_outside(&mut Command::new("git"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git");
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// Make `command` run by nobody in particular on no Ruby in particular: no
/// caller `--usage` would name, `git` the only program on `PATH`, no gem or
/// bundle settings, and `home` as both `HOME` and the root the machine's
/// Rubies are looked for under — so a checkout runs on the Ruby `home` holds
/// (DEC-180), whatever the machine running the suite has.
pub fn neutral<'a>(command: &'a mut Command, home: &Path) -> &'a mut Command {
    for var in [
        // What `--usage` reads to tell an agent from a person or CI.
        "CLAUDECODE",
        "CLAUDE_CODE_ENTRYPOINT",
        "AI_AGENT",
        "CURSOR_TRACE_ID",
        "CURSOR_AGENT",
        "CI",
        "GITHUB_ACTIONS",
        "TREKR_USAGE",
        // Where a version manager keeps its Rubies.
        "MISE_DATA_DIR",
        "XDG_DATA_HOME",
    ] {
        command.env_remove(var);
    }
    strip_outside(command)
        .env("HOME", home)
        .env("TREKR_TEST_SYSTEM", home)
        .env("PATH", git_only())
}
