//! `trekr --lsp` — the nine operations an agent actually uses, over stdio.
//!
//! A thin resident front over the on-disk index, not an owner of it (PLAN §4).
//! The editor owns the process: no auto-spawn, no lockfile, no lifecycle beyond
//! "stdin closed, so stop". Everything it answers, the CLI can answer too; what
//! it adds is not paying 210 ms to rebuild the tree on every keystroke.
//!
//! Deliberately absent, and permanently: completion, formatting, rename,
//! semantic tokens. Claude Code's `LSP` tool exposes nine operations and none of
//! those are among them (PLAN §1).

mod convert;
mod handlers;
pub(crate) mod log;
mod state;

use log::Log;
use lsp_server::{Connection, ExtractError, Message, Notification, Request, RequestId, Response};
use lsp_types::notification::Notification as _;
use lsp_types::request::Request as _;
use lsp_types::{
    HoverProviderCapability, OneOf, ServerCapabilities, TextDocumentSyncCapability,
    TextDocumentSyncKind,
};
use state::Session;
use std::path::PathBuf;

/// What this server tells a client it can do. Nothing here is aspirational —
/// every one is answered below.
fn capabilities() -> ServerCapabilities {
    ServerCapabilities {
        // Full text on every change: Ruby files are small and a full reparse is
        // microseconds, so incremental sync would be complexity bought with
        // nothing.
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        definition_provider: Some(OneOf::Left(true)),
        references_provider: Some(OneOf::Left(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        workspace_symbol_provider: Some(OneOf::Left(true)),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        implementation_provider: Some(lsp_types::ImplementationProviderCapability::Simple(true)),
        call_hierarchy_provider: Some(lsp_types::CallHierarchyServerCapability::Simple(true)),
        ..Default::default()
    }
}

pub(crate) fn run(verbose: bool) -> anyhow::Result<()> {
    let log = Log::open(verbose);
    // Said on stderr, once, because a log nobody can find is not observability.
    if let Some(path) = Log::where_to_look() {
        eprintln!("trekr: logging to {}", path.display());
    }
    log.event(
        "start",
        serde_json::json!({
            "pid": std::process::id(),
            "version": env!("CARGO_PKG_VERSION"),
            "cwd": std::env::current_dir().unwrap_or_default().to_string_lossy(),
            "binary": std::env::current_exe().ok().map(|p| p.to_string_lossy().into_owned()),
        }),
    );
    let (connection, threads) = Connection::stdio();
    // The connection owns the sender half of the writer thread's channel, so
    // it has to be *dropped* before joining — otherwise the writer never sees
    // the channel close and the join blocks forever. Taking it by value here
    // is what makes that happen.
    let result = serve(connection, &log);
    log.event(
        "stop",
        serde_json::json!({ "error": result.as_ref().err().map(|e| e.to_string()) }),
    );
    let outcome = result?;
    if outcome == Outcome::Retired {
        // Leave without joining. `join` waits on the reader first, and that
        // thread is parked in a blocking read on stdin which an editor holds
        // open — closing the descriptor turns the read into EOF on macOS but
        // NOT on Linux, where close(2) does not disturb a read already in
        // flight. Joining there hangs forever, having just logged that this
        // build retired: precisely the silent staleness retirement exists to
        // prevent. A retiring process has nothing left to unwind, so exit.
        std::process::exit(0);
    }
    threads.join()?;
    Ok(())
}

/// Why the serve loop ended — retirement has to skip the thread join below.
#[derive(PartialEq)]
enum Outcome {
    ShutDown,
    Retired,
}

fn serve(connection: Connection, log: &Log) -> anyhow::Result<Outcome> {
    let binary = Binary::current();
    let params = connection.initialize(serde_json::to_value(capabilities())?)?;
    let root = workspace_root(&params);
    log.event(
        "initialize",
        serde_json::json!({
            // The defect this log was written for: a client whose root is not
            // the repo the queried file lives in. Recording both is what makes
            // that legible instead of a guess.
            "root": root.to_string_lossy(),
            "client": params.get("clientInfo").and_then(|c| c.get("name")),
            "root_uri": params.get("rootUri"),
            "workspace_folders": params
                .get("workspaceFolders")
                .and_then(|f| f.as_array())
                .map(|f| f.len()),
        }),
    );
    log.detail("initialize_params", || params.clone());

    let store = crate::store::open_default()?;
    let mut session = Session::open(root, store);

    for message in &connection.receiver {
        match message {
            Message::Request(request) => {
                if connection.handle_shutdown(&request)? {
                    log.event("shutdown", serde_json::json!({}));
                    return Ok(Outcome::ShutDown);
                }
                let response = dispatch(&mut session, request, log);
                connection.sender.send(Message::Response(response))?;
                // Answer first, then check whether this build is still the
                // current one. A server that keeps serving after its binary
                // has been replaced is silent staleness — the bug class this
                // engine hunts everywhere else — and it cost two manual
                // `pkill`s a session to notice.
                if let Some(built) = &binary
                    && built.superseded()
                {
                    log.event(
                        "retire",
                        serde_json::json!({
                            "reason": "the binary on disk is newer than this process",
                            "path": built.path.to_string_lossy(),
                        }),
                    );
                    // The response above went out over a rendezvous channel,
                    // so the writer already has it and flushes before it can
                    // do anything else.
                    return Ok(Outcome::Retired);
                }
            }
            Message::Notification(notification) => {
                let method = notification.method.clone();
                let published = notify(&mut session, notification);
                log.event(
                    "notification",
                    serde_json::json!({
                        "op": method,
                        "diagnostics": published.is_some(),
                    }),
                );
                if let Some(diagnostics) = published {
                    connection.sender.send(diagnostics)?;
                }
            }
            Message::Response(_) => {}
        }
    }
    // The channel closed: the client went away without a shutdown request.
    Ok(Outcome::ShutDown)
}

/// The executable this process is running, and when it was written.
///
/// Checked after each request so a replaced binary is noticed within one query
/// rather than whenever somebody thinks to look. The editor owns the process
/// lifecycle (PLAN §1), so retiring is the whole mechanism: exit cleanly and
/// the client spawns the new build on its next request.
struct Binary {
    path: PathBuf,
    modified: std::time::SystemTime,
}

impl Binary {
    fn current() -> Option<Binary> {
        let path = std::env::current_exe().ok()?;
        let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
        Some(Binary { path, modified })
    }

    /// Has the file been replaced since this process started?
    ///
    /// A missing or unreadable file is *not* superseded: a binary mid-replace
    /// would otherwise retire every server on the machine at once.
    fn superseded(&self) -> bool {
        std::fs::metadata(&self.path)
            .and_then(|meta| meta.modified())
            .is_ok_and(|now| now > self.modified)
    }
}

/// The workspace folder, from whichever field the client used.
fn workspace_root(params: &serde_json::Value) -> PathBuf {
    let folder = params
        .get("workspaceFolders")
        .and_then(|f| f.as_array())
        .and_then(|f| f.first())
        .and_then(|f| f.get("uri"))
        .and_then(|u| u.as_str())
        .or_else(|| params.get("rootUri").and_then(|u| u.as_str()));
    let root = folder
        .and_then(convert::uri_to_path)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    // The store keys checkouts on git's canonical path. An editor sends the
    // path the user typed, and on macOS `/var` is a symlink to `/private/var` —
    // so without this the tree is looked up under a root that does not exist
    // and comes back empty, silently.
    std::fs::canonicalize(&root).unwrap_or(root)
}

fn dispatch(session: &mut Session, request: Request, log: &Log) -> Response {
    let id = request.id.clone();
    let method = request.method.clone();
    let asked = asked_about(&request.params);
    log.detail("request_params", || request.params.clone());

    let started = std::time::Instant::now();
    let result = route(session, request);
    let elapsed = started.elapsed();

    let (status, answered) = match &result {
        Ok(value) => ("ok", shape(value)),
        Err(_) => ("error", None),
    };
    log.event(
        "request",
        serde_json::json!({
            "op": method,
            "file": asked.0,
            "line": asked.1,
            // Two significant figures: a sub-millisecond timer is not evidence
            // for a third.
            "ms": round2(elapsed.as_secs_f64() * 1000.0),
            "status": status,
            "answered": answered,
            "error": result.as_ref().err().map(|e| e.to_string()),
        }),
    );

    match result {
        Ok(value) => Response {
            id,
            result: Some(value),
            error: None,
        },
        Err(error) => Response {
            id,
            result: None,
            error: Some(lsp_server::ResponseError {
                code: lsp_server::ErrorCode::InternalError as i32,
                message: error.to_string(),
                data: None,
            }),
        },
    }
}

fn route(session: &mut Session, request: Request) -> anyhow::Result<serde_json::Value> {
    use lsp_types::request as req;
    match request.method.as_str() {
        req::GotoDefinition::METHOD => run_handler(request, |p| handlers::definition(session, p)),
        req::References::METHOD => run_handler(request, |p| handlers::references(session, p)),
        req::DocumentSymbolRequest::METHOD => {
            run_handler(request, |p| handlers::document_symbol(session, p))
        }
        req::WorkspaceSymbolRequest::METHOD => {
            run_handler(request, |p| handlers::workspace_symbol(session, p))
        }
        req::HoverRequest::METHOD => run_handler(request, |p| handlers::hover(session, p)),
        req::GotoImplementation::METHOD => {
            run_handler(request, |p| handlers::implementation(session, p))
        }
        req::CallHierarchyPrepare::METHOD => {
            run_handler(request, |p| handlers::prepare_call_hierarchy(session, p))
        }
        req::CallHierarchyIncomingCalls::METHOD => {
            run_handler(request, |p| handlers::incoming_calls(session, p))
        }
        req::CallHierarchyOutgoingCalls::METHOD => {
            run_handler(request, |p| handlers::outgoing_calls(session, p))
        }
        // Anything else: null rather than an error, so a client probing for a
        // capability it did not read gets a civil answer.
        _ => Ok(serde_json::Value::Null),
    }
}

/// The file and line a request is about, when its params name one. Enough to
/// reproduce the query from the log without recording the whole document.
fn asked_about(params: &serde_json::Value) -> (Option<String>, Option<u64>) {
    let document = params
        .get("textDocument")
        .or_else(|| params.get("item"))
        .and_then(|d| d.get("uri"))
        .and_then(|u| u.as_str())
        .map(str::to_string);
    let line = params
        .get("position")
        .and_then(|p| p.get("line"))
        .and_then(serde_json::Value::as_u64)
        // LSP counts from zero; the log speaks the same 1-based lines the CLI
        // does, so a line copied out of it can be pasted into `--def`.
        .map(|line| line + 1);
    (document, line)
}

/// How much came back, without recording what. `null` is zero, and an empty
/// answer being *visible* is the whole point — that is the defect this log was
/// written to catch.
fn shape(value: &serde_json::Value) -> Option<usize> {
    match value {
        serde_json::Value::Null => Some(0),
        serde_json::Value::Array(items) => Some(items.len()),
        _ => Some(1),
    }
}

fn round2(ms: f64) -> f64 {
    (ms * 100.0).round() / 100.0
}

/// Deserialize a request's params, run the handler, serialize the answer.
fn run_handler<P, R>(
    request: Request,
    handler: impl FnOnce(P) -> anyhow::Result<R>,
) -> anyhow::Result<serde_json::Value>
where
    P: serde::de::DeserializeOwned,
    R: serde::Serialize,
{
    let params: P = serde_json::from_value(request.params)?;
    Ok(serde_json::to_value(handler(params)?)?)
}

/// Document lifecycle. Returns syntax diagnostics to publish, when there are
/// any to say something about.
///
/// Documents are keyed by their canonical absolute path, not by a
/// workspace-relative one: a session answers for several checkouts at once, and
/// two of them can each have an `app.rb`.
fn notify(session: &mut Session, notification: Notification) -> Option<Message> {
    use lsp_types::notification as note;
    match notification.method.as_str() {
        note::DidOpenTextDocument::METHOD => {
            let params: lsp_types::DidOpenTextDocumentParams =
                serde_json::from_value(notification.params).ok()?;
            let path = document_path(params.text_document.uri.as_str())?;
            session.did_open(path.clone(), params.text_document.text);
            handlers::diagnostics(session, &path, params.text_document.uri)
        }
        note::DidChangeTextDocument::METHOD => {
            let params: lsp_types::DidChangeTextDocumentParams =
                serde_json::from_value(notification.params).ok()?;
            let path = document_path(params.text_document.uri.as_str())?;
            // FULL sync, so the last change carries the whole document.
            let text = params.content_changes.into_iter().next_back()?.text;
            session.did_open(path.clone(), text);
            handlers::diagnostics(session, &path, params.text_document.uri)
        }
        note::DidCloseTextDocument::METHOD => {
            let params: lsp_types::DidCloseTextDocumentParams =
                serde_json::from_value(notification.params).ok()?;
            session.did_close(&document_path(params.text_document.uri.as_str())?);
            None
        }
        _ => None,
    }
}

/// A document URI as the one path this session will key it by. Canonical, so
/// `/var` and `/private/var` are the same document.
fn document_path(uri: &str) -> Option<PathBuf> {
    let path = convert::uri_to_path(uri)?;
    Some(std::fs::canonicalize(&path).unwrap_or(path))
}

/// Kept so the unused-import lint does not fire on the error types the
/// dispatcher's shape implies.
#[allow(dead_code)]
fn _unused(_: ExtractError<Request>, _: RequestId) {}
