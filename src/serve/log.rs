//! What the resident front did, as ndjson.
//!
//! Two consumers, and they want different things from one stream:
//!
//! * **debugging** — what the client sent at `initialize`, which requests
//!   arrived, what came back, and how long each took. The first live session
//!   returned empty for everything and we had to *guess* why; a log answers it
//!   in one read.
//! * **usage signal** — which of the nine operations an agent actually calls,
//!   against which checkouts, and how often the answer is empty. That is the
//!   feedback loop that picks what to build next.
//!
//! Never stdout: that is the LSP wire. Default is `serve.log` beside the
//! database, one compact object per line, at a level cheap enough to leave on.
//! `TREKR_LOG=-` sends it to stderr instead, `TREKR_LOG=off` silences it, and
//! `TREKR_LOG_LEVEL=debug` adds the wire-level params.

use serde_json::json;
use std::io::Write;
use std::sync::Mutex;

/// How much to say. Summary is the default because it is cheap and because a
/// log nobody leaves on answers nothing.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Level {
    /// One line per request: op, target, duration, what came back.
    Summary,
    /// Adds the raw params of every request and notification.
    Debug,
}

pub(crate) struct Log {
    sink: Option<Mutex<Sink>>,
    level: Level,
}

enum Sink {
    Stderr,
    File(std::fs::File),
}

impl Log {
    /// `$TREKR_LOG`: a path, `-` for stderr, `off` to silence. Default is
    /// `serve.log` beside the database.
    ///
    /// A log that cannot be opened is not an error worth refusing to serve
    /// over — it degrades to silence.
    pub(crate) fn open(verbose: bool) -> Log {
        let level = if verbose || env_is("TREKR_LOG_LEVEL", "debug") {
            Level::Debug
        } else {
            Level::Summary
        };
        let sink = match std::env::var("TREKR_LOG").as_deref() {
            Ok("off") => None,
            Ok("-") => Some(Sink::Stderr),
            Ok(path) => open_file(std::path::Path::new(path)).map(Sink::File),
            Err(_) => default_path()
                .as_deref()
                .and_then(open_file)
                .map(Sink::File),
        };
        Log {
            sink: sink.map(Mutex::new),
            level,
        }
    }

    pub(crate) fn debugging(&self) -> bool {
        self.sink.is_some() && self.level >= Level::Debug
    }

    /// One event. `fields` is merged into the line, so every event shares
    /// `ts` and `event` and adds its own.
    pub(crate) fn event(&self, event: &str, fields: serde_json::Value) {
        let Some(sink) = &self.sink else { return };
        let mut line = json!({ "ts": timestamp(), "event": event });
        if let (Some(object), Some(extra)) = (line.as_object_mut(), fields.as_object()) {
            for (key, value) in extra {
                object.insert(key.clone(), value.clone());
            }
        }
        let Ok(mut text) = serde_json::to_string(&line) else {
            return;
        };
        text.push('\n');
        // A failed write is dropped rather than propagated: logging must never
        // be able to take the server down or corrupt the wire.
        if let Ok(mut sink) = sink.lock() {
            let _ = match &mut *sink {
                Sink::Stderr => std::io::stderr().write_all(text.as_bytes()),
                Sink::File(file) => file.write_all(text.as_bytes()).and_then(|()| file.flush()),
            };
        }
    }

    /// Only paid when the level asks for it — the closure is not run otherwise,
    /// so a debug line costs nothing at summary level.
    pub(crate) fn detail(&self, event: &str, fields: impl FnOnce() -> serde_json::Value) {
        if self.debugging() {
            self.event(event, fields());
        }
    }

    /// Where the log is, for a message that tells a human where to look.
    pub(crate) fn where_to_look() -> Option<std::path::PathBuf> {
        match std::env::var("TREKR_LOG").as_deref() {
            Ok("off") | Ok("-") => None,
            Ok(path) => Some(std::path::PathBuf::from(path)),
            Err(_) => default_path(),
        }
    }
}

fn env_is(name: &str, value: &str) -> bool {
    std::env::var(name).is_ok_and(|v| v.eq_ignore_ascii_case(value))
}

fn default_path() -> Option<std::path::PathBuf> {
    let db = crate::store::default_path().ok()?;
    Some(db.parent()?.join("serve.log"))
}

fn open_file(path: &std::path::Path) -> Option<std::fs::File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
}

/// UTC, to the millisecond, ISO-8601. Sortable as text, which is what makes a
/// flat file greppable.
fn timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    iso8601(now.as_secs() as i64, now.subsec_millis())
}

fn iso8601(epoch_seconds: i64, millis: u32) -> String {
    let days = epoch_seconds.div_euclid(86_400);
    let seconds = epoch_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        seconds / 3600,
        (seconds % 3600) / 60,
        seconds % 60,
    )
}

/// Howard Hinnant's `civil_from_days`, the standard shift-the-epoch-to-March
/// trick that makes the leap day the last day of the year.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_a_sortable_utc_timestamp() {
        assert_eq!(iso8601(0, 0), "1970-01-01T00:00:00.000Z");
        // A leap day, which is where a hand-rolled calendar goes wrong.
        assert_eq!(iso8601(1_709_164_800, 42), "2024-02-29T00:00:00.042Z");
        assert_eq!(iso8601(1_735_689_599, 999), "2024-12-31T23:59:59.999Z");
    }

    #[test]
    fn a_log_with_nowhere_to_write_is_silent_rather_than_fatal() {
        // A directory is not an openable file, which is the closest stand-in
        // for "the sink failed": serving must continue regardless.
        let log = Log {
            sink: open_file(std::path::Path::new("/"))
                .map(Sink::File)
                .map(Mutex::new),
            level: Level::Debug,
        };
        assert!(!log.debugging());
        log.event("request", serde_json::json!({"op": "x"}));
    }
}
