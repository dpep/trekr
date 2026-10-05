//! Hot reload: a replaced binary takes over the running session (DEC-050).
//!
//! When the file this process was launched as changes — `brew upgrade`, a
//! reinstall, `cargo build` — the server waits for a moment with nothing in
//! flight, writes down what the client told it and will not say again, and
//! `exec`s the new build in its own place. The pid, stdin and stdout survive
//! exec, so the editor's connection carries on; the new process reads the
//! handoff instead of waiting for an `initialize` that is never coming.
//!
//! Prior art: contour's MCP restart (watch `argv[0]`, stamp by inode) and ae's
//! daemon step-down (snapshot identity at startup, compare on a tick).

use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

/// Set across the exec: the handoff file the new process resumes from.
const RESUME: &str = "TREKR_LSP_RESUME";

/// Set on a candidate binary to ask which handoff format it reads, instead of
/// serving.
const PROBE: &str = "TREKR_LSP_PROBE";

/// The handoff's shape. Bumped whenever a field changes meaning; a build that
/// reads a different one is retired to, not exec'd into.
const FORMAT: u32 = 1;

/// How long a candidate gets to answer the probe. It prints one line and
/// exits, so anything near this is a binary that is not going to work.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// [`PROBE_TIMEOUT`], or the e2e suite's longer one (`TREKR_TEST_PROBE_MS`):
/// a copied debug binary on a loaded test machine can take longer than that
/// to start, and those tests are about what the answer says, not how soon.
fn probe_timeout() -> Duration {
    static LIMIT: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *LIMIT.get_or_init(|| {
        std::env::var("TREKR_TEST_PROBE_MS")
            .ok()
            .and_then(|ms| ms.parse().ok())
            .map_or(PROBE_TIMEOUT, Duration::from_millis)
    })
}

/// How many times one stamp is asked after a transient refusal. A probe blocks
/// the session for up to [`PROBE_TIMEOUT`], so a binary that always hangs must
/// not be re-asked at every quiet moment.
const TRIES: u8 = 3;

/// How long after each transient refusal the stamp is asked again. Asked at
/// the next quiet moments instead, the tries landed seconds apart, inside the
/// one load spike that timed the first out, and stalled the session's
/// requests for each probe in turn.
const AGAIN_AFTER: [Duration; TRIES as usize - 1] =
    [Duration::from_secs(30), Duration::from_secs(120)];

/// What the client told this session that it will not say again.
#[derive(Serialize, Deserialize)]
pub(crate) struct Handoff {
    format: u32,
    /// The version that wrote it — for the log, not for compatibility.
    pub(crate) from: String,
    /// `initialize`'s params, verbatim: root, capabilities,
    /// `initializationOptions`, and the client's path spelling all derive
    /// from them.
    pub(crate) params: serde_json::Value,
    /// Registrations the client holds for us (`client/registerCapability`
    /// ids). Asking again would register a duplicate.
    pub(crate) registered: Vec<String>,
    /// The editor's buffers — unsaved text the disk does not have.
    pub(crate) documents: Vec<Buffer>,
    /// Bytes read off stdin and not yet a whole message.
    pub(crate) unread: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Buffer {
    pub(crate) path: PathBuf,
    pub(crate) version: i32,
    pub(crate) text: String,
}

impl Handoff {
    pub(crate) fn new(
        params: serde_json::Value,
        registered: Vec<String>,
        documents: Vec<Buffer>,
        unread: Vec<u8>,
    ) -> Handoff {
        Handoff {
            format: FORMAT,
            from: env!("CARGO_PKG_VERSION").to_string(),
            params,
            registered,
            documents,
            unread,
        }
    }
}

/// A candidate's answer to [`PROBE`], when this process is one: print the
/// format and leave. True when it answered.
pub(crate) fn answer_probe() -> bool {
    if std::env::var_os(PROBE).is_none() {
        return false;
    }
    println!(
        "{}",
        serde_json::json!({ "handoff": FORMAT, "version": env!("CARGO_PKG_VERSION") })
    );
    true
}

/// The handoff this process was exec'd to resume, if it was. The file is
/// removed as soon as it is read, whether or not it parses: it holds the
/// user's unsaved buffers, and nothing else will ever read it.
pub(crate) fn resuming() -> Option<anyhow::Result<Handoff>> {
    let path = PathBuf::from(std::env::var_os(RESUME)?);
    Some(read_handoff(&path))
}

fn read_handoff(path: &Path) -> anyhow::Result<Handoff> {
    let bytes = crate::scan::read_source(path);
    let _ = std::fs::remove_file(path);
    let handoff: Handoff = serde_json::from_slice(&bytes?)?;
    anyhow::ensure!(
        handoff.format == FORMAT,
        "handoff format {} from trekr {}, this build reads {FORMAT}",
        handoff.format,
        handoff.from
    );
    Ok(handoff)
}

/// Write the handoff where only this user can read it.
pub(crate) fn write_handoff(handoff: &Handoff) -> std::io::Result<PathBuf> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    // macOS clocks tick in microseconds, so the time alone can repeat
    // within one process; the counter can't.
    static WRITTEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nth = WRITTEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "trekr-lsp-{}-{nanos}-{nth}.handoff",
        std::process::id()
    ));
    // `create_new` refuses a file (or a symlink) already there, so a shared
    // /tmp cannot be used to redirect it.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    let written = serde_json::to_vec(handoff)
        .map_err(std::io::Error::other)
        .and_then(|bytes| file.write_all(&bytes));
    if let Err(error) = written {
        let _ = std::fs::remove_file(&path);
        return Err(error);
    }
    Ok(path)
}

