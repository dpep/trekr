//! Which features get used, by whom, how often they come back empty, and how
//! slow — counted, never logged (DEC-063).
//!
//! One row per day × surface × feature × flags × caller × outcome × latency
//! bucket, holding a count. No query text, no paths, no repository names: the
//! table answers "is `--explain` used" and "how often does `--refs` come back
//! empty for agents", and nothing about what anyone was looking at.
//!
//! Its own small database, `trekr.usage.db` beside the store, and not a table
//! in it: the store is a cache that a VERSION bump drops (DEC-009), and this is
//! the one thing trekr keeps that cannot be rebuilt. `$TREKR_USAGE` points it
//! elsewhere, or `off` turns it off.
//!
//! A count must never cost the caller an answer: every failure here is
//! swallowed, and callers record only after their output is written.

pub(crate) mod origin;

use rusqlite::{Connection, params};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

/// Daily rows older than this are pruned on write. Long enough to see a
/// feature's adoption across a few releases, short enough that the file stays
/// a few hundred KB whatever the traffic.
pub(crate) const RETENTION_DAYS: u32 = 90;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS usage_daily (
  day     TEXT    NOT NULL,   -- local date, YYYY-MM-DD
  surface TEXT    NOT NULL,   -- cli | lsp
  feature TEXT    NOT NULL,   -- refs, def, definition, completion, reload, …
  flags   TEXT    NOT NULL,   -- canonical comma list of knobs, '' for none
  origin  TEXT    NOT NULL,   -- claude-code, human, piped, an editor, …
  outcome TEXT    NOT NULL,   -- hit | uncertain | empty | not-indexed | cancelled | error:<kind>
  latency TEXT    NOT NULL,   -- a bucket from `bucket`, '' when not timed
  cold    INTEGER NOT NULL,   -- 1 for an LSP session's first request
  count   INTEGER NOT NULL,
  PRIMARY KEY (day, surface, feature, flags, origin, outcome, latency, cold)
) WITHOUT ROWID;
";

/// How an operation ended. `Uncertain` is an answer with competitors or a low
/// confidence — answered, but not with the certainty the product promises.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Outcome {
    Hit,
    Uncertain,
    Empty,
    NotIndexed,
    Cancelled,
    Error(&'static str),
}

impl Outcome {
    pub(crate) fn label(&self) -> String {
        match self {
            Outcome::Hit => "hit".into(),
            Outcome::Uncertain => "uncertain".into(),
            Outcome::Empty => "empty".into(),
            Outcome::NotIndexed => "not-indexed".into(),
            Outcome::Cancelled => "cancelled".into(),
            Outcome::Error(kind) => format!("error:{kind}"),
        }
    }
}

/// Below this, a resolved answer counts as `Uncertain`: it is the point where
/// half the evidence disagrees (DEC-063).
pub(crate) const LOW_CONFIDENCE: f64 = 0.5;

/// A coarse latency bucket. Decades, because the question is "interactive or
/// not", and a finer timing of one invocation is noise, not evidence.
pub(crate) fn bucket(elapsed: Duration) -> &'static str {
    match elapsed.as_secs_f64() * 1000.0 {
        ms if ms < 1.0 => "<1ms",
        ms if ms < 10.0 => "<10ms",
        ms if ms < 100.0 => "<100ms",
        ms if ms < 1000.0 => "<1s",
        ms if ms < 10_000.0 => "<10s",
        _ => "10s+",
    }
}

/// The order of the buckets, fastest first.
pub(crate) const BUCKETS: [&str; 6] = ["<1ms", "<10ms", "<100ms", "<1s", "<10s", "10s+"];

/// One counted thing.
pub(crate) struct Tally<'a> {
    pub(crate) surface: &'static str,
    pub(crate) feature: &'a str,
    pub(crate) flags: String,
    pub(crate) origin: &'a str,
    pub(crate) outcome: Outcome,
    pub(crate) latency: Option<Duration>,
    pub(crate) cold: bool,
}

