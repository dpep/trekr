//! Checkout scan: which Ruby files are here, and what blob is each one?
//!
//! This is the only module that knows a path exists. It hands the blob layer a
//! set of OIDs and keeps the path→OID map for itself, which is exactly the seam
//! that lets two worktrees of one repo share one index.
//!
//! Git already stores the OID of every tracked file, so `git ls-files -s` is a
//! ~100 ms answer on 100k files. Only files that differ from the index get
//! hashed, and they get hashed *the way git does* so an uncommitted edit keys
//! the same as it will once committed.

use crate::core::Oid;
use anyhow::Result;
use sha1::{Digest, Sha1};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// One checkout's Ruby files, path (repo-relative) → blob OID.
pub(crate) type Files = BTreeMap<String, Oid>;

/// Is this a file we can extract facts from?
pub(crate) fn is_ruby(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    if let Some((_, ext)) = name.rsplit_once('.')
        && matches!(ext, "rb" | "rake" | "gemspec" | "ru" | "jbuilder" | "rbi")
    {
        return true;
    }
    matches!(
        name,
        "Gemfile" | "Rakefile" | "Guardfile" | "Capfile" | "Podfile" | "Brewfile"
    )
}

/// Git's blob hash: SHA-1 over `blob <byte-len>\0` then the content.
pub(crate) fn hash_blob(bytes: &[u8]) -> Oid {
    let mut hasher = Sha1::new();
    hasher.update(format!("blob {}\0", bytes.len()).as_bytes());
    hasher.update(bytes);
    Oid(format!("{:x}", hasher.finalize()))
}

/// Parse `git ls-files -s -z`: `<mode> <oid> <stage>\t<path>\0` per entry.
///
/// Non-blob modes are dropped: `160000` is a submodule (the OID names a commit
/// in another repo, not content we can read) and `120000` is a symlink (the
/// blob is a path string, not Ruby).
pub(crate) fn parse_ls_files(out: &[u8]) -> Files {
    let mut files = Files::new();
    for entry in out.split(|b| *b == 0) {
        let Ok(entry) = std::str::from_utf8(entry) else {
            continue;
        };
        let Some((meta, path)) = entry.split_once('\t') else {
            continue;
        };
        let mut parts = meta.split(' ');
        let (Some(mode), Some(oid)) = (parts.next(), parts.next()) else {
            continue;
        };
        if mode != "100644" && mode != "100755" {
            continue;
        }
        if is_ruby(path) {
            files.insert(path.to_string(), Oid(oid.to_string()));
        }
    }
    files
}

/// Split a `-z` (NUL-delimited) git path list.
fn parse_paths(out: &[u8]) -> Vec<String> {
    out.split(|b| *b == 0)
        .filter_map(|p| std::str::from_utf8(p).ok())
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect()
}

/// git could not be run, or refused. A type rather than a string, so the CLI
/// can tell "you are not in a checkout" from a store or file failure.
#[derive(Debug)]
pub(crate) struct GitError {
    message: String,
    source: Option<std::io::Error>,
}

impl GitError {
    pub(crate) fn failed(message: impl Into<String>) -> Self {
        GitError {
            message: message.into(),
            source: None,
        }
    }

    /// git's own words for it, which are stable across versions.
    pub(crate) fn not_a_repo(&self) -> bool {
        self.message.contains("not a git repository")
    }
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for GitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_ref().map(|e| e as _)
    }
}

fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|source| GitError {
            message: "could not run git (is it installed and on PATH?)".into(),
            source: Some(source),
        })?;
    if !out.status.success() {
        return Err(GitError::failed(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
        .into());
    }
    Ok(out.stdout)
}