/// Become the binary at `path`, resuming from `handoff`. Returns only on
/// failure — a successful exec never comes back.
pub(crate) fn exec(path: &Path, handoff: &Path) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    Command::new(path)
        .args(std::env::args_os().skip(1))
        .env(RESUME, handoff)
        .exec()
}

/// The binary this process was launched as, and what it looked like then.
pub(crate) struct Launched {
    path: PathBuf,
    stamp: Stamp,
    refused: Option<Refused>,
}

/// A changed stamp that was refused transiently: how often, and when it may
/// be asked again.
struct Refused {
    stamp: Stamp,
    tries: u8,
    until: Instant,
}

/// A binary's identity on disk, following symlinks.
///
/// The inode is the load-bearing field: an installer renames a new file over
/// the path, and `brew upgrade` re-points a symlink at a new Cellar file —
/// both a new inode, while a clone on APFS can carry the old mtime across.
/// Size and mtime catch a rewrite in place; the mode catches a `chmod +x` on
/// a file that was refused for not being executable. Stat'd, not hashed: it
/// is read at every quiet moment, and costs ~4 µs through brew's symlink.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Stamp {
    dev: u64,
    ino: u64,
    len: u64,
    modified: SystemTime,
    mode: u32,
}

impl Launched {
    /// Taken first thing, so an upgrade that lands during startup is seen as
    /// a change rather than baked into the baseline.
    pub(crate) fn now() -> Option<Launched> {
        let path = launch_path()?;
        let stamp = stamp_of(&path)?;
        Some(Launched {
            path,
            stamp,
            refused: None,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// What the launch path holds now, if that is not what this process
    /// started from. A path that has become unreadable is not a change: a
    /// binary mid-replace would otherwise look like one; nor is a stamp
    /// refused a moment ago, until its spacing is out.
    pub(crate) fn changed(&self) -> Option<Stamp> {
        let waiting = |now: &Stamp| {
            self.refused
                .as_ref()
                .is_some_and(|r| r.stamp == *now && Instant::now() < r.until)
        };
        stamp_of(&self.path).filter(|now| *now != self.stamp && !waiting(now))
    }

    /// Stop reporting this stamp as a change — it was tried and refused, and
    /// retrying at every quiet moment would spawn a probe per request.
    pub(crate) fn settle(&mut self, stamp: Stamp) {
        self.stamp = stamp;
        self.refused = None;
    }

    /// `stamp` was refused: whether it is asked again later. A transient
    /// refusal is, up to [`TRIES`] times, each [`AGAIN_AFTER`] the last;
    /// anything else is settled, and the next change to the file is tried
    /// afresh.
    pub(crate) fn refused(&mut self, stamp: Stamp, transient: bool) -> bool {
        let tries = match &self.refused {
            Some(refused) if refused.stamp == stamp => refused.tries + 1,
            _ => 1,
        };
        let again = transient && tries < TRIES;
        match again {
            true => {
                self.refused = Some(Refused {
                    stamp,
                    tries,
                    until: Instant::now() + AGAIN_AFTER[usize::from(tries) - 1],
                })
            }
            false => self.settle(stamp),
        }
        again
    }
}

/// The path to watch: the name this process was launched *as*, not the file
/// it resolves to (contour's `launch_path`, where both halves were measured).
///
/// `current_exe()` reads `/proc/self/exe` on Linux, which resolves through
/// brew's symlink to the old Cellar file and so never moves on an upgrade;
/// after a rename-over it reads `… (deleted)`. `argv[0]` is the stable name.
fn launch_path() -> Option<PathBuf> {
    let argv0 = PathBuf::from(std::env::args_os().next()?);
    if argv0.as_os_str().is_empty() {
        return None;
    }
    if argv0.components().count() > 1 {
        return match argv0.is_absolute() {
            true => Some(argv0),
            false => Some(std::env::current_dir().ok()?.join(argv0)),
        };
    }
    // A bare name — how an editor launches `trekr` — is resolved the way the
    // editor's spawn did.
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(&argv0))
        .find(|candidate| candidate.is_file())
        .or_else(|| std::env::current_exe().ok())
}

fn stamp_of(path: &Path) -> Option<Stamp> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).ok()?;
    Some(Stamp {
        dev: meta.dev(),
        ino: meta.ino(),
        len: meta.len(),
        modified: meta.modified().ok()?,
        mode: meta.mode(),
    })
}

