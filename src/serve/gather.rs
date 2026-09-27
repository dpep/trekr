//! How much of a references answer is kept, in what order, and what is said
//! about the rest.
//!
//! A common name in a monorepo has hundreds of thousands of call sites. An
//! editor cannot show them and an agent cannot read them, so the answer is
//! bounded — and a bounded answer has to say it is one, and has to keep the
//! part worth keeping: confirmed callers before possible ones (DEC-056).

use lsp_types::Location;

/// References kept per answer unless the client sets `referenceLimit`.
///
/// Measured on discourse: the confirmed tier of a method, the part a cut must
/// never lose, fits under this for all but a handful of methods, and the full
/// list is `trekr --refs` away.
pub(crate) const DEFAULT_LIMIT: usize = 1000;

/// Evidence first, then source order: the `refs::order` key, plus the column
/// so two calls on one line keep their order.
pub(super) type Key = (u8, u8, String, u32, u32);

/// Which references survive the limit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Policy {
    /// The best `limit` of every file, by evidence. Needs the whole scan,
    /// unless `limit` confirmed callers are already in hand.
    Best,
    /// The first `limit` found, files nearest the definition first. For a
    /// stream, which cannot take back what it sent, and for a name whose
    /// receiver is unknown, where "confirmed" is about *some* method of that
    /// name and a full scan to promote it would buy nothing.
    First,
}

pub(super) struct Gather {
    limit: usize,
    policy: Policy,
    kept: Vec<(Key, Location)>,
    /// How many of `kept` a stream has already sent.
    sent: usize,
    /// References seen, kept or not.
    pub(super) found: usize,
}

impl Gather {
    pub(super) fn new(limit: usize, policy: Policy) -> Gather {
        Gather {
            limit: limit.max(1),
            policy,
            kept: Vec::new(),
            sent: 0,
            found: 0,
        }
    }

    pub(super) fn offer(&mut self, key: Key, location: Location) {
        self.found += 1;
        match self.policy {
            Policy::First if self.kept.len() >= self.limit => {}
            Policy::First => self.kept.push((key, location)),
            Policy::Best => {
                self.kept.push((key, location));
                // Sorted and cut in batches: the memory stays within twice the
                // limit, and a sort per offer would be quadratic.
                if self.kept.len() >= 2 * self.limit {
                    self.trim();
                }
            }
        }
    }

    fn trim(&mut self) {
        self.kept.sort_by(|a, b| a.0.cmp(&b.0));
        self.kept.truncate(self.limit);
    }

    /// Nothing still unread could change the answer: the first `limit` are in
    /// hand, or `limit` confirmed callers are — no later site outranks those.
    pub(super) fn settled(&mut self) -> bool {
        match self.policy {
            Policy::First => self.kept.len() >= self.limit,
            Policy::Best => {
                if self.kept.len() < self.limit {
                    return false;
                }
                self.trim();
                self.kept.last().is_some_and(|(key, _)| key.0 == 0)
            }
        }
    }

    /// What a stream has not sent yet, best first.
    pub(super) fn batch(&mut self) -> Vec<Location> {
        let sent = std::mem::replace(&mut self.sent, self.kept.len());
        let fresh = &mut self.kept[sent..];
        fresh.sort_by(|a, b| a.0.cmp(&b.0));
        fresh.iter().map(|(_, location)| location.clone()).collect()
    }

    /// Everything kept, best first.
    pub(super) fn finish(mut self) -> Vec<Location> {
        self.trim();
        self.kept
            .into_iter()
            .map(|(_, location)| location)
            .collect()
    }

    pub(super) fn kept(&self) -> usize {
        self.kept.len().min(self.limit)
    }
}

/// What a cut answer says about itself. `None` when nothing was left out.
pub(super) struct Cut<'a> {
    /// The name as written, and the question to hand the CLI for the rest.
    pub(super) name: &'a str,
    pub(super) query: &'a str,
    pub(super) shown: usize,
    pub(super) found: usize,
    /// Files read, of those that call the name — counted only when they
    /// were listed up front.
    pub(super) read: usize,
    pub(super) files: Option<usize>,
    /// The scan stopped with files unread.
    pub(super) stopped: bool,
    pub(super) of: Of,
}

/// What the references are references to — it changes what the order means
/// and what the CLI can hand back.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Of {
    Method,
    /// The receiver never resolved, so every call of the name counts.
    BareName,
    /// A class, module or constant; `--refs` answers it by name only.
    Constant,
}