/// The repository root containing `path`, which may name a file or a
/// directory — callers hold whichever the user typed, and git needs a
/// directory to run in.
pub(crate) fn repo_root(path: &Path) -> Result<PathBuf> {
    let dir = if path.is_dir() {
        path
    } else {
        // A bare filename's parent is the empty path, which is not a directory
        // git can run in.
        path.parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
    };
    // git's own complaint names `.git` and "parent directories"; say it plainly.
    let not_a_repo = || {
        let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        GitError::failed(format!(
            "not a git repository: {} (trekr answers inside a git checkout)",
            crate::core::paths::pretty(&dir.to_string_lossy())
        ))
    };
    if let Some(root) = discover(dir) {
        return Ok(root);
    }
    let out = match git(dir, &["rev-parse", "--show-toplevel"]) {
        Err(e)
            if e.downcast_ref::<GitError>()
                .is_some_and(GitError::not_a_repo) =>
        {
            return Err(not_a_repo().into());
        }
        out => out?,
    };
    let path = String::from_utf8(out)?.trim().to_string();
    if path.is_empty() {
        return Err(not_a_repo().into());
    }
    Ok(PathBuf::from(path))
}

/// `git rev-parse --show-toplevel` without running git, for the plain case:
/// the nearest ancestor holding a `.git` that is recognisably a repository,
/// on the same filesystem, owned by us. Every query asks this, and the spawn
/// was most of a fast one. `None` whenever git might answer differently —
/// discovery steered by the environment, a `.git` it would not accept, a
/// configured work tree, `safe.directory`'s ownership rule — and git decides.
fn discover(dir: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    const STEERING: [&str; 4] = [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
        "GIT_COMMON_DIR",
    ];
    if STEERING.iter().any(|var| std::env::var_os(var).is_some()) {
        return None;
    }
    let start = std::fs::canonicalize(dir).ok()?;
    // Inside a gitdir git refuses `--show-toplevel`; a walk would not.
    if start.components().any(|c| c.as_os_str() == ".git") {
        return None;
    }
    // Git never climbs into the nearest ceiling above where it starts.
    let ceiling = std::env::var("GIT_CEILING_DIRECTORIES")
        .unwrap_or_default()
        .split(':')
        .filter(|entry| !entry.is_empty())
        .map(|entry| std::fs::canonicalize(entry).unwrap_or_else(|_| PathBuf::from(entry)))
        .filter(|entry| *entry != start && start.starts_with(entry))
        .max_by_key(|entry| entry.as_os_str().len());
    let device = std::fs::metadata(&start).ok()?.dev();
    let uid = unsafe { libc::geteuid() };
    for candidate in start.ancestors() {
        if ceiling.as_ref().is_some_and(|c| c.starts_with(candidate)) {
            return None;
        }
        let meta = std::fs::metadata(candidate).ok()?;
        // Git stops at a filesystem boundary unless told otherwise.
        if meta.dev() != device {
            return None;
        }
        let dot_git = candidate.join(".git");
        let Ok(git_meta) = std::fs::symlink_metadata(&dot_git) else {
            continue;
        };
        let gitdir = if git_meta.is_dir() {
            dot_git
        } else if git_meta.is_file() {
            // A worktree's or submodule's `.git` names its gitdir.
            let text = std::fs::read_to_string(&dot_git).ok()?;
            let named = PathBuf::from(text.strip_prefix("gitdir:")?.trim());
            candidate.join(named)
        } else {
            return None;
        };
        let common = match std::fs::read_to_string(gitdir.join("commondir")) {
            Ok(text) => gitdir.join(text.trim()),
            Err(_) => gitdir.clone(),
        };
        let recognised = gitdir.join("HEAD").is_file()
            && common.join("objects").is_dir()
            && common.join("refs").is_dir();
        // `core.worktree`, `core.bare`, per-worktree config: git's to read.
        let config = std::fs::read_to_string(common.join("config"))
            .ok()?
            .to_ascii_lowercase();
        let plain = !config.contains("worktree") && !config.contains("bare = true");
        let ours = uid != 0 && meta.uid() == uid && std::fs::metadata(&gitdir).ok()?.uid() == uid;
        if !recognised || !plain || !ours {
            return None;
        }
        return true_case(candidate);
    }
    None
}

