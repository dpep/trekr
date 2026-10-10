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

pub(crate) mod near;

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

/// An app's schema dumped as SQL — `db/structure.sql`, or a second
/// database's `db/<name>_structure.sql` — which is read for its tables
/// (DEC-480).
pub(crate) fn is_structure_sql(path: &str) -> bool {
    let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
    (dir == "db" || dir.ends_with("/db"))
        && (name == "structure.sql" || name.ends_with("_structure.sql"))
}

/// How a file's text is read, by its path: the one place that says so, so a
/// new template language is a new variant every reader must answer for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reader {
    /// Ruby as written — and anything not named below, a script without an
    /// extension too.
    Ruby,
    /// An ERB template: its tags' Ruby, the rest blanked (DEC-520).
    Erb,
    /// A RABL template, which is Ruby.
    Rabl,
    /// An app's schema dumped as SQL, read for its tables (DEC-480).
    StructureSql,
}

impl Reader {
    pub(crate) fn of(path: &str) -> Reader {
        if is_structure_sql(path) {
            Reader::StructureSql
        } else if is_erb(path) {
            Reader::Erb
        } else if path.ends_with(".rabl") {
            Reader::Rabl
        } else {
            Reader::Ruby
        }
    }
}

/// A view template whose Ruby the index reads (DEC-520): ERB, read through
/// its tags, and RABL, which is Ruby.
pub(crate) fn is_template(path: &str) -> bool {
    matches!(Reader::of(path), Reader::Erb | Reader::Rabl)
}

/// A file whose text is read as Ruby: Ruby by its name, a template's tags,
/// or a script named without an extension (`bin/rails`, `.irbrc`).
pub(crate) fn reads_as_ruby(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    is_ruby(path) || is_template(path) || !name.trim_start_matches('.').contains('.')
}

/// An ERB template: `show.html.erb`, `welcome.text.erb`, `_form.erb`.
pub(crate) fn is_erb(path: &str) -> bool {
    path.ends_with(".erb")
}