/// What a candidate binary said when asked whether it can take over.
#[derive(Debug, PartialEq)]
pub(crate) enum Candidate {
    /// It reads this build's handoff.
    Resumable { version: String },
    /// It runs, but cannot read this build's handoff — older than hot reload,
    /// or a different format. The session cannot be carried across.
    Unresumable { reason: String },
    /// It does not run. `transient` when it is worth asking again: a file
    /// still open for writing (`ETXTBSY`) is one being installed, and a probe
    /// that timed out may have lost only to a loaded machine.
    Broken { reason: String, transient: bool },
}

/// Ask the binary at `path` whether it can resume this session — before
/// exec'ing it, because an exec that succeeds into a binary that then dies
/// takes the editor's connection with it, and there is no coming back.
pub(crate) fn probe(path: &Path) -> Candidate {
    probe_within(path, probe_timeout())
}

/// [`probe`], allowing the candidate `limit` for each answer.
fn probe_within(path: &Path, limit: Duration) -> Candidate {
    // A pre-reload build exits with an error once its stdin closes, so the
    // status says nothing here; the answer is what counts.
    let answer = match run_briefly(Command::new(path).arg("--lsp").env(PROBE, "1"), limit) {
        Ok((_, answer)) => answer,
        Err(error) => return broken(&error),
    };
    let said: Option<serde_json::Value> = serde_json::from_str(answer.trim()).ok();
    let format = said
        .as_ref()
        .and_then(|s| s.get("handoff"))
        .and_then(serde_json::Value::as_u64);
    let version = said
        .as_ref()
        .and_then(|s| s.get("version"))
        .and_then(|v| v.as_str())
        .unwrap_or("?")
        .to_string();
    match format {
        Some(format) if format == u64::from(FORMAT) => Candidate::Resumable { version },
        Some(format) => Candidate::Unresumable {
            reason: format!("trekr {version} reads handoff format {format}, not {FORMAT}"),
        },
        // No answer: a build from before hot reload, which served the probe as
        // an ordinary session and hung up on the closed stdin — or one that
        // does not run. `--version` tells them apart.
        None => match run_briefly(Command::new(path).arg("--version"), limit) {
            Ok((true, _)) => Candidate::Unresumable {
                reason: "the new binary predates hot reload".into(),
            },
            Ok((false, _)) => Candidate::Broken {
                reason: "the new binary exits with an error".into(),
                transient: false,
            },
            Err(error) => broken(&error),
        },
    }
}

fn broken(error: &std::io::Error) -> Candidate {
    Candidate::Broken {
        reason: error.to_string(),
        transient: matches!(
            error.kind(),
            std::io::ErrorKind::ExecutableFileBusy | std::io::ErrorKind::TimedOut
        ),
    }
}

