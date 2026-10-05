//! Why a command failed, as a stable `kind` and an exit code (DEC-067).
//!
//! Mirrors rq's `Failure` (rq DECISIONS D22): sysexits codes, one per remedy,
//! so no error shares a number with a verdict — `0` answered, `1` a definitive
//! nothing, `2` no answer yet (the checkout is not indexed).

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    /// The command line asks for something trekr can't do.
    Usage,
    /// A file or path the command names doesn't exist.
    NotFound,
    /// The path is real but no git checkout contains it.
    NotARepo,
    /// git couldn't be run, or failed for a reason other than the above.
    Git,
    /// trekr couldn't do something that should always work — a bug.
    Internal,
    /// The store can't be opened, read or written.
    Database,
    /// Any other file I/O: the tree snapshots, a file the index is reading.
    Io,
}

impl Failure {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Failure::Usage => "usage",
            Failure::NotFound => "not_found",
            Failure::NotARepo => "not_a_repo",
            Failure::Git => "git",
            Failure::Internal => "internal",
            Failure::Database => "database",
            Failure::Io => "io",
        }
    }

    /// Coarser than `kind`: one code per thing the caller does about it.
    pub(crate) fn exit_code(self) -> u8 {
        match self {
            Failure::Usage => 64,                        // EX_USAGE: fix the command
            Failure::NotFound | Failure::NotARepo => 66, // EX_NOINPUT: fix the path
            Failure::Git => 69,                          // EX_UNAVAILABLE
            Failure::Internal => 70,                     // EX_SOFTWARE
            Failure::Database | Failure::Io => 74,       // EX_IOERR
        }
    }

    /// An error of this kind, with this message.
    pub(crate) fn error(self, message: impl fmt::Display) -> anyhow::Error {
        Tagged {
            kind: self,
            error: anyhow::anyhow!("{message}"),
        }
        .into()
    }

    /// Which kind `error` is: the tag its origin gave it, else what the
    /// chain holds. Untagged and unrecognized is a failure trekr did not
    /// anticipate, which is `internal` by definition.
    pub(crate) fn of(error: &anyhow::Error) -> Failure {
        let chain = || error.chain();
        if let Some(tagged) = chain().find_map(|e| e.downcast_ref::<Tagged>()) {
            return tagged.kind;
        }
        if let Some(git) = chain().find_map(|e| e.downcast_ref::<crate::scan::GitError>()) {
            return match git.not_a_repo() {
                true => Failure::NotARepo,
                false => Failure::Git,
            };
        }
        if chain().any(|e| e.is::<rusqlite::Error>() || e.is::<crate::store::Unopenable>()) {
            Failure::Database
        } else if chain().any(|e| e.is::<std::io::Error>()) {
            Failure::Io
        } else {
            Failure::Internal
        }
    }
}

/// An error its origin has already classified.
///
/// Displays as the error it wraps and continues that error's chain, so
/// `{:#}` reads the same with or without the tag.
#[derive(Debug)]
struct Tagged {
    kind: Failure,
    error: anyhow::Error,
}

impl fmt::Display for Tagged {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.error, f)
    }
}

impl std::error::Error for Tagged {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.error.source()
    }
}

/// Classify a failure where it happens, when only the call site knows what
/// it means — an `io::ErrorKind::NotFound` is the caller's typo for a file
/// they named, and a bug anywhere else.
pub(crate) trait Tag<T> {
    fn tag(self, kind: Failure) -> anyhow::Result<T>;
}

impl<T, E: Into<anyhow::Error>> Tag<T> for Result<T, E> {
    fn tag(self, kind: Failure) -> anyhow::Result<T> {
        self.map_err(|error| {
            Tagged {
                kind,
                error: error.into(),
            }
            .into()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;

    #[test]
    fn a_tag_survives_context_and_leaves_the_message_alone() {
        let error = Err::<(), _>(std::io::Error::other("disk on fire"))
            .tag(Failure::NotFound)
            .context("reading widget.rb")
            .unwrap_err();
        assert_eq!(Failure::of(&error), Failure::NotFound);
        assert_eq!(format!("{error:#}"), "reading widget.rb: disk on fire");
    }

    #[test]
    fn an_untagged_error_is_classified_by_its_chain() {
        let io = anyhow::Error::new(std::io::Error::other("x")).context("snapshot");
        let sql = anyhow::Error::new(rusqlite::Error::InvalidQuery);
        let git = anyhow::Error::new(crate::scan::GitError::failed(
            "git rev-parse failed: fatal: not a git repository (or any of the parent directories): .git",
        ));
        for (error, want) in [
            (io, Failure::Io),
            (sql, Failure::Database),
            (
                anyhow::Error::new(crate::store::Unopenable("trekr store P: x".into())),
                Failure::Database,
            ),
            (git, Failure::NotARepo),
            (anyhow::anyhow!("a surprise"), Failure::Internal),
        ] {
            assert_eq!(Failure::of(&error), want, "{error:#}");
        }
    }

    #[test]
    fn every_error_code_is_clear_of_every_verdict() {
        use Failure::*;
        for kind in [Usage, NotFound, NotARepo, Git, Internal, Database, Io] {
            assert!(kind.exit_code() >= 64, "{kind:?} collides with 0, 1 or 2");
        }
    }
}
