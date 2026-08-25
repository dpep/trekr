//! Where did the index time actually go?
//!
//! Indexing has four phases with very different costs, and which one dominates
//! changes with the corpus and the machine. Guessing has been wrong before, so
//! `--index --profile` reports it: human-readable on stderr, structured on
//! stderr when `--json` is on, and free when the flag is absent.
//!
//! Timings go to **stderr** so that `trekr --index --json --profile | jq` still
//! sees only the answer on stdout.

use serde::Serialize;
use std::time::{Duration, Instant};

/// One named phase of an index run.
#[derive(Debug, Serialize)]
pub(crate) struct Phase {
    pub(crate) name: &'static str,
    pub(crate) ms: f64,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct Profile {
    phases: Vec<Phase>,
    /// Worker threads the parse phase was allowed to use.
    pub(crate) jobs: usize,
    /// Blobs this checkout references.
    pub(crate) blobs: usize,
    /// Blobs actually read and parsed.
    pub(crate) parsed: usize,
    /// Blobs already known, so never opened. The point of the blob store.
    pub(crate) skipped: usize,
    /// Bytes fed to the parser.
    pub(crate) bytes: u64,
    /// The slowest files to parse — this is what finds pathological inputs.
    slowest: Vec<SlowFile>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SlowFile {
    pub(crate) path: String,
    pub(crate) ms: f64,
    pub(crate) bytes: u64,
}

/// How many slow files to keep. Enough to see a pattern, few enough that the
/// report stays readable.
const SLOWEST: usize = 5;

impl Profile {
    /// Accumulate into the named phase rather than appending a second one.
    ///
    /// Indexing gems runs the same phases once per gem; without this a cold
    /// run on rails reports 258 entries instead of four.
    pub(crate) fn phase(&mut self, name: &'static str, elapsed: Duration) {
        let ms = elapsed.as_secs_f64() * 1000.0;
        match self.phases.iter_mut().find(|phase| phase.name == name) {
            Some(phase) => phase.ms += ms,
            None => self.phases.push(Phase { name, ms }),
        }
    }

    /// Keep only the worst few, so a 100k-file index does not accumulate a
    /// 100k-entry list to sort at the end.
    pub(crate) fn saw_file(&mut self, path: String, elapsed: Duration, bytes: u64) {
        let ms = elapsed.as_secs_f64() * 1000.0;
        if self.slowest.len() >= SLOWEST && self.slowest.last().is_some_and(|w| w.ms >= ms) {
            return;
        }
        self.slowest.push(SlowFile { path, ms, bytes });
        self.slowest
            .sort_by(|a, b| b.ms.partial_cmp(&a.ms).unwrap_or(std::cmp::Ordering::Equal));
        self.slowest.truncate(SLOWEST);
    }

    pub(crate) fn merge_files(&mut self, others: Vec<SlowFile>) {
        for file in others {
            self.saw_file(
                file.path,
                Duration::from_secs_f64(file.ms / 1000.0),
                file.bytes,
            );
        }
    }

    fn total_ms(&self) -> f64 {
        self.phases.iter().map(|p| p.ms).sum()
    }

    /// MB of source per second through the parser — the number that says
    /// whether more workers would help.
    fn parse_throughput(&self) -> Option<f64> {
        let parse = self.phases.iter().find(|p| p.name == "parse")?;
        (parse.ms > 0.0).then(|| (self.bytes as f64 / 1e6) / (parse.ms / 1000.0))
    }

    pub(crate) fn report_text(&self) {
        let total = self.total_ms();
        eprintln!("\nprofile — {:.0} ms total, {} jobs", total, self.jobs);
        for phase in &self.phases {
            let share = if total > 0.0 {
                100.0 * phase.ms / total
            } else {
                0.0
            };
            eprintln!("  {:<14} {:>8.0} ms  {:>5.1}%", phase.name, phase.ms, share);
        }
        eprintln!(
            "  {:<14} {} parsed, {} already known, {:.0} MB read",
            "blobs",
            self.parsed,
            self.skipped,
            self.bytes as f64 / 1e6
        );
        if let Some(throughput) = self.parse_throughput() {
            eprintln!(
                "  {:<14} {:.0} MB/s across {} jobs",
                "throughput", throughput, self.jobs
            );
        }
        if !self.slowest.is_empty() {
            eprintln!("  slowest files:");
            for file in &self.slowest {
                eprintln!(
                    "    {:>7.1} ms  {:>7} KB  {}",
                    file.ms,
                    file.bytes / 1024,
                    file.path
                );
            }
        }
    }

    pub(crate) fn report_json(&self) {
        match serde_json::to_string(self) {
            Ok(rendered) => eprintln!("{rendered}"),
            Err(error) => eprintln!("trekr: could not render profile: {error}"),
        }
    }
}

/// Time one phase, returning whatever it produced.
pub(crate) fn timed<T>(
    profile: &mut Option<Profile>,
    name: &'static str,
    work: impl FnOnce() -> T,
) -> T {
    // No flag, no clock: the profile must not be something you pay for when
    // you did not ask.
    let Some(profile) = profile else {
        return work();
    };
    let start = Instant::now();
    let result = work();
    profile.phase(name, start.elapsed());
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_phase_seen_twice_accumulates_rather_than_repeating() {
        let mut profile = Profile::default();
        profile.phase("parse", Duration::from_millis(10));
        profile.phase("parse", Duration::from_millis(5));
        assert_eq!(profile.phases.len(), 1);
        assert_eq!(profile.phases[0].ms, 15.0);
    }

    #[test]
    fn keeps_only_the_slowest_files() {
        let mut profile = Profile::default();
        for i in 1..20 {
            profile.saw_file(format!("f{i}.rb"), Duration::from_millis(i), 0);
        }
        assert_eq!(profile.slowest.len(), SLOWEST);
        assert_eq!(profile.slowest[0].path, "f19.rb", "the worst is first");
        assert!(profile.slowest[0].ms > profile.slowest[1].ms);
    }

    #[test]
    fn throughput_needs_a_parse_phase_to_divide_by() {
        let mut profile = Profile::default();
        assert!(profile.parse_throughput().is_none());
        profile.bytes = 2_000_000;
        profile.phase("parse", Duration::from_millis(1000));
        assert_eq!(profile.parse_throughput(), Some(2.0));
    }
}