impl Cut<'_> {
    /// Whether anything was left out — the only case worth a word.
    pub(super) fn is_cut(&self) -> bool {
        self.found > self.shown || self.stopped
    }

    /// One line for the editor: how much is shown, of how much, and where
    /// the rest is.
    pub(super) fn message(&self) -> String {
        let of = if self.stopped {
            let files = match self.files {
                Some(files) => format!("{} of the {}", thousands(self.read), thousands(files)),
                None => format!("the first {}", thousands(self.read)),
            };
            format!(
                "{} references to `{}`, from {files} files that call it",
                thousands(self.shown),
                self.name,
            )
        } else {
            format!(
                "{} of {} references to `{}`",
                thousands(self.shown),
                thousands(self.found),
                self.name,
            )
        };
        let (order, rest) = match self.of {
            // Read to the end, the whole answer is ranked; stopped early, it
            // is ranked within what was read, which was the nearest files.
            Of::Method if self.stopped => (", nearest the definition first", "For all of them"),
            Of::Method => (", confirmed callers first", "For all of them"),
            Of::BareName => (
                "; the receiver's type is unknown, so every call of the name counts",
                "For all of them",
            ),
            Of::Constant => ("", "For every mention of the name"),
        };
        format!(
            "trekr: showing {of}{order}. {rest}: trekr --refs '{}'",
            self.query
        )
    }
}

fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// Files nearest `anchor` first: the definition's own file, then its
/// directory, then outward. A cut or a streamed prefix then comes from the
/// code most likely to be about this method — the same signal `refs`
/// proximity ranks by, read from the path instead of the tree.
pub(super) fn nearest_first(paths: &mut [String], anchor: &str) {
    let shared = |path: &str| {
        path.split('/')
            .zip(anchor.split('/'))
            .take_while(|(a, b)| a == b)
            .count()
    };
    paths.sort_by_cached_key(|path| (std::cmp::Reverse(shared(path)), path.clone()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(line: u32) -> Location {
        let start = lsp_types::Position { line, character: 0 };
        Location {
            uri: "file:///x.rb".parse().unwrap(),
            range: lsp_types::Range { start, end: start },
        }
    }

    fn key(tier: u8, line: u32) -> Key {
        (tier, 0, "x.rb".into(), line, 0)
    }

    fn lines(locations: &[Location]) -> Vec<u32> {
        locations.iter().map(|l| l.range.start.line).collect()
    }

    #[test]
    fn best_keeps_the_confirmed_over_earlier_possibles() {
        let mut gather = Gather::new(2, Policy::Best);
        for line in 0..5 {
            gather.offer(key(1, line), at(line));
        }
        gather.offer(key(0, 9), at(9));
        assert!(!gather.settled(), "a later confirmed could still displace");
        assert_eq!(gather.found, 6);
        assert_eq!(lines(&gather.finish()), [9, 0]);
    }

    #[test]
    fn best_settles_once_the_limit_is_all_confirmed() {
        let mut gather = Gather::new(2, Policy::Best);
        gather.offer(key(1, 0), at(0));
        gather.offer(key(0, 1), at(1));
        assert!(!gather.settled());
        gather.offer(key(0, 2), at(2));
        assert!(gather.settled());
    }

    #[test]
    fn first_stops_at_the_limit_and_streams_each_batch_best_first() {
        let mut gather = Gather::new(3, Policy::First);
        gather.offer(key(1, 0), at(0));
        gather.offer(key(0, 1), at(1));
        assert_eq!(lines(&gather.batch()), [1, 0]);
        gather.offer(key(1, 2), at(2));
        gather.offer(key(0, 3), at(3));
        assert!(gather.settled());
        assert_eq!(lines(&gather.batch()), [2], "the fourth is past the limit");
        assert!(gather.batch().is_empty(), "nothing is sent twice");
        assert_eq!(gather.found, 4);
    }

    #[test]
    fn a_cut_says_how_much_and_where_the_rest_is() {
        let full = Cut {
            name: "save",
            query: "Widget#save",
            shown: 1000,
            found: 5450,
            read: 40,
            files: Some(40),
            stopped: false,
            of: Of::Method,
        };
        assert!(full.is_cut());
        assert_eq!(
            full.message(),
            "trekr: showing 1,000 of 5,450 references to `save`, confirmed callers first. \
             For all of them: trekr --refs 'Widget#save'"
        );
        let early = Cut {
            name: "to",
            query: "to",
            shown: 1000,
            found: 1003,
            read: 212,
            files: None,
            stopped: true,
            of: Of::BareName,
        };
        assert!(
            early
                .message()
                .contains("1,000 references to `to`, from the first 212 files")
        );
        assert!(early.message().contains("receiver's type is unknown"));
        let whole = Cut {
            found: 12,
            shown: 12,
            stopped: false,
            ..early
        };
        assert!(!whole.is_cut());
        let listed = Cut {
            files: Some(3608),
            ..early
        };
        assert!(listed.message().contains("from 212 of the 3,608 files"));
    }

    #[test]
    fn nearest_first_starts_at_the_definition() {
        let mut paths = vec![
            "lib/z.rb".to_string(),
            "app/models/b.rb".to_string(),
            "app/jobs/a.rb".to_string(),
            "app/models/topic.rb".to_string(),
        ];
        nearest_first(&mut paths, "app/models/topic.rb");
        assert_eq!(
            paths,
            [
                "app/models/topic.rb",
                "app/models/b.rb",
                "app/jobs/a.rb",
                "lib/z.rb"
            ]
        );
    }
}
