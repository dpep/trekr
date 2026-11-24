//! An index that stops before it finishes says how far it got, and never
//! exits 0 (DEC-400): a script that trusts the exit would otherwise answer
//! from a checkout most of which was never read.
//!
//! Two ways to stop: the write lock outwaited (DEC-139's wait ran out), which
//! exits 2, "no answer yet, ask again"; and a signal — Ctrl-C, a caller's
//! timeout — after which the process still dies of that signal, as a shell
//! expects. What the report says is read from the store, not from this
//! process: the commits a reader will see are what the checkout now holds.

use super::{Output, json_text, paths, warming_note};
use crate::store::Store;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

/// The checkout being indexed, and how to report on it.
static INDEXING: OnceLock<(String, Output)> = OnceLock::new();
/// The index is in: a signal from here on is just a signal.
static DONE: AtomicBool = AtomicBool::new(false);

const SIGNALS: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

/// Where [`forward`] writes a signal for the watching thread to read.
static FORWARD_TO: AtomicI32 = AtomicI32::new(-1);

/// The handler: whichever thread the signal lands on, hand it to the
/// watcher. Only async-signal-safe calls, and errno left as it was found.
extern "C" fn forward(signal: libc::c_int) {
    let byte = signal as u8;
    // SAFETY: `errno_location` is this thread's errno; `write` is
    // async-signal-safe, to a non-blocking pipe that `watch_signals` opened.
    unsafe {
        let errno = *errno_location();
        libc::write(
            FORWARD_TO.load(Ordering::Relaxed),
            (&raw const byte).cast(),
            1,
        );
        *errno_location() = errno;
    }
}

#[cfg(target_os = "linux")]
unsafe fn errno_location() -> *mut libc::c_int {
    // SAFETY: libc's accessor for the calling thread's errno.
    unsafe { libc::__errno_location() }
}

#[cfg(not(target_os = "linux"))]
unsafe fn errno_location() -> *mut libc::c_int {
    // SAFETY: libc's accessor for the calling thread's errno.
    unsafe { libc::__error() }
}

/// Take the stopping signals onto a thread of their own: a handler forwards
/// each through a pipe, so it reaches the watcher whichever thread it lands
/// on, and threads started before this one need no mask. A child process
/// does not inherit the handler — exec resets it.
pub(super) fn watch_signals() {
    let mut fds = [0; 2];
    // SAFETY: a pipe into a two-element array, both ends close-on-exec and
    // the write end non-blocking, so a handler never waits on a full pipe;
    // then the handler installed for each stopping signal.
    unsafe {
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            return;
        }
        for fd in fds {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
        libc::fcntl(fds[1], libc::F_SETFL, libc::O_NONBLOCK);
        FORWARD_TO.store(fds[1], Ordering::Relaxed);
        for signal in SIGNALS {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = forward as extern "C" fn(libc::c_int) as libc::sighandler_t;
            action.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(signal, &action, std::ptr::null_mut());
        }
    }
    crate::stack::spawn(move || {
        let mut byte = 0u8;
        // SAFETY: reads one byte into `byte` from the pipe opened above.
        if unsafe { libc::read(fds[0], (&raw mut byte).cast(), 1) } != 1 {
            return;
        }
        let signal = libc::c_int::from(byte);
        if !DONE.load(Ordering::SeqCst)
            && let Some((root, out)) = INDEXING.get()
        {
            report(*out, root, &format!("stopped by {}", name(signal)));
        }
        // Die of it, as the default action would have.
        // SAFETY: restores the default action and delivers the signal.
        unsafe {
            libc::signal(signal, libc::SIG_DFL);
            libc::raise(signal);
        }
        std::process::exit(128 + signal);
    });
}

/// From here a stop is reported against `root`.
pub(super) fn indexing(root: &str, out: Output) {
    let _ = INDEXING.set((root.to_string(), out));
}

/// The index is in.
pub(super) fn finished() {
    DONE.store(true, Ordering::SeqCst);
}

/// The lock outwaited somewhere in `error`'s chain.
pub(super) fn outwaited(error: &anyhow::Error) -> bool {
    error
        .chain()
        .filter_map(|e| e.downcast_ref::<rusqlite::Error>())
        .any(crate::store::is_busy)
}

/// Say what the stopped index left: on stderr, and under `--json` as the
/// answer on stdout.
pub(super) fn report(out: Output, root: &str, why: &str) {
    let pretty = paths::pretty(root);
    let store = crate::store::default_path()
        .ok()
        .and_then(|p| Store::open_existing(&p).ok());
    // This index's own mark is over once it is; another writer's, which
    // outwaited this one, is over only if that writer is gone.
    let warming = store
        .as_ref()
        .and_then(|s| s.warming(root).ok().flatten())
        .map(|w| crate::store::Warming {
            interrupted: w.interrupted || w.pid == std::process::id(),
            ..w
        });
    let indexed = store
        .as_ref()
        .is_some_and(|s| s.has_checkout(root).unwrap_or(false));
    let hint = format!("trekr --index {pretty}");
    match &warming {
        Some(w) => eprintln!(
            "trekr: index incomplete: {} of {} files of {pretty} read — {why}; \
             answers from it are partial until: {hint}",
            w.read, w.of
        ),
        None if indexed => eprintln!(
            "trekr: index not finished — {why}; the earlier index of {pretty} \
             stands until: {hint}"
        ),
        None => eprintln!(
            "trekr: index not finished — {why}, before any of {pretty} was written; \
             to index it: {hint}"
        ),
    }
    use std::io::Write;
    // On Ctrl-C a reader down the pipe (`| tail`) has often gone already:
    // the closed pipe is ignored, so the process still dies of the signal
    // that stopped it, not of SIGPIPE, and a write error is no panic.
    // SAFETY: replaces SIGPIPE's action; this process is on its way out.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };
    let mut stdout = std::io::stdout();
    if out != Output::Text
        && let Ok(rendered) = json_text(
            out,
            &serde_json::json!({
                "repo": root,
                "status": "incomplete",
                "reason": why,
                "hint": hint,
                "warming": warming.as_ref().map(|w| warming_note(root, w)),
            }),
        )
    {
        let _ = writeln!(stdout, "{rendered}");
    }
    let _ = stdout.flush();
}

fn name(signal: libc::c_int) -> &'static str {
    match signal {
        libc::SIGINT => "SIGINT",
        libc::SIGTERM => "SIGTERM",
        _ => "SIGHUP",
    }
}