/// The path as the filesystem spells it. `canonicalize` keeps a
/// case-insensitive volume's typed case; git's answer, from `getcwd`, does not.
#[cfg(target_os = "macos")]
fn true_case(dir: &Path) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let file = std::fs::File::open(dir).ok()?;
    let mut buf = vec![0u8; libc::PATH_MAX as usize];
    let fd = std::os::unix::io::AsRawFd::as_raw_fd(&file);
    if unsafe { libc::fcntl(fd, libc::F_GETPATH, buf.as_mut_ptr()) } == -1 {
        return None;
    }
    let len = buf.iter().position(|b| *b == 0)?;
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..len])))
}

#[cfg(not(target_os = "macos"))]
fn true_case(dir: &Path) -> Option<PathBuf> {
    Some(dir.to_path_buf())
}

/// A cheap fingerprint of git's own view of the checkout (DEC-035).
///
/// `.git/index` is rewritten by `add`, `checkout`, `merge`, `rebase`, and by
/// the `status`/`diff` that any editor or prompt runs constantly — so its mtime
/// and size move whenever git has noticed anything. Reading two numbers off one
/// stat is **O(1) in repo size**, which is the property that matters: the full
/// scan is 145 ms on discourse and 6 s on a 10M-line monorepo, and neither can
/// sit on a query path.
///
/// **What it cannot see**, stated here so nobody rediscovers it: a tracked file
/// edited with nothing having refreshed git's index, and a brand-new untracked
/// file. Both are caught by an explicit `--index`. This is a *probe*, not a
/// proof — it answers "might anything have changed", and a false negative is
/// the reason `--index` still exists.
pub(crate) fn git_fingerprint(root: &Path) -> Option<i64> {
    // A worktree's `.git` is a file pointing at the real gitdir.
    let dot_git = root.join(".git");
    let index = match std::fs::read_to_string(&dot_git) {
        Ok(text) => {
            let dir = text.strip_prefix("gitdir:")?.trim();
            PathBuf::from(dir).join("index")
        }
        Err(_) => dot_git.join("index"),
    };
    let meta = std::fs::metadata(index).ok()?;
    let modified = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    // Nanoseconds and size together: a same-second rewrite of the same length
    // is possible, and the nanoseconds are what separate them.
    Some((modified.as_nanos() as i64).wrapping_mul(31) ^ (meta.len() as i64))
}

/// Every Ruby file in the working tree, keyed by the blob its *current* bytes
/// hash to — not what HEAD says. Uncommitted edits are first-class.
pub(crate) fn scan(root: &Path) -> Result<Files> {
    let mut files = parse_ls_files(&git(root, &["ls-files", "-s", "-z"])?);

    // Tracked files whose working-tree bytes differ from the index, plus files
    // git has never seen. Both need hashing; nothing else does. One `status`
    // answers both, and unlike `ls-files -o` it uses git's untracked cache,
    // which is most of a no-op index where it is enabled (DEC-043).
    // `--no-optional-locks` keeps it from rewriting `.git/index`, which is
    // what `git_fingerprint` watches.
    let (mut dirty, untracked_dirs) = parse_status(&git(
        root,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain",
            "-z",
            "--untracked-files=normal",
            "--no-renames",
            "--ignore-submodules=all",
        ],
    )?);
    // `normal` names a wholly untracked directory rather than its files; list
    // those alone, which walks only them.
    if !untracked_dirs.is_empty() {
        let mut args = vec!["ls-files", "-o", "--exclude-standard", "-z", "--"];
        args.extend(untracked_dirs.iter().map(String::as_str));
        dirty.extend(parse_paths(&git(root, &args)?));
    }

    for path in dirty {
        if !is_ruby(&path) {
            continue;
        }
        match std::fs::read(root.join(&path)) {
            Ok(bytes) => {
                files.insert(path, hash_blob(&bytes));
            }
            // Deleted from the worktree, or unreadable: it is not here, so it
            // is not in the map. No error case to handle downstream.
            Err(_) => {
                files.remove(&path);
            }
        }
    }
    Ok(files)
}

