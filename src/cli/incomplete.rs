//! An index that stops before it finishes says how far it got, and never
//! exits 0 (DEC-400): a script that trusts the exit would otherwise answer
//! from a checkout most of which was never read.
//!
//! Two ways to stop: the write lock outwaited (DEC-139's wait ran out), which
//! exits 2, "no answer yet, ask again"; and a signal — Ctrl-C, a caller's
//! timeout — after which the process still dies of that signal, as a shell
//! expects. What the report says is read from the store, not from this
//! process: the commits a reader will see are what the checkout now holds.

use super::{Output, emit_json, paths, store_path, warming_note};
use crate::store::Store;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// The checkout being indexed, and how to report on it.
static INDEXING: OnceLock<(String, Output)> = OnceLock::new();
/// The index is in: a signal from here on is just a signal.
static DONE: AtomicBool = AtomicBool::new(false);

const SIGNALS: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

/// Take the stopping signals onto a thread of their own. Call before this
/// process starts any other thread: each inherits the mask, so the signal
/// can only arrive here. A child process does not — std resets it at spawn.
pub(super) fn watch_signals() {
    // SAFETY: a signal set built and installed as the calling thread's mask.
    let set = unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for signal in SIGNALS {
            libc::sigaddset(&mut set, signal);
        }
        if libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) != 0 {
            return;
        }
        set
    };
    std::thread::spawn(move || {
        let mut signal = 0;
        // SAFETY: waits on the set this function blocked.
        if unsafe { libc::sigwait(&set, &mut signal) } != 0 {
            return;
        }
        if !DONE.load(Ordering::SeqCst)
            && let Some((root, out)) = INDEXING.get()
        {
            report(*out, root, &format!("stopped by {}", name(signal)));
        }
        // Die of it, as the default action would have.
        // SAFETY: restores the default action and delivers the signal to
        // this thread, which no longer blocks it.
        unsafe {
            libc::signal(signal, libc::SIG_DFL);
            libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
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
    let store = store_path()
        .ok()
        .and_then(|p| Store::open_existing(&p).ok());
    // Whatever pid the mark names, its index is over once this one is.
    let warming = store
        .as_ref()
        .and_then(|s| s.warming(root).ok().flatten())
        .map(|w| crate::store::Warming {
            interrupted: true,
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
    if out != Output::Text {
        let _ = emit_json(
            out,
            &serde_json::json!({
                "repo": root,
                "status": "incomplete",
                "reason": why,
                "hint": hint,
                "warming": warming.as_ref().map(|w| warming_note(root, w)),
            }),
        );
    }
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

fn name(signal: libc::c_int) -> &'static str {
    match signal {
        libc::SIGINT => "SIGINT",
        libc::SIGTERM => "SIGTERM",
        _ => "SIGHUP",
    }
}