/// Run a command to completion within `limit`, silently: whether it
/// succeeded, and its stdout. An error is a command that could not be run.
fn run_briefly(command: &mut Command, limit: Duration) -> std::io::Result<(bool, String)> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // A pre-reload build serves the probe as a session; keep it out of
        // the user's log.
        .env("TREKR_LOG", "off")
        .spawn()?;
    let deadline = Instant::now() + limit;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "did not answer",
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let mut out = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        stdout.read_to_string(&mut out)?;
    }
    Ok((status.success(), out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("trekr-reload-unit-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn a_handoff_round_trips_privately_and_is_read_once() {
        use std::os::unix::fs::PermissionsExt;
        let handoff = Handoff::new(
            serde_json::json!({"rootUri": "file:///w"}),
            vec!["trekr-watch".into()],
            vec![Buffer {
                path: "/w/app.rb".into(),
                version: 7,
                text: "unsaved ✓".into(),
            }],
            b"Content-Len".to_vec(),
        );
        let path = write_handoff(&handoff).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "unsaved buffers are the user's alone");

        let back = read_handoff(&path).unwrap();
        assert_eq!(back.documents[0].text, "unsaved ✓");
        assert_eq!(back.documents[0].version, 7);
        assert_eq!(back.unread, b"Content-Len");
        assert_eq!(back.registered, ["trekr-watch"]);
        assert!(!path.exists(), "removed once read");
    }

    #[test]
    fn a_handoff_in_another_format_is_refused_and_still_removed() {
        let mut handoff = Handoff::new(serde_json::json!({}), vec![], vec![], vec![]);
        handoff.format = FORMAT + 1;
        let path = write_handoff(&handoff).unwrap();
        assert!(read_handoff(&path).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn a_probe_tells_a_resumable_build_from_an_old_one_from_a_broken_one() {
        let dir = scratch("probe");
        let cases = [
            (
                script(
                    &dir,
                    "current",
                    &format!(r#"echo '{{"handoff":{FORMAT},"version":"9.9.9"}}'"#),
                ),
                "resumable",
            ),
            (
                script(&dir, "other-format", r#"echo '{"handoff":999}'"#),
                "unresumable",
            ),
            // Serves the probe as a session, fails on the closed stdin, and
            // still knows its version.
            (
                script(
                    &dir,
                    "old",
                    r#"[ "$1" = --version ] && echo 'trekr 0.1.5' && exit 0; exit 1"#,
                ),
                "unresumable",
            ),
            (script(&dir, "crashes", "exit 3"), "broken"),
            (dir.join("missing"), "broken"),
        ];
        // A shell script's exec can outlast `PROBE_TIMEOUT` on a loaded
        // machine; what is under test is how each answer is read.
        let probe = |path: &Path| probe_within(path, Duration::from_secs(60));
        for (path, expected) in cases {
            let got = match probe(&path) {
                Candidate::Resumable { .. } => "resumable",
                Candidate::Unresumable { .. } => "unresumable",
                Candidate::Broken { .. } => "broken",
            };
            assert_eq!(got, expected, "{}", path.display());
        }
        assert_eq!(
            probe(&dir.join("current")),
            Candidate::Resumable {
                version: "9.9.9".into()
            }
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_relinked_symlink_is_a_change_and_an_unreadable_path_is_not() {
        let dir = scratch("stamp");
        let old = script(&dir, "v1", "true");
        let new = script(&dir, "v2", "true");
        let link = dir.join("trekr");
        std::os::unix::fs::symlink(&old, &link).unwrap();
        let launched = Launched {
            stamp: stamp_of(&link).unwrap(),
            path: link.clone(),
            refused: None,
        };
        assert!(launched.changed().is_none());

        std::fs::remove_file(&link).unwrap();
        assert!(launched.changed().is_none(), "mid-replace is not a change");

        std::os::unix::fs::symlink(&new, &link).unwrap();
        assert!(launched.changed().is_some(), "brew's relink is");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_probe_that_timed_out_or_found_the_file_busy_is_asked_again() {
        use std::io::{Error, ErrorKind};
        for (kind, transient) in [
            (ErrorKind::TimedOut, true),
            (ErrorKind::ExecutableFileBusy, true),
            (ErrorKind::NotFound, false),
            (ErrorKind::PermissionDenied, false),
        ] {
            let Candidate::Broken { transient: got, .. } = broken(&Error::from(kind)) else {
                panic!("an error is a broken candidate");
            };
            assert_eq!(got, transient, "{kind:?}");
        }
    }

    #[test]
    fn a_transient_refusal_is_retried_a_few_times_spaced_out_then_settled() {
        let dir = scratch("refused");
        let path = script(&dir, "trekr", "true");
        let old = stamp_of(&path).unwrap();
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        let mut launched = Launched {
            path,
            stamp: old,
            refused: None,
        };
        let new = launched.changed().expect("rewritten");

        // Each refusal waits out its spacing before the stamp is a change
        // again; the load that timed the probe out is still there at once.
        let mut asked = Vec::new();
        for _ in 0..TRIES {
            let again = launched.refused(new, true);
            asked.push(again);
            if again {
                assert!(launched.changed().is_none(), "asked again at once");
                let refused = launched.refused.as_mut().expect("backing off");
                let spacing = refused.until - Instant::now();
                assert!(spacing > Duration::from_secs(10), "{spacing:?}");
                refused.until = Instant::now();
                assert_eq!(launched.changed(), Some(new), "asked again, later");
            }
        }
        assert_eq!(
            asked.iter().filter(|again| **again).count(),
            usize::from(TRIES - 1)
        );
        assert!(launched.changed().is_none(), "settled after the last try");

        let mut launched = Launched {
            stamp: old,
            ..launched
        };
        assert!(
            !launched.refused(new, false),
            "a lasting refusal settles at once"
        );
        assert!(launched.changed().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
