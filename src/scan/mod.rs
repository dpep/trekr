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
use anyhow::{Context, Result, bail};
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

fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .with_context(|| format!("running git {}", args.join(" ")))?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
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
    let out = git(dir, &["rev-parse", "--show-toplevel"])?;
    let path = String::from_utf8(out)?.trim().to_string();
    if path.is_empty() {
        bail!("not a git repository: {}", dir.display());
    }
    Ok(PathBuf::from(path))
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
    // git has never seen. Both need hashing; nothing else does.
    let mut dirty = parse_paths(&git(root, &["diff-files", "--name-only", "-z"])?);
    dirty.extend(parse_paths(&git(
        root,
        &["ls-files", "-o", "--exclude-standard", "-z"],
    )?));

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