// ----- what the operation itself noticed -----
//
// The dispatcher knows the command and the clock; only the handler knows that
// the cursor was snapped, the answer was ambiguous, or the list was cut. A
// per-thread note carries that back without threading a parameter through
// every handler: both fronts run an operation start to finish on one thread.

#[derive(Default)]
pub(crate) struct Note {
    pub(crate) feature: Option<&'static str>,
    pub(crate) flags: BTreeSet<&'static str>,
    pub(crate) outcome: Option<Outcome>,
}

thread_local! {
    static NOTE: RefCell<Note> = RefCell::new(Note::default());
}

/// A knob or variant worth counting: `bare`, `snapped`, `cut`, `require`.
pub(crate) fn flag(name: &'static str) {
    NOTE.with(|n| n.borrow_mut().flags.insert(name));
}

/// The outcome, when the exit code alone would say less.
pub(crate) fn outcome(outcome: Outcome) {
    NOTE.with(|n| n.borrow_mut().outcome = Some(outcome));
}

/// Which feature answered, when the dispatcher could not tell in advance.
pub(crate) fn feature(name: &'static str) {
    NOTE.with(|n| n.borrow_mut().feature = Some(name));
}

/// Take and clear the note, so the next operation starts from nothing.
pub(crate) fn take() -> Note {
    NOTE.with(|n| std::mem::take(&mut *n.borrow_mut()))
}

/// Flags as a canonical string: sorted, comma-joined, so one call shape is one
/// row however it was spelled.
pub(crate) fn join(flags: &BTreeSet<&'static str>) -> String {
    flags.iter().copied().collect::<Vec<_>>().join(",")
}

// ----- storage -----

/// `$TREKR_USAGE`: a path, or `off`. Default is `<db>.usage.db` beside the
/// store, so an isolated `$TREKR_DB` gets isolated counts too.
pub(crate) fn path() -> Option<PathBuf> {
    match std::env::var("TREKR_USAGE").as_deref() {
        Ok("off") => None,
        Ok(path) => Some(PathBuf::from(path)),
        Err(_) => Some(
            crate::store::default_path()
                .ok()?
                .with_extension("usage.db"),
        ),
    }
}

/// A counter that is never an error. `None` inside when counting is off or
/// the file could not be opened.
pub(crate) struct Recorder {
    conn: Option<Mutex<Connection>>,
}

impl Recorder {
    pub(crate) fn open() -> Recorder {
        Recorder {
            conn: path().and_then(|p| open(&p).ok()).map(Mutex::new),
        }
    }

    #[cfg(test)]
    pub(crate) fn off() -> Recorder {
        Recorder { conn: None }
    }

    pub(crate) fn record(&self, tally: &Tally) {
        let Some(conn) = &self.conn else { return };
        if let Ok(mut conn) = conn.lock() {
            let _ = write(&mut conn, tally);
        }
    }
}

fn open(path: &std::path::Path) -> rusqlite::Result<Connection> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = Connection::open(path)?;
    // A short wait: two processes counting at once is normal, and losing one
    // count beats making a caller wait on another's write.
    conn.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA busy_timeout=200;",
    )?;
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

fn write(conn: &mut Connection, tally: &Tally) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    // Local date, as rq's `usage_daily`: an evening's work filed under
    // tomorrow makes a per-day report quietly wrong.
    tx.execute(
        "INSERT INTO usage_daily (day, surface, feature, flags, origin, outcome, latency, cold, count)
         VALUES (date('now', 'localtime'), ?1, ?2, ?3, ?4, ?5, ?6, ?7, 1)
         ON CONFLICT(day, surface, feature, flags, origin, outcome, latency, cold)
         DO UPDATE SET count = count + 1",
        params![
            tally.surface,
            tally.feature,
            tally.flags,
            tally.origin,
            tally.outcome.label(),
            tally.latency.map(bucket).unwrap_or(""),
            tally.cold,
        ],
    )?;
    tx.execute(
        "DELETE FROM usage_daily WHERE day < date('now', 'localtime', ?1)",
        params![format!("-{RETENTION_DAYS} days")],
    )?;
    tx.commit()
}