/// Is this a file the index reads? A gem's templates are not: no app calls
/// into them, and what they call is the gem's to answer.
pub(crate) fn is_indexed(path: &str) -> bool {
    is_ruby(path) || is_structure_sql(path) || is_template(path)
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
        if is_indexed(path) {
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
    git_in(root, args, None)
}

/// [`git`], registered with `gits` while it runs when there is one, so that
/// a [`Probe`] given up on can stop it.
fn git_in(root: &Path, args: &[&str], gits: Option<&Gits>) -> Result<Vec<u8>> {
    let mut command = Command::new("git");
    command.args(args).current_dir(root);
    let out = match gits {
        Some(gits) => gits.run(command),
        None => command.output(),
    }
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

/// The git processes one [`Probe`] has running. Stopping kills and reaps
/// each, and any started after: a walk nobody will read only slows the
/// next query's own.
#[derive(Default)]
struct Gits(std::sync::Mutex<Running>);

#[derive(Default)]
struct Running {
    stopped: bool,
    children: Vec<std::process::Child>,
}

impl Gits {
    fn lock(&self) -> std::sync::MutexGuard<'_, Running> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// `command.output()`, unless stopped first.
    fn run(&self, mut command: Command) -> std::io::Result<std::process::Output> {
        use std::io::Read;
        use std::process::Stdio;
        let stopped = || std::io::Error::new(std::io::ErrorKind::Interrupted, "given up on");
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut stdout = child.stdout.take().expect("stdout is piped");
        let mut stderr = child.stderr.take().expect("stderr is piped");
        let id = child.id();
        {
            let mut running = self.lock();
            if running.stopped {
                let _ = child.kill();
                let _ = child.wait();
                return Err(stopped());
            }
            running.children.push(child);
        }
        // Both pipes at once, or a full one stalls the other.
        let errors = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stderr.read_to_end(&mut bytes);
            bytes
        });
        let mut out = Vec::new();
        let read = stdout.read_to_end(&mut out);
        let err = errors.join().unwrap_or_default();
        let child = {
            let mut running = self.lock();
            let at = running.children.iter().position(|c| c.id() == id);
            at.map(|at| running.children.swap_remove(at))
        };
        // Gone from the list: `stop` killed and reaped it.
        let Some(mut child) = child else {
            return Err(stopped());
        };
        let status = child.wait()?;
        read?;
        Ok(std::process::Output {
            status,
            stdout: out,
            stderr: err,
        })
    }

    fn stop(&self) {
        let mut running = self.lock();
        running.stopped = true;
        for mut child in running.children.drain(..) {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
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
    let ceilings = std::env::var("GIT_CEILING_DIRECTORIES").unwrap_or_default();
    discover_under(dir, &ceilings)
}

/// [`discover`] with git's `GIT_CEILING_DIRECTORIES` given rather than read.
fn discover_under(dir: &Path, ceilings: &str) -> Option<PathBuf> {
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
    let ceiling = ceilings
        .split(':')
        .filter(|entry| !entry.is_empty())
        .map(|entry| std::fs::canonicalize(entry).unwrap_or_else(|_| PathBuf::from(entry)))
        .filter(|entry| *entry != start && start.starts_with(entry))
        .max_by_key(|entry| entry.as_os_str().len());
    let device = std::fs::metadata(&start).ok()?.dev();
    // SAFETY: geteuid takes nothing and cannot fail.
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
            let text = read_text(&dot_git).ok()?;
            let named = PathBuf::from(text.strip_prefix("gitdir:")?.trim());
            candidate.join(named)
        } else {
            return None;
        };
        let common = match read_text(gitdir.join("commondir")) {
            Ok(text) => gitdir.join(text.trim()),
            Err(_) => gitdir.clone(),
        };
        let recognised = gitdir.join("HEAD").is_file()
            && common.join("objects").is_dir()
            && common.join("refs").is_dir();
        // `core.worktree`, `core.bare`, per-worktree config: git's to read.
        let config = read_text(common.join("config")).ok()?.to_ascii_lowercase();
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
    // SAFETY: `fd` is open for as long as `file` lives, and F_GETPATH writes
    // at most PATH_MAX bytes, the buffer's length.
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
/// **What it cannot see**: a tracked file edited with nothing having refreshed
/// git's index, and a brand-new untracked file. A query no longer reads it:
/// [`Probe`] compares the working tree's content with the store's, which
/// sees both. The index still records it beside each checkout.
pub(crate) fn git_fingerprint(root: &Path) -> Option<i64> {
    // A worktree's `.git` is a file pointing at the real gitdir.
    let dot_git = root.join(".git");
    let index = match read_text(&dot_git) {
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

/// The most of one source file trekr reads: far past any hand-written Ruby or
/// schema dump, short of what would cost the process its memory.
pub(crate) const MAX_SOURCE: u64 = 64 << 20;

/// A file read as trekr reads one: a regular file (a pipe would wait for a
/// writer, `/dev/zero` never ends), no larger than [`MAX_SOURCE`]. Every file
/// trekr reads goes through here or [`read_text`]; clippy refuses
/// `std::fs::read` and `read_to_string` (`clippy.toml`).
pub(crate) fn read_source(path: impl AsRef<Path>) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let path = path.as_ref();
    let meta = std::fs::metadata(path)?;
    if !meta.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    let too_large = || {
        std::io::Error::new(
            std::io::ErrorKind::FileTooLarge,
            format!("larger than {} MiB", MAX_SOURCE >> 20),
        )
    };
    if meta.len() > MAX_SOURCE {
        return Err(too_large());
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    // Bounded again as read: the file may grow after the stat.
    std::fs::File::open(path)?
        .take(MAX_SOURCE + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SOURCE {
        return Err(too_large());
    }
    Ok(bytes)
}

/// [`read_source`], as UTF-8 text: a config file, a lockfile, a `.git` file.
pub(crate) fn read_text(path: impl AsRef<Path>) -> std::io::Result<String> {
    String::from_utf8(read_source(path)?)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// More changed files than this in one batch is an operation — a checkout, a
/// rebase — and is handed to a full index rather than refreshed one by one,
/// by the language server and a CLI query alike.
pub(crate) const BULK: usize = 32;

/// How long a query waits for [`Probe`]: past it, the answer comes from the
/// index and says the working tree was not checked. git takes this long only
/// when its stat cache is cold — after a copy or a restore — and re-hashes
/// every file, which a query does not sit through.
pub(crate) const PROBE_WAIT: std::time::Duration = std::time::Duration::from_secs(1);

/// The working tree's map as an index would write it ([`scan`]), read on a
/// thread so the query's tree is built meanwhile, and so it can stop waiting
/// at [`PROBE_WAIT`]: a query compares it with
/// the store to find every file edited, added or deleted since the index —
/// untracked ones and edits git has not been told about included (DEC-035).
/// `git status` compares content where a stat moved, so a `touch` is no edit.
/// `then` runs on the same thread with what it found, so the comparison is
/// made there too.
pub(crate) struct Probe<T> {
    started: std::time::Instant,
    found: std::sync::mpsc::Receiver<Result<T>>,
    gits: std::sync::Arc<Gits>,
}

/// A probe given up on, or never asked, stops its git.
impl<T> Drop for Probe<T> {
    fn drop(&mut self) {
        self.gits.stop();
    }
}

impl<T: Send + 'static> Probe<T> {
    pub(crate) fn start(root: &Path, then: impl FnOnce(Files) -> T + Send + 'static) -> Probe<T> {
        let (send, found) = std::sync::mpsc::channel();
        let root = root.to_path_buf();
        let gits = std::sync::Arc::new(Gits::default());
        let running = gits.clone();
        std::thread::spawn(move || drop(send.send(scan_in(&root, Some(&running)).map(then))));
        Probe {
            started: std::time::Instant::now(),
            found,
            gits,
        }
    }

    /// What `then` made of the working tree's map, or why the map could not
    /// be had, in words.
    pub(crate) fn finish(self) -> std::result::Result<T, String> {
        use std::sync::mpsc::RecvTimeoutError;
        let left = PROBE_WAIT.saturating_sub(self.started.elapsed());
        match self.found.recv_timeout(left) {
            Ok(Ok(files)) => Ok(files),
            Ok(Err(error)) => Err(format!(
                "git could not say what changed since the index ({error:#})"
            )),
            Err(RecvTimeoutError::Disconnected) => {
                Err("git could not say what changed since the index".to_string())
            }
            Err(RecvTimeoutError::Timeout) => Err(format!(
                "git took longer than {} s to say what changed since the index \
                 (a `git status` refreshes its cache)",
                PROBE_WAIT.as_secs()
            )),
        }
    }
}

/// Every Ruby file in the working tree, keyed by the blob its *current* bytes
/// hash to — not what HEAD says. Uncommitted edits are first-class.
pub(crate) fn scan(root: &Path) -> Result<Files> {
    scan_in(root, None)
}

/// [`scan`], its gits registered with `gits`.
fn scan_in(root: &Path, gits: Option<&Gits>) -> Result<Files> {
    // Tracked files whose working-tree bytes differ from the index, plus files
    // git has never seen. Both need hashing; nothing else does. One `status`
    // answers both, and unlike `ls-files -o` it uses git's untracked cache,
    // which is most of a no-op index where it is enabled (DEC-043).
    // `--no-optional-locks` keeps it from rewriting `.git/index`, which is
    // what `git_fingerprint` watches. Both read the index alone, so they run
    // at once: a query waits on this (DEC-035).
    let (listed, status) = std::thread::scope(|scope| {
        let listed = scope.spawn(|| git_in(root, &["ls-files", "-s", "-z"], gits));
        let status = git_in(
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
            gits,
        );
        let listed = listed
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        (listed, status)
    });
    let mut files = parse_ls_files(&listed?);
    let (mut dirty, untracked_dirs) = parse_status(&status?);
    // `normal` names a wholly untracked directory rather than its files; list
    // those alone, which walks only them.
    if !untracked_dirs.is_empty() {
        let mut args = vec!["ls-files", "-o", "--exclude-standard", "-z", "--"];
        args.extend(untracked_dirs.iter().map(String::as_str));
        dirty.extend(parse_paths(&git_in(root, &args, gits)?));
    }

    for path in dirty {
        if !is_indexed(&path) {
            continue;
        }
        match read_source(root.join(&path)) {
            Ok(bytes) => {
                files.insert(path, hash_blob(&bytes));
            }
            // Deleted from the worktree, unreadable, or not a source at all (a
            // link to a device or a pipe): it is not here, so it is not in the map. No error case to handle downstream.
            Err(_) => {
                files.remove(&path);
            }
        }
    }
    one_schema_per_app(root, &mut files);
    Ok(files)
}

/// The key [`scan`] lists `relative` under, if it does, asked of one file:
/// indexed by name, tracked or untracked and not ignored, and the schema
/// dump its app reads. git's spelling, which on macOS is composed (NFC)
/// however the name is written on disk.
fn admits(root: &Path, relative: &str, gits: Option<&Gits>) -> Result<Option<String>> {
    if !is_indexed(relative) {
        return Ok(None);
    }
    let pathspec = format!(":(literal){relative}");
    let args = [
        "ls-files",
        "-c",
        "-o",
        "--exclude-standard",
        "-z",
        "--",
        &pathspec,
    ];
    let out = git_in(root, &args, gits)?;
    // A literal pathspec names this file alone, though git may spell it
    // otherwise; a directory's entries are not it.
    let inside = format!("{relative}/");
    let Some(key) = parse_paths(&out)
        .into_iter()
        .find(|p| !p.starts_with(&inside))
    else {
        return Ok(None);
    };
    if !crate::schema::is_dump(&key) {
        return Ok(Some(key));
    }
    let (dir, _) = key.rsplit_once('/').unwrap_or(("", &key));
    let app = &dir[..dir.len().saturating_sub("db".len())];
    Ok(schema_dumps(root, app).contains(&key).then_some(key))
}

/// How long git is given to say whether the index walk reads one file before
/// it is taken as hung. Waited for off the serve loop, so it can be long.
const ADMIT_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// [`admits`], asked on a thread so the serve loop waits for git no longer
/// than it chooses and takes the answer whenever it comes. Past
/// [`ADMIT_WAIT`], or dropped unanswered, its git is stopped, as a
/// [`Probe`]'s are.
pub(crate) struct Admission {
    started: std::time::Instant,
    found: std::sync::mpsc::Receiver<Result<Option<String>>>,
    gits: std::sync::Arc<Gits>,
    answer: Option<std::result::Result<Option<String>, String>>,
}

impl Drop for Admission {
    fn drop(&mut self) {
        self.gits.stop();
    }
}

impl Admission {
    pub(crate) fn start(root: &Path, relative: &str) -> Admission {
        let (send, found) = std::sync::mpsc::channel();
        let gits = std::sync::Arc::new(Gits::default());
        let running = gits.clone();
        let (root, relative) = (root.to_path_buf(), relative.to_string());
        std::thread::spawn(move || drop(send.send(admits(&root, &relative, Some(&running)))));
        Admission {
            started: std::time::Instant::now(),
            found,
            gits,
            answer: None,
        }
    }

    /// git's answer — [`admits`]'s key, or why there is none to be had —
    /// waiting up to `wait` for it; `None` while git is still out.
    pub(crate) fn answer(
        &mut self,
        wait: std::time::Duration,
    ) -> Option<std::result::Result<Option<String>, String>> {
        use std::sync::mpsc::RecvTimeoutError;
        if self.answer.is_none() {
            self.answer = match self.found.recv_timeout(wait) {
                Ok(found) => Some(found.map_err(|error| format!("{error:#}"))),
                Err(RecvTimeoutError::Timeout) if self.started.elapsed() < ADMIT_WAIT => None,
                Err(RecvTimeoutError::Timeout) => {
                    self.gits.stop();
                    Some(Err(format!(
                        "git ls-files took longer than {} s",
                        ADMIT_WAIT.as_secs()
                    )))
                }
                Err(RecvTimeoutError::Disconnected) => Some(Err("git did not answer".into())),
            };
        }
        self.answer.clone()
    }
}

/// The schema dumps in the app at `app` (a directory of `root`, `""` for the
/// root itself), as the index reads them: one per database.
pub(crate) fn schema_dumps(root: &Path, app: &str) -> Vec<String> {
    let db = format!("{app}db");
    let Ok(entries) = std::fs::read_dir(root.join(&db)) else {
        return Vec::new();
    };
    let mut files: Files = entries
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .map(|name| format!("{db}/{name}"))
        .filter(|path| crate::schema::is_dump(path))
        .map(|path| (path, Oid(String::new())))
        .collect();
    one_schema_per_app(root, &mut files);
    files.into_keys().collect()
}

/// An app that commits both `db/schema.rb` and `db/structure.sql` keeps one
/// of them current, and only that one is read: both would declare every
/// column twice, half of them stale (DEC-480). Rails loads the file
/// `schema_format` names in `config/application.rb`.
fn one_schema_per_app(root: &Path, files: &mut Files) {
    let dumps: Vec<String> = files
        .keys()
        .filter(|path| is_structure_sql(path))
        .cloned()
        .collect();
    for sql in dumps {
        let Some((dir, name)) = sql.rsplit_once('/') else {
            continue;
        };
        let ruby = format!("{dir}/{}", name.replace("structure.sql", "schema.rb"));
        if !files.contains_key(&ruby) {
            continue;
        }
        let app = &dir[..dir.len() - "db".len()];
        let config = read_text(root.join(format!("{app}config/application.rb")));
        match crate::schema::sql_is_the_schema(config.ok().as_deref()) {
            true => files.remove(&ruby),
            false => files.remove(&sql),
        };
    }
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
    hash(root, list(root, subdir))
}

/// `walk`'s paths, without reading a file: what is left out is never read.
pub(crate) fn list(root: &Path, subdir: &str) -> Vec<String> {
    let mut paths = Vec::new();
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
            if is_ruby(&relative) {
                paths.push(relative.into_owned());
            }
        }
    }
    paths
}

/// `paths` under `root`, hashed the way git would; one that cannot be read
/// is left out.
pub(crate) fn hash(root: &Path, paths: impl IntoIterator<Item = String>) -> Files {
    paths
        .into_iter()
        .filter_map(|path| {
            let bytes = read_source(root.join(&path)).ok()?;
            let oid = hash_blob(&bytes);
            Some((path, oid))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_source_is_a_regular_file_of_bounded_size() {
        let temp = std::env::temp_dir().join(format!("trekr-source-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).unwrap();
        let file = temp.join("a.rb");
        std::fs::write(&file, "x = 1\n").unwrap();
        assert_eq!(read_source(&file).unwrap(), b"x = 1\n");
        std::os::unix::fs::symlink(&file, temp.join("linked.rb")).unwrap();
        assert_eq!(read_source(temp.join("linked.rb")).unwrap(), b"x = 1\n");

        // A device never ends, and a pipe would wait for a writer.
        let fifo = temp.join("pipe.rb");
        assert!(
            Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );
        for path in [Path::new("/dev/zero"), &fifo, &temp] {
            let error = read_source(path).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput, "{path:?}");
        }

        let huge = temp.join("huge.rb");
        std::fs::File::create(&huge)
            .unwrap()
            .set_len(MAX_SOURCE + 1)
            .unwrap();
        let error = read_source(&huge).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::FileTooLarge);
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn a_file_is_read_by_the_reader_its_path_names() {
        for (path, reader) in [
            ("app/models/widget.rb", Reader::Ruby),
            ("bin/rails", Reader::Ruby),
            ("app/views/a/show.html.erb", Reader::Erb),
            ("app/views/a/show.json.rabl", Reader::Rabl),
            ("db/structure.sql", Reader::StructureSql),
            ("db/seeds.sql", Reader::Ruby),
        ] {
            assert_eq!(Reader::of(path), reader, "{path}");
        }
    }

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
        // A ceiling between the start and the repository hides it.
        let ceiling = main.to_string_lossy();
        assert_eq!(discover_under(&main.join("lib/deep"), &ceiling), None);
        assert_eq!(
            discover_under(&main, &ceiling),
            Some(toplevel(&main)),
            "the start itself is looked at"
        );
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
    fn a_schema_dump_is_read_from_an_apps_db_directory() {
        for path in [
            "db/structure.sql",
            "engines/shop/db/structure.sql",
            "db/animals_structure.sql",
        ] {
            assert!(is_indexed(path), "{path}");
        }
        for path in ["structure.sql", "db/seeds.sql", "docs/db/structure.sql.md"] {
            assert!(!is_indexed(path), "{path}");
        }
    }

    #[test]
    fn an_app_with_both_dumps_reads_the_one_rails_loads() {
        let temp = std::env::temp_dir().join(format!("trekr-dumps-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(temp.join("sql/config")).unwrap();
        std::fs::write(
            temp.join("sql/config/application.rb"),
            "config.active_record.schema_format = :sql\n",
        )
        .unwrap();
        let oid = || Oid("0".into());
        let mut files: Files = [
            "db/schema.rb",
            "db/structure.sql",
            "sql/db/schema.rb",
            "sql/db/structure.sql",
            "only/db/structure.sql",
        ]
        .into_iter()
        .map(|path| (path.to_string(), oid()))
        .collect();
        one_schema_per_app(&temp, &mut files);
        let kept: Vec<&str> = files.keys().map(String::as_str).collect();
        assert_eq!(
            kept,
            [
                "db/schema.rb",
                "only/db/structure.sql",
                "sql/db/structure.sql"
            ],
            "Rails' default is schema.rb; the config can say otherwise"
        );
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn one_file_is_admitted_exactly_when_the_scan_lists_it() {
        let temp = std::env::temp_dir().join(format!("trekr-admits-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .args(args)
                .current_dir(&temp)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?}");
        };
        let paths = [
            "tracked.rb",
            "ignored/forced.rb",
            "notes.txt",
            "db/schema.rb",
            "db/structure.sql",
            "untracked.rb",
            "fresh/new.rb",
            "ignored/copy.rb",
            "tmp/scratch.rb",
            "missing.rb",
        ];
        for path in &paths[..paths.len() - 1] {
            let file = temp.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, "class A; end\n").unwrap();
        }
        std::fs::write(temp.join(".gitignore"), "ignored/\n*scratch*\n").unwrap();
        git(&["init", "-q"]);
        git(&["add", "tracked.rb", "notes.txt", "db", ".gitignore"]);
        git(&["add", "-f", "ignored/forced.rb"]);
        git(&[
            "-c",
            "user.name=x",
            "-c",
            "user.email=x@x",
            "commit",
            "-qm",
            "x",
        ]);
        let root = std::fs::canonicalize(&temp).unwrap();
        let scanned = scan(&root).unwrap();
        for path in paths {
            assert_eq!(
                admits(&root, path, None).unwrap().as_deref(),
                scanned.contains_key(path).then_some(path),
                "{path}: one file's answer is the scan's"
            );
        }
        assert!(
            admits(&root, "fresh/new.rb", None).unwrap().is_some(),
            "an untracked file is read"
        );
        assert!(
            admits(&root, "ignored/copy.rb", None).unwrap().is_none(),
            "an ignored one is not"
        );
        let _ = std::fs::remove_dir_all(&temp);
    }

    /// A name written decomposed (NFD) is listed by macOS's git composed
    /// (`core.precomposeunicode`), and the scan keys it so: an editor's path
    /// spelled as on disk is admitted under the scan's key.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_decomposed_name_is_admitted_under_the_scans_key() {
        let temp = std::env::temp_dir().join(format!("trekr-admits-nfd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).unwrap();
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .args(args)
                .current_dir(&temp)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?}");
        };
        let (tracked, untracked) = ("nai\u{308}ve.rb", "cafe\u{301}.rb");
        std::fs::write(temp.join(tracked), "class A; end\n").unwrap();
        git(&["init", "-q"]);
        git(&["add", "-A"]);
        std::fs::write(temp.join(untracked), "class B; end\n").unwrap();
        let root = std::fs::canonicalize(&temp).unwrap();
        let scanned = scan(&root).unwrap();
        for (written, composed) in [(tracked, "na\u{ef}ve.rb"), (untracked, "caf\u{e9}.rb")] {
            assert!(scanned.contains_key(composed), "{composed}: {scanned:?}");
            assert_eq!(
                admits(&root, written, None).unwrap().as_deref(),
                Some(composed),
                "{written}"
            );
        }
        let _ = std::fs::remove_dir_all(&temp);
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