/// Parse `git status --porcelain -z`: `XY <path>\0` per entry, plus the
/// original path as a second field after a rename or copy.
///
/// Returns every path git reported — staged-only ones included, which rehash
/// to the OID the index already gave them — and, separately, the untracked
/// directories `--untracked-files=normal` collapsed to `dir/`.
fn parse_status(out: &[u8]) -> (Vec<String>, Vec<String>) {
    let (mut paths, mut dirs) = (Vec::new(), Vec::new());
    let mut fields = out.split(|b| *b == 0);
    while let Some(field) = fields.next() {
        let Some((code, path)) = std::str::from_utf8(field)
            .ok()
            .and_then(|f| Some((f.get(..2)?, f.get(3..)?)))
        else {
            continue;
        };
        if code.contains(['R', 'C']) {
            fields.next();
        }
        if code == "??" && path.ends_with('/') {
            dirs.push(path.to_string());
        } else if !path.is_empty() {
            paths.push(path.to_string());
        }
    }
    (paths, dirs)
}

/// Every Ruby file under a directory, hashed the way git would.
///
/// This is DEC-001's exception, and it earns it: a gem is not a git checkout,
/// but its bytes never change, so hashing them once per machine is the cheapest
/// case the blob store has. A project checkout still goes through `scan`,
/// because git already knows every OID and re-hashing 100k files to learn what
/// git could have told us is the cost this design exists to avoid.
pub(crate) fn walk(root: &Path, subdir: &str) -> Files {
    let mut files = Files::new();
    let start = root.join(subdir);
    let mut stack = vec![start];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                // Symlinked directories are how a walk turns into a cycle.
                if !kind.is_symlink() {
                    stack.push(path);
                }
                continue;
            }
            let Some(relative) = path.strip_prefix(root).ok().map(|p| p.to_string_lossy()) else {
                continue;
            };
            if !is_ruby(&relative) {
                continue;
            }
            if let Ok(bytes) = std::fs::read(&path) {
                files.insert(relative.into_owned(), hash_blob(&bytes));
            }
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_a_plain_directory_and_skips_what_is_not_ruby() {
        let temp = std::env::temp_dir().join(format!("trekr-walk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(temp.join("lib/deep")).unwrap();
        std::fs::create_dir_all(temp.join("test")).unwrap();
        std::fs::write(temp.join("lib/a.rb"), "class A; end\n").unwrap();
        std::fs::write(temp.join("lib/deep/b.rb"), "class B; end\n").unwrap();
        std::fs::write(temp.join("lib/README.md"), "no\n").unwrap();
        std::fs::write(temp.join("test/c.rb"), "class C; end\n").unwrap();

        let files = walk(&temp, "lib");
        let mut paths: Vec<&String> = files.keys().collect();
        paths.sort();
        assert_eq!(
            paths,
            ["lib/a.rb", "lib/deep/b.rb"],
            "recursive, Ruby only, and scoped to the subdirectory asked for"
        );
        assert_eq!(files["lib/a.rb"], hash_blob(b"class A; end\n"));
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn finds_the_root_git_would_name_or_leaves_it_to_git() {
        let temp = std::env::temp_dir().join(format!("trekr-discover-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        let git = |dir: &Path, args: &[&str]| {
            let ok = Command::new("git")
                .args(args)
                .current_dir(dir)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?}");
        };
        let main = temp.join("main");
        std::fs::create_dir_all(main.join("lib/deep")).unwrap();
        std::fs::create_dir_all(main.join("inner/sub")).unwrap();
        std::fs::write(main.join("lib/a.rb"), "class A; end\n").unwrap();
        git(&main, &["init", "-q"]);
        git(
            &main,
            &[
                "-c",
                "user.name=x",
                "-c",
                "user.email=x@x",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "x",
            ],
        );
        git(&main.join("inner"), &["init", "-q"]);
        git(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                temp.join("wt").to_str().unwrap(),
            ],
        );
        std::fs::create_dir_all(temp.join("wt/lib")).unwrap();
        let toplevel = |dir: &Path| {
            let out = Command::new("git")
                .args(["rev-parse", "--show-toplevel"])
                .current_dir(dir)
                .output()
                .unwrap();
            PathBuf::from(String::from_utf8(out.stdout).unwrap().trim())
        };

        for dir in [
            main.clone(),
            main.join("lib/deep"),
            main.join("inner/sub"),
            temp.join("wt/lib"),
        ] {
            assert_eq!(discover(&dir), Some(toplevel(&dir)), "{}", dir.display());
            assert_eq!(repo_root(&dir.join("x.rb")).unwrap(), toplevel(&dir));
        }
        // On a case-insensitive volume, the case git reports, not the typed one.
        let shouted = temp.join("MAIN/lib");
        if shouted.is_dir() {
            assert_eq!(discover(&shouted), Some(toplevel(&main)));
        }
        // A ceiling between the start and the repository hides it, from both.
        let before = std::env::var_os("GIT_CEILING_DIRECTORIES");
        unsafe { std::env::set_var("GIT_CEILING_DIRECTORIES", &main) };
        assert_eq!(discover(&main.join("lib/deep")), None);
        assert!(repo_root(&main.join("lib/deep")).is_err());
        assert_eq!(
            discover(&main),
            Some(toplevel(&main)),
            "the start itself is looked at"
        );
        match before {
            Some(value) => unsafe { std::env::set_var("GIT_CEILING_DIRECTORIES", value) },
            None => unsafe { std::env::remove_var("GIT_CEILING_DIRECTORIES") },
        }
        // A gitdir itself is git's call, and git says no.
        assert_eq!(discover(&main.join(".git/objects")), None);
        // A `.git` git would not accept: leave it to git, which refuses it.
        let fake = temp.join("fake");
        std::fs::create_dir_all(fake.join(".git")).unwrap();
        assert_eq!(discover(&fake), None);
        assert!(repo_root(&fake).is_err());
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn hashes_a_blob_the_way_git_does() {
        // `printf '' | git hash-object --stdin` and the same for "hello\n".
        assert_eq!(hash_blob(b"").0, "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391");
        assert_eq!(
            hash_blob(b"hello\n").0,
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
    }

    #[test]
    fn recognizes_ruby_by_extension_and_by_bare_name() {
        for path in ["a/b.rb", "lib/t.rake", "x.gemspec", "config.ru", "Gemfile"] {
            assert!(is_ruby(path), "{path} is Ruby");
        }
        for path in ["a/b.py", "README.md", "Gemfile.lock", "norb"] {
            assert!(!is_ruby(path), "{path} is not Ruby");
        }
    }

    #[test]
    fn status_names_every_changed_path_and_the_untracked_directories_apart() {
        let out = b" M app/a.rb\x00M  staged.rb\x00?? new.rb\x00?? fresh/\x00\
                    UU conflict.rb\x00R  to.rb\x00from.rb\x00 D gone.rb\x00";
        let (paths, dirs) = parse_status(out);
        assert_eq!(
            paths,
            [
                "app/a.rb",
                "staged.rb",
                "new.rb",
                "conflict.rb",
                "to.rb",
                "gone.rb"
            ],
            "a rename's origin is its own field, not another path"
        );
        assert_eq!(dirs, ["fresh/"]);
    }

    #[test]
    fn keeps_only_ruby_blobs_and_drops_symlinks_and_submodules() {
        let out = b"100644 aaa 0\ta.rb\x00100644 bbb 0\tb.py\x00\
                    120000 ccc 0\tlink.rb\x00160000 ddd 0\tsub\x00100755 eee 0\tbin/x.rb\x00";
        let files = parse_ls_files(out);
        assert_eq!(
            files.keys().collect::<Vec<_>>(),
            ["a.rb", "bin/x.rb"],
            "symlinks and submodules carry no Ruby content"
        );
        assert_eq!(files["a.rb"].0, "aaa");
    }
}