/// One stored row, as `--usage --json` emits it.
#[derive(serde::Serialize, Clone)]
pub(crate) struct Row {
    pub(crate) day: String,
    pub(crate) surface: String,
    pub(crate) feature: String,
    pub(crate) flags: String,
    pub(crate) origin: String,
    pub(crate) outcome: String,
    pub(crate) latency: String,
    pub(crate) cold: bool,
    pub(crate) count: i64,
}

/// Rows from the last `days` days (all retained when `None`), newest first.
/// A file that does not exist yet is no rows, not an error.
pub(crate) fn read(path: &std::path::Path, days: Option<u32>) -> rusqlite::Result<Vec<Row>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let conn = open(path)?;
    let since = format!("-{} days", days.unwrap_or(RETENTION_DAYS).saturating_sub(1));
    let mut stmt = conn.prepare(
        "SELECT day, surface, feature, flags, origin, outcome, latency, cold, count
           FROM usage_daily
          WHERE day >= date('now', 'localtime', ?1)
          ORDER BY day DESC, surface, count DESC, feature, flags, origin, outcome, latency, cold",
    )?;
    stmt.query_map(params![since], |r| {
        Ok(Row {
            day: r.get(0)?,
            surface: r.get(1)?,
            feature: r.get(2)?,
            flags: r.get(3)?,
            origin: r.get(4)?,
            outcome: r.get(5)?,
            latency: r.get(6)?,
            cold: r.get(7)?,
            count: r.get(8)?,
        })
    })?
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tally(feature: &str, outcome: Outcome) -> Tally<'_> {
        Tally {
            surface: "cli",
            feature,
            flags: String::new(),
            origin: "human",
            outcome,
            latency: Some(Duration::from_millis(40)),
            cold: false,
        }
    }

    #[test]
    fn a_repeated_call_is_one_row_with_a_count() {
        let dir = std::env::temp_dir().join(format!("trekr-usage-unit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("u.db");
        let mut conn = open(&path).unwrap();
        write(&mut conn, &tally("refs", Outcome::Hit)).unwrap();
        write(&mut conn, &tally("refs", Outcome::Hit)).unwrap();
        write(&mut conn, &tally("refs", Outcome::Empty)).unwrap();
        let rows = read(&path, None).unwrap();
        assert_eq!(rows.len(), 2, "one row per distinct outcome");
        assert_eq!(rows[0].count, 2, "most counted first within a day");
        assert_eq!(rows[0].latency, "<100ms");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prunes_rows_past_retention_on_write() {
        let dir = std::env::temp_dir().join(format!("trekr-usage-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("u.db");
        let mut conn = open(&path).unwrap();
        conn.execute(
            "INSERT INTO usage_daily VALUES (date('now', 'localtime', '-200 days'), 'cli', 'def', '', 'human', 'hit', '', 0, 5)",
            [],
        )
        .unwrap();
        write(&mut conn, &tally("def", Outcome::Hit)).unwrap();
        let total: i64 = conn
            .query_row("SELECT SUM(count) FROM usage_daily", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total, 1, "the 200-day-old row is gone");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn buckets_are_decades() {
        assert_eq!(bucket(Duration::from_micros(300)), "<1ms");
        assert_eq!(
            bucket(Duration::from_millis(10)),
            "<100ms",
            "edges round up"
        );
        assert_eq!(bucket(Duration::from_secs(12)), "10s+");
    }

    #[test]
    fn the_note_is_taken_whole_and_cleared() {
        flag("snapped");
        flag("bare");
        outcome(Outcome::Uncertain);
        let note = take();
        assert_eq!(join(&note.flags), "bare,snapped");
        assert_eq!(note.outcome, Some(Outcome::Uncertain));
        assert!(take().flags.is_empty());
    }
}
