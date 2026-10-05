//! The `miss` lines the language server writes to its log (`serve/miss.rs`,
//! DEC-083), read back for `--usage --misses`.

use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// One recorded miss, as `--usage --misses` shows it.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct Recorded {
    pub(crate) ts: String,
    pub(crate) op: String,
    pub(crate) file: String,
    pub(crate) line: u64,
    pub(crate) col: u64,
    #[serde(default)]
    pub(crate) token: String,
    pub(crate) outcome: String,
    #[serde(default)]
    pub(crate) why: Option<String>,
}

/// How much of the log's tail is read. The log is append-only and a month of
/// daily use is a couple of MB; the recent misses are what anyone acts on.
const TAIL_BYTES: u64 = 8 << 20;

/// The misses in the log's tail, oldest first, at or after `since` (an ISO
/// timestamp prefix, compared as text). A log that does not exist yet is no
/// misses.
pub(crate) fn read(log: &Path, since: Option<&str>) -> std::io::Result<Vec<Recorded>> {
    let mut file = match std::fs::File::open(log) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let len = file.metadata()?.len();
    let skip = len.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(skip))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    // Started mid-file: the first line is a fragment.
    let lines = text.lines().skip(usize::from(skip > 0));
    Ok(lines
        .filter(|line| line.contains(r#""event":"miss""#))
        .filter_map(|line| serde_json::from_str::<Recorded>(line).ok())
        .filter(|miss| since.is_none_or(|since| miss.ts.as_str() >= since))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_only_miss_events_since_a_time() {
        let dir = std::env::temp_dir().join(format!("trekr-miss-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let log = dir.join("lsp.log");
        let line = |ts: &str, event: &str| {
            format!(
                r#"{{"ts":"{ts}","event":"{event}","op":"definition","file":"/a.rb","line":3,"col":5,"token":"foo","outcome":"empty","why":null}}"#
            )
        };
        let text = [
            line("2026-01-01T00:00:00.000Z", "miss"),
            line("2026-01-02T00:00:00.000Z", "request"),
            line("2026-01-03T00:00:00.000Z", "miss"),
        ]
        .join("\n");
        std::fs::write(&log, text).unwrap();
        let misses = read(&log, Some("2026-01-02")).unwrap();
        assert_eq!(misses.len(), 1);
        assert_eq!(misses[0].ts, "2026-01-03T00:00:00.000Z");
        assert!(read(&dir.join("absent.log"), None).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
