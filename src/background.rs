//! An index run in the background: the side of DEC-322 and DEC-500 that
//! runs in the `trekr --index` child a language server or a query spawned —
//! the files it is asked to read first, and the priority it yields.

use crate::log::Log;
use std::path::{Path, PathBuf};

/// Set on the index child: this run is background work, so it lowers its own
/// CPU and disk priority. The child does it rather than the spawn, so a
/// `trekr --index` someone runs by hand stays at full speed.
pub(crate) const BACKGROUND: &str = "TREKR_BACKGROUND";

/// Is this an index run the LSP spawned?
pub(crate) fn in_background() -> bool {
    std::env::var_os(BACKGROUND).is_some()
}

/// Files the language server wants read first: the ones open in the editor,
/// a path a line on the index child's stdin, as the editor opens them
/// (DEC-322). Read on a thread of their own, so the index never waits for one.
#[derive(Default)]
pub(crate) struct Hints {
    sent: std::sync::Arc<std::sync::Mutex<Vec<PathBuf>>>,
    /// Someone may send any: this index was spawned by a language server.
    pub(crate) listening: bool,
}

impl Hints {
    /// Listen on stdin, in an index the language server or a query
    /// spawned (DEC-500), which `spawned` says. Never a
    /// terminal: a background job reading one would be stopped by the shell.
    pub(crate) fn listen(spawned: bool) -> Hints {
        let mut hints = Hints::default();
        // SAFETY: asks whether a descriptor is a terminal; nothing is read.
        if !spawned || unsafe { libc::isatty(0) } == 1 {
            return hints;
        }
        hints.listening = true;
        let sink = hints.sent.clone();
        let read = move || {
            use std::io::BufRead;
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else {
                    return;
                };
                if let Ok(mut sink) = sink.lock() {
                    sink.push(PathBuf::from(line));
                }
            }
        };
        // A query writes its one file and closes the pipe at spawn: read it
        // now, or the first part may be written without it.
        match in_background() {
            true => drop(std::thread::spawn(read)),
            false => read(),
        }
        hints
    }

    /// Also read the hints other processes append to `file` (DEC-512),
    /// from its start, as they arrive. Someone may send some now.
    pub(crate) fn tail(&mut self, file: PathBuf) {
        self.listening = true;
        let sink = self.sent.clone();
        std::thread::spawn(move || {
            use std::io::{Read, Seek};
            let (mut at, mut partial) = (0u64, Vec::new());
            loop {
                if let Ok(mut opened) = crate::store::early::open_hints(&file)
                    && opened.seek(std::io::SeekFrom::Start(at)).is_ok()
                {
                    let mut more = Vec::new();
                    if let Ok(read) = opened.read_to_end(&mut more) {
                        at += read as u64;
                        partial.extend_from_slice(&more);
                    }
                }
                while let Some(end) = partial.iter().position(|b| *b == b'\n') {
                    let line: Vec<u8> = partial.drain(..=end).collect();
                    let line = String::from_utf8_lossy(&line[..end]).into_owned();
                    if let Ok(mut sink) = sink.lock() {
                        sink.push(PathBuf::from(line));
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        });
    }

    /// The hints that arrived since the last call, as they were sent: for a
    /// losing index to hand to the winning one.
    pub(crate) fn take_sent(&self) -> Vec<PathBuf> {
        self.sent
            .lock()
            .map(|mut sent| std::mem::take(&mut *sent))
            .unwrap_or_default()
    }

    /// Hints as if sent already.
    #[cfg(test)]
    pub(crate) fn sent(paths: &[PathBuf]) -> Hints {
        let hints = Hints {
            listening: true,
            ..Hints::default()
        };
        hints.sent.lock().unwrap().extend_from_slice(paths);
        hints
    }

    /// The hints that arrived since the last call, as paths in `root`.
    pub(crate) fn take(&self, root: &Path) -> Vec<String> {
        let Ok(mut hints) = self.sent.lock() else {
            return Vec::new();
        };
        std::mem::take(&mut *hints)
            .into_iter()
            .filter_map(|path| {
                let path = std::fs::canonicalize(&path).unwrap_or(path);
                Some(path.strip_prefix(root).ok()?.to_string_lossy().into_owned())
            })
            .collect()
    }
}

/// In an index run the LSP spawned: drop CPU priority by 10 and disk I/O to a
/// low but not starvable tier, and log what the kernel then holds — read
/// back, not assumed, so a refused request shows as `unchanged`. Any other
/// run: nothing.
///
/// Not the lowest I/O tier (macOS `IOPOL_THROTTLE`, Linux's idle class): the
/// index holds SQLite's write lock while it writes, so an I/O tier that can
/// be starved stretches the lock that a save or a CLI query waits on (DEC-062).
///
/// Call before the run starts a thread: on Linux both settings are
/// per-thread, and only threads created afterwards inherit them.
pub(crate) fn yield_if_background() {
    if !in_background() {
        return;
    }
    // Best-effort: a refusal just means a less polite index.
    // SAFETY: plain syscalls on this process; no memory crosses them.
    let nice = unsafe {
        libc::nice(10);
        lower_io();
        libc::getpriority(libc::PRIO_PROCESS, 0)
    };
    Log::open(false).event(
        "index_priority",
        serde_json::json!({ "pid": std::process::id(), "nice": nice, "io": io_class() }),
    );
}

// <sys/resource.h>; not in the libc crate. IOPOL_TYPE_DISK = 0,
// IOPOL_SCOPE_PROCESS = 0, IOPOL_UTILITY = 4.
#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn setiopolicy_np(iotype: libc::c_int, scope: libc::c_int, policy: libc::c_int) -> libc::c_int;
    fn getiopolicy_np(iotype: libc::c_int, scope: libc::c_int) -> libc::c_int;
}

#[cfg(target_os = "macos")]
unsafe fn lower_io() {
    // SAFETY: sets this process's own I/O policy; no memory is passed.
    unsafe { setiopolicy_np(0, 0, 4) };
}

#[cfg(target_os = "macos")]
fn io_class() -> &'static str {
    // SAFETY: a read of this process's own policy.
    match unsafe { getiopolicy_np(0, 0) } {
        4 => "utility",
        _ => "unchanged",
    }
}

// <linux/ioprio.h>: IOPRIO_WHO_PROCESS = 1, class above IOPRIO_CLASS_SHIFT =
// 13, IOPRIO_CLASS_BE = 2 at its lowest level, 7. No libc wrapper exists.
#[cfg(target_os = "linux")]
const BEST_EFFORT_LOWEST: libc::c_long = (2 << 13) | 7;

#[cfg(target_os = "linux")]
unsafe fn lower_io() {
    // SAFETY: sets this thread's own I/O priority; no memory is passed.
    unsafe { libc::syscall(libc::SYS_ioprio_set, 1, 0, BEST_EFFORT_LOWEST) };
}

#[cfg(target_os = "linux")]
fn io_class() -> &'static str {
    // SAFETY: a read of this thread's own I/O priority.
    match unsafe { libc::syscall(libc::SYS_ioprio_get, 1, 0) } {
        BEST_EFFORT_LOWEST => "best-effort-7",
        _ => "unchanged",
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
unsafe fn lower_io() {}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn io_class() -> &'static str {
    "unchanged"
}
