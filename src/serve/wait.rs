//! A request whose answer would be incomplete only because the checkout's
//! index is still being built waits for it, off the loop, rather than going
//! out as a silent empty (DEC-811).
//!
//! The rule is the CLI's (DEC-500): a question about the whole checkout —
//! references, implementations, callers, workspace symbols — waits for the
//! whole index; a position's question waits until the checkout has its first
//! part, and then only when it found nothing. A parked request is retried as
//! the index moves, answered once it is whole, and refused with
//! `RequestFailed` when the wait runs out or no index is coming.

use super::state::Session;
use lsp_server::{Request, Response};
use lsp_types::request::{self as req, Request as _};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// How long a request waits for the index: a cold index of discourse or
/// mastodon takes about 10 s, and the first part lands well before.
const WAIT: Duration = Duration::from_secs(30);

/// `WAIT`, or a test's: a test of the refusal cannot wait thirty seconds.
fn wait() -> Duration {
    static WAIT_FOR_TESTS: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *WAIT_FOR_TESTS.get_or_init(|| {
        std::env::var("TREKR_TEST_INDEX_WAIT_MS")
            .ok()
            .and_then(|ms| ms.parse().ok())
            .map_or(WAIT, Duration::from_millis)
    })
}

/// What a request needs of the index before its answer is whole.
#[derive(Clone, Copy, PartialEq)]
enum Need {
    /// All of it: a caller or an override may be in any file.
    Whole,
    /// Its own file and what that names, as a first part holds them.
    Position,
}

fn need_of(method: &str) -> Option<Need> {
    match method {
        req::References::METHOD
        | req::GotoImplementation::METHOD
        | req::CallHierarchyIncomingCalls::METHOD
        | req::WorkspaceSymbolRequest::METHOD => Some(Need::Whole),
        req::GotoDefinition::METHOD
        | req::GotoDeclaration::METHOD
        | req::CallHierarchyPrepare::METHOD
        | req::CallHierarchyOutgoingCalls::METHOD => Some(Need::Position),
        // Hover says it in its text and completion with `isIncomplete`
        // (DEC-320); the rest read the open file alone.
        _ => None,
    }
}

/// How far a checkout's index has got, to tell when a retry may differ.
#[derive(Clone, PartialEq)]
struct Seen {
    indexed: bool,
    warming: Option<(u64, u64, bool)>,
}

fn seen(session: &Session, root: &std::path::Path) -> Seen {
    Seen {
        indexed: session.indexed(root),
        warming: session.warming(root).map(|w| (w.read, w.of, w.interrupted)),
    }
}

/// What to do with an answer.
pub(super) enum Verdict {
    /// Send it.
    Answer,
    /// Hold it for the index of this checkout.
    Wait(PathBuf),
    /// Send this error in its place.
    Refuse(String),
}

/// Whether `response` to `request` is whole, or waits for `root`'s index.
/// `why` is the handler's reason for an empty answer.
pub(super) fn verdict(
    session: &mut Session,
    request: &Request,
    response: &Response,
    why: Option<&str>,
) -> Verdict {
    let Some(need) = need_of(&request.method) else {
        return Verdict::Answer;
    };
    let Some(result) = &response.result else {
        return Verdict::Answer;
    };
    // Nothing at the position, or a file trekr cannot read: no index helps.
    if why.is_some_and(super::handlers::index_free) {
        return Verdict::Answer;
    }
    let Some(root) = root_of(session, request) else {
        return Verdict::Answer;
    };
    let now = seen(session, &root);
    if now.indexed && now.warming.is_none() {
        return Verdict::Answer;
    }
    let empty = match result {
        serde_json::Value::Null => true,
        serde_json::Value::Array(items) => items.is_empty(),
        _ => false,
    };
    if need == Need::Position && now.indexed && !empty {
        // Found in the first part: answered, and said to be partial (DEC-331).
        return Verdict::Answer;
    }
    if coming(session, &root) {
        Verdict::Wait(root)
    } else if !session.background.enabled() {
        Verdict::Refuse(refusal(session, &root, false))
    } else {
        // An index that failed says so itself (DEC-039); one never asked
        // for — a root that is no Ruby project — is not waited for.
        Verdict::Answer
    }
}

/// Whether an index of `root` is running or about to: this session's, or
/// another process's whose mark is live.
fn coming(session: &Session, root: &std::path::Path) -> bool {
    session.background.coming(root)
        || session.will_index(root)
        || session.warming(root).is_some_and(|w| !w.interrupted)
}

/// The checkout a request is about: its document's, or for a workspace
/// symbol, the workspace's.
fn root_of(session: &mut Session, request: &Request) -> Option<PathBuf> {
    if request.method == req::WorkspaceSymbolRequest::METHOD {
        let root = session.root.clone();
        return session.locate_query(&root).map(|l| l.root);
    }
    let (uri, _) = super::asked_about(&request.params);
    let path = super::document_path(&uri?)?;
    session.locate_query(&path).map(|l| l.root)
}

/// What a request refused for the index says.
fn refusal(session: &Session, root: &std::path::Path, coming: bool) -> String {
    let checkout = crate::core::paths::pretty(&root.to_string_lossy());
    let state = match session.warming(root) {
        Some(w) if w.interrupted => format!("its index was cut short ({})", w.how_far()),
        Some(w) => format!("it is still being indexed ({})", w.how_far()),
        None => "it is not indexed yet".to_string(),
    };
    match coming {
        true => format!(
            "trekr has no whole answer for {checkout} yet: {state}. Ask again when \
             the index finishes."
        ),
        false => format!(
            "trekr has no whole answer for {checkout}: {state}, and this session \
             will not index it. Run `trekr --index` there, then ask again."
        ),
    }
}

/// A request held for an index.
pub(super) struct Parked {
    pub(super) request: Request,
    root: PathBuf,
    pub(super) since: Instant,
    seen: Seen,
}

impl Parked {
    pub(super) fn new(session: &Session, request: Request, root: PathBuf) -> Parked {
        Parked {
            request,
            seen: seen(session, &root),
            root,
            since: Instant::now(),
        }
    }

    /// The index moved since this was last tried, or is no longer coming:
    /// try again.
    pub(super) fn moved(&mut self, session: &Session) -> bool {
        let now = seen(session, &self.root);
        let moved = now != self.seen;
        self.seen = now;
        moved || !coming(session, &self.root)
    }

    /// Waited as long as a request waits: the refusal to send, if so.
    pub(super) fn expired(&self, session: &Session) -> Option<String> {
        (self.since.elapsed() >= wait()).then(|| refusal(session, &self.root, true))
    }

    /// What it is refused with when the session ends before the index does.
    pub(super) fn abandoned(&self, session: &Session) -> String {
        refusal(session, &self.root, true)
    }
}
