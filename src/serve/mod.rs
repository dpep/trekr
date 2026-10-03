//! `trekr --lsp` — the nine operations an agent uses, and completion for the
//! editor, over stdio.
//!
//! A thin resident front over the on-disk index, not an owner of it (PLAN §4).
//! The editor owns the process: no auto-spawn, no lockfile, no lifecycle beyond
//! "stdin closed, so stop" — and, when the binary is replaced, becoming the new
//! one in place (`reload.rs`, DEC-050). Everything it answers, the CLI can
//! answer too; what it adds is not paying 210 ms to rebuild the tree on every
//! keystroke.
//!
//! Completion is here because the surface became an editor as well as an
//! agent's tool (DEC-040, reversing PLAN §1 for completion alone). Still
//! absent: formatting, rename, semantic tokens, type checking.

mod complete;
mod convert;
mod doc;
pub(crate) mod fresh;
mod gather;
mod handlers;
mod inbox;
pub(crate) mod log;
pub(crate) mod miss;
mod reload;
mod require;
mod schema;
mod state;
mod variables;
pub(crate) mod vars;
mod wire;

use crate::usage::Outcome;
use inbox::{Inbox, Next};
use log::Log;
use lsp_server::{Message, Notification, Request, RequestId, Response};
use lsp_types::notification::Notification as _;
use lsp_types::request::Request as _;
use lsp_types::{
    HoverProviderCapability, OneOf, ServerCapabilities, TextDocumentSyncCapability,
    TextDocumentSyncKind,
};
use state::Session;
use std::path::PathBuf;
use std::time::Duration;

/// What this server tells a client it can do. Nothing here is aspirational —
/// every one is answered below.
fn capabilities() -> ServerCapabilities {
    ServerCapabilities {
        // Full text on every change: Ruby files are small and a full reparse is
        // microseconds, so incremental sync would be complexity bought with
        // nothing. Saves are asked for because a save is when the index moves.
        text_document_sync: Some(TextDocumentSyncCapability::Options(
            lsp_types::TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::FULL),
                save: Some(lsp_types::TextDocumentSyncSaveOptions::SaveOptions(
                    lsp_types::SaveOptions {
                        include_text: Some(false),
                    },
                )),
                ..Default::default()
            },
        )),
        definition_provider: Some(OneOf::Left(true)),
        references_provider: Some(OneOf::Left(true)),
        document_highlight_provider: Some(OneOf::Left(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        workspace_symbol_provider: Some(OneOf::Left(true)),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        implementation_provider: Some(lsp_types::ImplementationProviderCapability::Simple(true)),
        // `require` strings, resolved to the file they load.
        document_link_provider: Some(lsp_types::DocumentLinkOptions {
            resolve_provider: Some(false),
            work_done_progress_options: Default::default(),
        }),
        call_hierarchy_provider: Some(lsp_types::CallHierarchyServerCapability::Simple(true)),
        completion_provider: Some(lsp_types::CompletionOptions {
            trigger_characters: Some(vec![".".into(), ":".into()]),
            // Docs are read per item, on demand: a list can be hundreds long.
            resolve_provider: Some(true),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// How often an otherwise idle server looks at its binary. A stat is ~4 µs,
/// so this is set by how stale an idle session may go, not by cost.
const RECHECK: Duration = Duration::from_secs(2);

pub(crate) fn run(verbose: bool) -> anyhow::Result<()> {
    if reload::answer_probe() {
        return Ok(());
    }
    // First, so an upgrade that lands while starting up is still a change.
    let launched = reload::Launched::now();
    let log = Log::open(verbose);
    let mut resumed = match reload::resuming() {
        None => None,
        Some(Ok(handoff)) => Some(handoff),
        Some(Err(error)) => {
            // The client will not send `initialize` again, so there is no
            // session to start either. Leaving is what is left: a client that
            // restarts a server that exits — VS Code's does — starts afresh.
            log.event(
                "resume",
                serde_json::json!({ "ok": false, "error": error.to_string() }),
            );
            log.count(
                "resume",
                String::new(),
                Outcome::Error("resume"),
                None,
                false,
            );
            return Err(error.context("resuming a hot-reloaded session"));
        }
    };
    if resumed.is_none() {
        // Said on stderr, once, because a log nobody can find is not
        // observability.
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
    }
    let unread = resumed
        .as_mut()
        .map(|handoff| std::mem::take(&mut handoff.unread))
        .unwrap_or_default();
    let inbox = Inbox::new(wire::Reader::stdin(unread)?);
    let writer = wire::Writer::stdout();
    let result = serve(&inbox, &writer, &log, launched, resumed);
    log.event(
        "stop",
        serde_json::json!({ "error": result.as_ref().err().map(|e| e.to_string()) }),
    );
    // Nothing reads stdin on another thread, so there is no reader to join —
    // and none to leave parked in a read an editor holds open, which is what
    // once made retiring hang on Linux.
    writer.finish();
    result
}

/// The handshake: wait for `initialize`, answer it with our capabilities, and
/// wait for `initialized`. lsp-server's, over this transport.
fn initialize(inbox: &Inbox, writer: &wire::Writer) -> anyhow::Result<serde_json::Value> {
    let (id, params) = loop {
        match inbox.next(None) {
            Next::Message(Message::Request(request)) if request.method == "initialize" => {
                break (request.id, request.params);
            }
            Next::Message(Message::Request(request)) => writer.send(
                Response::new_err(
                    request.id,
                    lsp_server::ErrorCode::ServerNotInitialized as i32,
                    format!("expected initialize request, got {}", request.method),
                )
                .into(),
            )?,
            Next::Message(Message::Notification(n))
                if n.method != lsp_types::notification::Exit::METHOD => {}
            Next::Idle => {}
            Next::Message(other) => anyhow::bail!("expected initialize request, got {other:?}"),
            Next::Closed => anyhow::bail!("the client disconnected before initialize"),
        }
    };
    let result = serde_json::json!({ "capabilities": capabilities() });
    writer.send(Response::new_ok(id, result).into())?;
    loop {
        match inbox.next(None) {
            Next::Message(Message::Notification(n)) if n.method == "initialized" => {
                return Ok(params);
            }
            Next::Idle => {}
            Next::Message(other) => {
                anyhow::bail!("expected initialized notification, got {other:?}")
            }
            Next::Closed => anyhow::bail!("the client disconnected before initialized"),
        }
    }
}

fn serve(
    inbox: &Inbox,
    writer: &wire::Writer,
    log: &Log,
    mut launched: Option<reload::Launched>,
    resumed: Option<reload::Handoff>,
) -> anyhow::Result<()> {
    let resuming = resumed.is_some();
    // A resumed session skips the handshake: the client did it with the
    // process this one replaced, and will not do it again.
    let (params, mut registered, buffers, from) = match resumed {
        Some(handoff) => (
            handoff.params,
            handoff.registered,
            handoff.documents,
            Some(handoff.from),
        ),
        None => (initialize(inbox, writer)?, Vec::new(), Vec::new(), None),
    };
    let root = workspace_root(&params);
    log.event(
        if resuming { "resume" } else { "initialize" },
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
            "from": from,
            "version": resuming.then_some(env!("CARGO_PKG_VERSION")),
            "documents": resuming.then_some(buffers.len()),
        }),
    );
    log.detail("initialize_params", || params.clone());
    let client_name = params
        .get("clientInfo")
        .and_then(|c| c.get("name"))
        .and_then(|n| n.as_str());
    log.set_client(client_name);
    // A resumed session is the same session under a new build: counted as a
    // resume, not a second start — but its first request is cold all the same,
    // since the successor starts with nothing warmed.
    log.count(
        if resuming { "resume" } else { "session" },
        String::new(),
        Outcome::Hit,
        None,
        false,
    );
    let mut first_request = true;

    let client = Client::from(&params);
    let spelling = Spelling::of(&params, &root);
    let store = crate::store::open_default()?;
    // Written now rather than at the first landing in core, where a failure
    // could only answer nothing: this is the one place that can say why.
    if let Err(error) = crate::store::core_dir() {
        log.event(
            "core_files_failed",
            serde_json::json!({ "error": format!("{error:#}") }),
        );
    }
    let mut session = Session::open(root.clone(), store);
    session.definition_links = client.definition_links;
    session.reference_limit = client.reference_limit;
    session.unresolved = client.unresolved;
    if let Some(value) = &client.unresolved_invalid {
        log.event(
            "setting_invalid",
            serde_json::json!({
                "setting": "unresolved",
                "value": value,
                "using": "confident",
                "valid": ["confident", "peek", "best", "none"],
            }),
        );
    }
    for buffer in buffers {
        session.did_open(buffer.path, buffer.text, buffer.version);
    }
    let mut indexer = fresh::Indexer::new(client.progress, client.index);

    if client.watch && !registered.iter().any(|id| id == WATCH) {
        // Changes the editor does not make — a checkout, a pull, a formatter —
        // only reach us if the client watches for them on our behalf.
        writer.send(watch_request())?;
        registered.push(WATCH.to_string());
    }
    // A Ruby project nobody has indexed: start now, so the first question
    // finds more than core and gems. Anywhere else waits to be asked about.
    // A resumed build whose store VERSION moved lands here too: opening the
    // store dropped the index (DEC-009), and answers are partial until this
    // background run refills it. So does one an index left unfinished
    // (DEC-320).
    let unfinished = session.warming(&root).is_some_and(|w| w.interrupted);
    if root.join("Gemfile").is_file() && (!session.indexed(&root) || unfinished) {
        indexer.want(root.clone(), false);
    }
    let mut warm = Warm::Cold;

    // Reopens this session, bounded: a store that reads as replaced straight
    // after reopening must not be reopened at every message.
    let mut reopens = 0;
    loop {
        session.collect_members(None);
        session.collect_trees();
        indexer.retry(&mut session);
        for message in indexer.poll(log, &session) {
            // A finished index moves the tree; warm it again when quiet.
            warm = Warm::Cold;
            writer.send(message)?;
        }
        session.reindexing = indexer.after_upgrade();
        session.background = indexer.view();
        // A new binary at the launch path is about to take over, with the
        // store it opens; reopening here would only make this one's.
        let replacing = launched.as_ref().is_some_and(|w| w.changed().is_some());
        if reopens < REOPENS
            && !replacing
            && let Some(why) = session.main_store().replaced()
        {
            reopens += 1;
            if let Some(message) = reopen(&mut session, &why, log) {
                indexer.reopened();
                indexer.want(root.clone(), true);
                warm = Warm::Cold;
                writer.send(message)?;
            }
        }
        // The one safe moment to become another program: nothing read and
        // unanswered, and no index child whose progress the successor could
        // not end.
        if let Some(watch) = &mut launched
            && !indexer.busy()
            && inbox.is_quiet()
            && let Some(stamp) = watch.changed()
        {
            let current = Current {
                params: &params,
                registered: &registered,
                session: &session,
                inbox,
                writer,
            };
            if swap(watch, stamp, current, log) == Swap::Retire {
                return Ok(());
            }
        }
        // Nothing to answer: build what the first questions would otherwise
        // pay for — the root's tree, then completion's member listing — one
        // step per quiet moment, so a request arriving meanwhile waits for at
        // most one of them.
        if warm < Warm::Done && inbox.is_quiet() {
            if session.indexed(&root) {
                let started = std::time::Instant::now();
                let (step, built) = match warm {
                    Warm::Cold => ("tree", session.tree(&root).is_ok()),
                    _ => ("members", session.list_members(&root).is_ok()),
                };
                log.event(
                    "warm",
                    serde_json::json!({
                        "step": step,
                        "ok": built,
                        "ms": started.elapsed().as_millis() as u64,
                    }),
                );
            }
            warm = warm.next();
        }
        // While warming, come straight back for the next step if nothing
        // arrived; while an index runs, wake periodically to notice it finish.
        let timeout = if warm < Warm::Done {
            Some(Duration::ZERO)
        } else if indexer.busy() || indexer.waiting() {
            Some(Duration::from_millis(250))
        } else {
            launched.is_some().then_some(RECHECK)
        };
        let message = match inbox.next(timeout) {
            Next::Message(message) => message,
            Next::Idle => continue,
            Next::Closed => break,
        };
        match message {
            Message::Request(request) => {
                if request.method == lsp_types::request::Shutdown::METHOD {
                    writer.send(Response::new_ok(request.id, ()).into())?;
                    log.event("shutdown", serde_json::json!({}));
                    await_exit(writer, inbox);
                    return Ok(());
                }
                let id = request.id.clone();
                let response = if inbox.is_cancelled(&id) {
                    // Withdrawn before its turn came: the cheapest answer, and
                    // the one that keeps a queue of stale hovers from being
                    // worked through one by one.
                    log.event(
                        "request",
                        serde_json::json!({ "op": request.method, "status": "cancelled" }),
                    );
                    let counted = Counted {
                        feature: feature_of(&request.method),
                        flags: String::new(),
                        outcome: Outcome::Cancelled,
                        latency: None,
                        miss: None,
                    };
                    (cancelled(id.clone()), counted)
                } else {
                    let cancel = || inbox.is_cancelled(&id);
                    let out = Outbound {
                        writer,
                        spelling: spelling.as_ref(),
                        log,
                    };
                    let (mut response, counted) = dispatch(&mut session, request, &out, &cancel);
                    if let (Some(spelling), Some(result)) = (&spelling, response.result.as_mut()) {
                        spelling.apply(result);
                    }
                    (response, counted)
                };
                inbox.settle(&id);
                writer.send(Message::Response(response.0))?;
                // Counted once the answer is on its way, never ahead of it.
                let counted = response.1;
                let cold = first_request && counted.latency.is_some();
                log.count(
                    &counted.feature,
                    counted.flags,
                    counted.outcome,
                    counted.latency,
                    cold,
                );
                if cold {
                    first_request = false;
                }
                if let Some(miss) = counted.miss {
                    let text = session.document(&miss.path).map(|d| d.text.clone());
                    log.event("miss", miss.event(text.as_deref()));
                }
            }
            Message::Notification(notification) => {
                if notification.method == lsp_types::notification::Exit::METHOD {
                    // Exit without shutdown: the protocol says stop, and so we
                    // do — there is no state here worth refusing to lose.
                    log.event("exit", serde_json::json!({ "shutdown": false }));
                    return Ok(());
                }
                let method = notification.method.clone();
                let published = notify(&mut session, &mut indexer, notification);
                log.event(
                    "notification",
                    serde_json::json!({
                        "op": method,
                        "diagnostics": published.is_some(),
                    }),
                );
                if let Some(mut diagnostics) = published {
                    if let (Some(spelling), Message::Notification(n)) =
                        (&spelling, &mut diagnostics)
                    {
                        spelling.apply(&mut n.params);
                    }
                    writer.send(diagnostics)?;
                }
            }
            Message::Response(_) => {}
        }
        for root in session.take_unindexed() {
            indexer.want(root, false);
        }
        for root in session.take_resume() {
            indexer.resume(root);
        }
    }
    // The pipe closed: the client went away without a shutdown request.
    Ok(())
}

/// How much of the root's state has been built ahead of being asked for.
#[derive(Clone, Copy, PartialEq, PartialOrd)]
enum Warm {
    Cold,
    Tree,
    Done,
}

impl Warm {
    fn next(self) -> Warm {
        match self {
            Warm::Cold => Warm::Tree,
            _ => Warm::Done,
        }
    }
}

/// After `shutdown`, the only thing left to do is wait for `exit`.
///
/// Anything else that arrives is refused rather than served — the spec's rule,
/// and the reason this is not lsp-server's `handle_shutdown`: that reads the
/// raw channel, and the `exit` it waits for may already be sitting in the
/// inbox, read ahead with everything else.
fn await_exit(writer: &wire::Writer, inbox: &Inbox) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return;
        }
        match inbox.next(Some(left)) {
            Next::Message(Message::Notification(n))
                if n.method == lsp_types::notification::Exit::METHOD =>
            {
                return;
            }
            Next::Message(Message::Request(request)) => {
                let _ = writer.send(
                    Response::new_err(
                        request.id,
                        lsp_server::ErrorCode::InvalidRequest as i32,
                        "the server is shutting down".into(),
                    )
                    .into(),
                );
            }
            Next::Message(_) | Next::Idle => {}
            Next::Closed => return,
        }
    }
}

fn cancelled(id: RequestId) -> Response {
    Response::new_err(
        id,
        lsp_server::ErrorCode::RequestCanceled as i32,
        "cancelled".into(),
    )
}

/// What a successor needs from the running session.
struct Current<'a> {
    params: &'a serde_json::Value,
    registered: &'a [String],
    session: &'a Session,
    inbox: &'a Inbox,
    writer: &'a wire::Writer,
}

#[derive(PartialEq)]
enum Swap {
    /// Keep serving on this build.
    Stay,
    /// Exit, so the client starts the new build afresh.
    Retire,
}

/// The binary at the launch path changed: become it, carrying the session.
///
/// A server that keeps serving after its binary is replaced is silent
/// staleness — the bug class this engine hunts everywhere else. Retiring (exit
/// and let the client restart) fixed that at a cost: the warmed tree and
/// listing, a restart counted against the client's crash budget, and — in a
/// client that does not restart servers — the server itself. Exec keeps the
/// pid and the pipes, so the client sees nothing.
///
/// Returns only when that could not happen.
fn swap(launched: &mut reload::Launched, stamp: reload::Stamp, now: Current, log: &Log) -> Swap {
    let path = launched.path().to_string_lossy().into_owned();
    let failed = |launched: &mut reload::Launched, error: String, retry: bool| {
        log.event(
            "reload_failed",
            serde_json::json!({ "path": path, "error": error, "retry": retry }),
        );
        log.count(
            "reload-failed",
            String::new(),
            Outcome::Error("reload"),
            None,
            false,
        );
        // A refusal that would recur is not retried at every quiet moment;
        // the next change to the file is tried afresh.
        if !retry {
            launched.settle(stamp);
        }
        Swap::Stay
    };
    let started = std::time::Instant::now();
    let version = match reload::probe(launched.path()) {
        reload::Candidate::Resumable { version } => version,
        reload::Candidate::Unresumable { reason } => {
            // Serving a stale build is worse than a restart.
            log.event(
                "retire",
                serde_json::json!({ "reason": reason, "path": path }),
            );
            log.count("retire", String::new(), Outcome::Hit, None, false);
            return Swap::Retire;
        }
        reload::Candidate::Broken { reason, transient } => {
            return failed(launched, reason, transient);
        }
    };
    let handoff = reload::Handoff::new(
        now.params.clone(),
        now.registered.to_vec(),
        now.session.editor_buffers(),
        now.inbox.unread(),
    );
    let file = match reload::write_handoff(&handoff) {
        Ok(file) => file,
        Err(error) => return failed(launched, error.to_string(), false),
    };
    log.event(
        "reload",
        serde_json::json!({
            "from": env!("CARGO_PKG_VERSION"),
            "to": version,
            "path": path,
            "documents": handoff.documents.len(),
            "unread": handoff.unread.len(),
            // Asking the new build, and writing the handoff: what the swap
            // costs before the exec.
            "ms": started.elapsed().as_millis() as u64,
        }),
    );
    log.count(
        "reload",
        String::new(),
        Outcome::Hit,
        Some(started.elapsed()),
        false,
    );
    // Everything answered so far goes out under this build.
    now.writer.flush();
    let error = reload::exec(launched.path(), &file);
    // Still here: the exec failed, and this build keeps the session.
    let _ = std::fs::remove_file(&file);
    let busy = error.kind() == std::io::ErrorKind::ExecutableFileBusy;
    failed(launched, error.to_string(), busy)
}

/// How many times one session reopens a replaced store.
const REOPENS: u32 = 3;

/// Reopen the store after another process replaced it: set it aside as
/// damaged, or rebuilt it for another schema (DEC-300). Opening lands on the
/// rebuilt file, or on this version's own store beside a newer trekr's, and
/// the caller refills it. The person at the editor is told once per reopen;
/// a reopen that fails is logged and answers go on from the old store.
fn reopen(session: &mut Session, why: &str, log: &Log) -> Option<Message> {
    let store = match crate::store::open_default() {
        Ok(store) => store,
        Err(error) => {
            log.event(
                "store_reopen_failed",
                serde_json::json!({ "why": why, "error": format!("{error:#}") }),
            );
            return None;
        }
    };
    let file = store
        .path()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    log.event(
        "store_reopened",
        serde_json::json!({ "why": why, "store": file }),
    );
    session.replace_store(store);
    let message = format!(
        "trekr: the index was replaced underneath this server ({why}); it now reads {file} \
         and is reindexing, so answers are partial until that finishes."
    );
    Some(
        Notification::new(
            "window/showMessage".to_string(),
            serde_json::json!({ "type": 3, "message": message }),
        )
        .into(),
    )
}

/// The workspace folder, from whichever field the client used.
fn workspace_root(params: &serde_json::Value) -> PathBuf {
    let spelled = client_root(params);
    // The store keys checkouts on git's canonical path. An editor sends the
    // path the user typed, and on macOS `/var` is a symlink to `/private/var` —
    // so without this the tree is looked up under a root that does not exist
    // and comes back empty, silently.
    std::fs::canonicalize(&spelled).unwrap_or(spelled)
}

/// The workspace root as the client spelled it.
fn client_root(params: &serde_json::Value) -> PathBuf {
    let folder = params
        .get("workspaceFolders")
        .and_then(|f| f.as_array())
        .and_then(|f| f.first())
        .and_then(|f| f.get("uri"))
        .and_then(|u| u.as_str())
        .or_else(|| params.get("rootUri").and_then(|u| u.as_str()));
    folder
        .and_then(convert::uri_to_path)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
}

/// Paths go out in the spelling the client used for its workspace.
///
/// Everything inside is canonical, because the store is. But a workspace
/// opened through a symlink — `~/code` linked elsewhere, or macOS's `/var` —
/// would then be sent locations under the *other* spelling, and an editor
/// opens those as different files: a second tab, with its own unsaved state.
struct Spelling {
    canonical: String,
    client: String,
}

impl Spelling {
    fn of(params: &serde_json::Value, root: &std::path::Path) -> Option<Spelling> {
        let client = client_root(params);
        (client != root).then(|| Spelling {
            canonical: convert::path_to_uri(root),
            client: convert::path_to_uri(&client),
        })
    }

    /// Rewrite every URI under the canonical root, anywhere in an answer —
    /// inside text too, since hover's "Defined in" is a markdown link.
    fn apply(&self, value: &mut serde_json::Value) {
        match value {
            serde_json::Value::String(text) => {
                let under = format!("{}/", self.canonical);
                if *text == self.canonical {
                    text.clone_from(&self.client);
                } else if text.contains(&under) {
                    *text = text.replace(&under, &format!("{}/", self.client));
                }
            }
            serde_json::Value::Array(items) => items.iter_mut().for_each(|v| self.apply(v)),
            serde_json::Value::Object(map) => map.values_mut().for_each(|v| self.apply(v)),
            _ => {}
        }
    }
}

/// A request, as `--usage` counts it.
struct Counted {
    feature: String,
    flags: String,
    outcome: Outcome,
    latency: Option<Duration>,
    /// A definition or hover that came back empty or unsure, logged once the
    /// answer is sent (DEC-083).
    miss: Option<miss::Miss>,
}

/// The operation, without the protocol's namespace: `definition`, `hover`,
/// `completionItem/resolve`. Client-chosen text for a method trekr does not
/// serve, so it is bounded like any other label.
fn feature_of(method: &str) -> String {
    let short = method.strip_prefix("textDocument/").unwrap_or(method);
    short
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '$' | '_' | '-'))
        .take(40)
        .collect()
}

fn dispatch(
    session: &mut Session,
    request: Request,
    out: &Outbound,
    cancel: &dyn Fn() -> bool,
) -> (Response, Counted) {
    let log = out.log;
    let id = request.id.clone();
    let method = request.method.clone();
    let asked = asked_about(&request.params);
    let clicked =
        miss::op_of(&method).and_then(|op| clicked_at(&request.params).map(|at| (op, at)));
    log.detail("request_params", || request.params.clone());

    let started = std::time::Instant::now();
    // Whatever an earlier operation noted and nobody took is not this one's.
    let _ = crate::usage::take();
    let _ = miss::take_why();
    let result = route(session, request, out, cancel);
    let elapsed = started.elapsed();
    let note = crate::usage::take();

    let (status, answered, code) = match &result {
        Ok(value) => ("ok", shape(value), None),
        Err(error) => {
            let code = error_code(error);
            let status = if matches!(code, lsp_server::ErrorCode::RequestCanceled) {
                "cancelled"
            } else {
                "error"
            };
            (status, None, Some(code))
        }
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

    let outcome = match (answered, code) {
        (Some(0), _) => Outcome::Empty,
        (Some(_), _) => note.outcome.unwrap_or(Outcome::Hit),
        (None, Some(lsp_server::ErrorCode::RequestCanceled)) => Outcome::Cancelled,
        (None, Some(lsp_server::ErrorCode::InvalidParams)) => Outcome::Error("params"),
        (None, Some(lsp_server::ErrorCode::MethodNotFound)) => Outcome::Error("unsupported"),
        (None, _) => Outcome::Error("internal"),
    };
    let why = miss::take_why();
    let missed = matches!(outcome, Outcome::Empty | Outcome::Uncertain);
    let miss = clicked
        .filter(|_| missed)
        .map(|(op, (path, position))| miss::Miss {
            op,
            path,
            position,
            outcome: outcome.label(),
            why,
        });
    let counted = Counted {
        feature: feature_of(&method),
        flags: crate::usage::join(&note.flags),
        outcome,
        latency: Some(elapsed),
        miss,
    };

    let response = match (result, code) {
        (Ok(value), _) => Response::new_ok(id, value),
        (Err(error), code) => Response::new_err(
            id,
            code.unwrap_or(lsp_server::ErrorCode::InternalError) as i32,
            error.to_string(),
        ),
    };
    (response, counted)
}

/// Why a request failed, in the protocol's vocabulary. A client treats these
/// differently — `MethodNotFound` means "do not ask again", `RequestCanceled`
/// means "you asked me to stop" — so collapsing them all into InternalError
/// told it the server was broken when it was not.
fn error_code(error: &anyhow::Error) -> lsp_server::ErrorCode {
    use lsp_server::ErrorCode;
    if error.is::<Cancelled>() {
        ErrorCode::RequestCanceled
    } else if error.is::<BadParams>() {
        ErrorCode::InvalidParams
    } else if error.is::<Unsupported>() {
        ErrorCode::MethodNotFound
    } else {
        ErrorCode::InternalError
    }
}

/// What a handler may say before its answer: partial results, a message for
/// the user, a log line. Paths in it go out in the client's spelling, as the
/// answer's do.
pub(crate) struct Outbound<'a> {
    writer: &'a wire::Writer,
    spelling: Option<&'a Spelling>,
    pub(crate) log: &'a Log,
}

impl Outbound<'_> {
    pub(crate) fn notify(&self, method: &str, mut params: serde_json::Value) -> anyhow::Result<()> {
        if let Some(spelling) = self.spelling {
            spelling.apply(&mut params);
        }
        self.writer
            .send(Notification::new(method.to_string(), params).into())
    }
}

/// The client withdrew the request while it was being worked on.
#[derive(Debug)]
pub(crate) struct Cancelled;

/// The params did not have the shape the method requires.
#[derive(Debug)]
struct BadParams(String);

/// A method this server does not implement.
#[derive(Debug)]
struct Unsupported(String);

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}
impl std::fmt::Display for BadParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid params: {}", self.0)
    }
}
impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "trekr does not implement {}", self.0)
    }
}
impl std::error::Error for Cancelled {}
impl std::error::Error for BadParams {}
impl std::error::Error for Unsupported {}

fn route(
    session: &mut Session,
    request: Request,
    out: &Outbound,
    cancel: &dyn Fn() -> bool,
) -> anyhow::Result<serde_json::Value> {
    use lsp_types::request as req;
    // Neither has a field to say an answer is partial (DEC-331).
    if matches!(
        request.method.as_str(),
        req::GotoDefinition::METHOD | req::References::METHOD | req::GotoImplementation::METHOD
    ) && let (Some(uri), _) = asked_about(&request.params)
        && let Some(path) = document_path(&uri)
    {
        handlers::tell_warming(session, &path, out)?;
    }
    match request.method.as_str() {
        req::GotoDefinition::METHOD => run_handler(request, |p| handlers::definition(session, p)),
        req::References::METHOD => {
            run_handler(request, |p| handlers::references(session, p, out, cancel))
        }
        req::DocumentSymbolRequest::METHOD => {
            run_handler(request, |p| handlers::document_symbol(session, p))
        }
        req::WorkspaceSymbolRequest::METHOD => {
            run_handler(request, |p| handlers::workspace_symbol(session, p))
        }
        req::HoverRequest::METHOD => run_handler(request, |p| handlers::hover(session, p)),
        req::DocumentHighlightRequest::METHOD => {
            run_handler(request, |p| handlers::document_highlight(session, p))
        }
        req::DocumentLinkRequest::METHOD => {
            run_handler(request, |p| handlers::document_link(session, p))
        }
        req::GotoImplementation::METHOD => {
            run_handler(request, |p| handlers::implementation(session, p))
        }
        req::CallHierarchyPrepare::METHOD => {
            run_handler(request, |p| handlers::prepare_call_hierarchy(session, p))
        }
        req::CallHierarchyIncomingCalls::METHOD => {
            run_handler(request, |p| handlers::incoming_calls(session, p, cancel))
        }
        req::CallHierarchyOutgoingCalls::METHOD => {
            run_handler(request, |p| handlers::outgoing_calls(session, p))
        }
        req::Completion::METHOD => run_handler(request, |p| complete::completion(session, p)),
        req::ResolveCompletionItem::METHOD => {
            run_handler(request, |p| complete::resolve(session, p))
        }
        other => Err(Unsupported(other.to_string()).into()),
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

/// The file and position a click is about, in the session's spelling of the
/// path.
fn clicked_at(params: &serde_json::Value) -> Option<(PathBuf, lsp_types::Position)> {
    let uri = params.get("textDocument")?.get("uri")?.as_str()?;
    let path = convert::uri_to_path(uri)?;
    let position = serde_json::from_value(params.get("position")?.clone()).ok()?;
    Some((std::fs::canonicalize(&path).unwrap_or(path), position))
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
    let params: P = serde_json::from_value(request.params).map_err(|e| BadParams(e.to_string()))?;
    Ok(serde_json::to_value(handler(params)?)?)
}

/// Document lifecycle. Returns syntax diagnostics to publish, when there are
/// any to say something about.
///
/// Documents are keyed by their canonical absolute path, not by a
/// workspace-relative one: a session answers for several checkouts at once, and
/// two of them can each have an `app.rb`.
fn notify(
    session: &mut Session,
    indexer: &mut fresh::Indexer,
    notification: Notification,
) -> Option<Message> {
    use lsp_types::notification as note;
    match notification.method.as_str() {
        note::DidSaveTextDocument::METHOD => {
            let params: lsp_types::DidSaveTextDocumentParams =
                serde_json::from_value(notification.params).ok()?;
            let path = document_path(params.text_document.uri.as_str())?;
            indexer.refresh(session, &path);
            None
        }
        note::DidChangeWatchedFiles::METHOD => {
            let params: lsp_types::DidChangeWatchedFilesParams =
                serde_json::from_value(notification.params).ok()?;
            watched(session, indexer, params.changes);
            None
        }
        note::DidOpenTextDocument::METHOD => {
            let params: lsp_types::DidOpenTextDocumentParams =
                serde_json::from_value(notification.params).ok()?;
            let path = document_path(params.text_document.uri.as_str())?;
            session.did_open(
                path.clone(),
                params.text_document.text,
                params.text_document.version,
            );
            if !indexer.opened(&path) {
                session.hand_to_another_index(&path);
            }
            handlers::diagnostics(session, &path, params.text_document.uri)
        }
        note::DidChangeTextDocument::METHOD => {
            let params: lsp_types::DidChangeTextDocumentParams =
                serde_json::from_value(notification.params).ok()?;
            let path = document_path(params.text_document.uri.as_str())?;
            // FULL sync is what we asked for, so each change is normally the
            // whole document. A client that sends ranged edits anyway gets
            // them applied rather than mistaken for the whole file.
            let mut text = session
                .editor_text(&path)
                .map(str::to_string)
                .unwrap_or_default();
            for change in params.content_changes {
                match change.range {
                    Some(range) => convert::apply_edit(&mut text, range, &change.text),
                    None => text = change.text,
                }
            }
            session.did_open(path.clone(), text, params.text_document.version);
            handlers::diagnostics(session, &path, params.text_document.uri)
        }
        note::DidCloseTextDocument::METHOD => {
            let params: lsp_types::DidCloseTextDocumentParams =
                serde_json::from_value(notification.params).ok()?;
            session.did_close(&document_path(params.text_document.uri.as_str())?);
            // A closed file's syntax errors would otherwise sit in the
            // Problems panel until the file is opened again.
            Some(handlers::publish(
                params.text_document.uri,
                Vec::new(),
                None,
            ))
        }
        _ => None,
    }
}

/// Files changed underneath the editor. A few are refreshed in place; many at
/// once, or any deletion, is an operation on the checkout and gets a full
/// index — `refresh_file` can add and replace a file but not remove one.
fn watched(
    session: &mut Session,
    indexer: &mut fresh::Indexer,
    changes: Vec<lsp_types::FileEvent>,
) {
    let paths: Vec<(PathBuf, lsp_types::FileChangeType)> = changes
        .into_iter()
        .filter_map(|change| Some((document_path(change.uri.as_str())?, change.typ)))
        .filter(|(path, _)| crate::scan::is_indexed(&path.to_string_lossy()))
        .collect();
    let bulk = paths.len() > fresh::BULK
        || paths
            .iter()
            .any(|(_, kind)| *kind == lsp_types::FileChangeType::DELETED);
    if bulk {
        let mut roots: Vec<PathBuf> = paths
            .iter()
            .filter_map(|(path, _)| {
                // A deleted file cannot be canonicalized or placed by git;
                // its directory usually still can.
                let probe = if path.exists() {
                    path.clone()
                } else {
                    path.parent()?.join(".")
                };
                session.locate(&probe).map(|located| located.root)
            })
            .collect();
        roots.sort();
        roots.dedup();
        for root in roots {
            if session.indexed(&root) {
                indexer.want(root, true);
            }
        }
        return;
    }
    for (path, _) in paths {
        indexer.refresh(session, &path);
    }
}

/// What the client said it can do, and what it asked of us.
struct Client {
    /// `window.workDoneProgress`: it will show `$/progress`.
    progress: bool,
    /// `workspace.didChangeWatchedFiles.dynamicRegistration`: it will watch
    /// files for us if asked.
    watch: bool,
    /// `initializationOptions.index`: whether to index checkouts in the
    /// background. On unless turned off.
    index: bool,
    /// `textDocument.definition.linkSupport`: it takes `LocationLink`s.
    definition_links: bool,
    /// `initializationOptions.referenceLimit`: how many references an answer
    /// keeps. A positive integer; anything else keeps the default.
    reference_limit: usize,
    /// `initializationOptions.unresolved`: `peek`, `best`, `confident` or
    /// `none`; anything else keeps the default.
    unresolved: state::Unresolved,
    /// The `unresolved` given, when it was none of those: logged once.
    unresolved_invalid: Option<serde_json::Value>,
}

impl Client {
    fn from(params: &serde_json::Value) -> Client {
        let flag = |pointer: &str| params.pointer(pointer).and_then(serde_json::Value::as_bool);
        let unresolved = params.pointer("/initializationOptions/unresolved");
        Client {
            progress: flag("/capabilities/window/workDoneProgress").unwrap_or(false),
            watch: flag("/capabilities/workspace/didChangeWatchedFiles/dynamicRegistration")
                .unwrap_or(false),
            index: flag("/initializationOptions/index").unwrap_or(true),
            definition_links: flag("/capabilities/textDocument/definition/linkSupport")
                .unwrap_or(false),
            reference_limit: params
                .pointer("/initializationOptions/referenceLimit")
                .and_then(serde_json::Value::as_u64)
                .filter(|&n| n > 0)
                .map_or(gather::DEFAULT_LIMIT, |n| n as usize),
            unresolved: unresolved
                .and_then(state::Unresolved::parse)
                .unwrap_or_default(),
            unresolved_invalid: unresolved
                .filter(|v| state::Unresolved::parse(v).is_none())
                .cloned(),
        }
    }
}

/// The id of the file-watch registration — carried across a reload, because
/// the client still holds it.
const WATCH: &str = "trekr-watch";

/// Ask the client to report changes to Ruby files. A branch switch arrives as
/// a burst of these, which is what tips a batch into a full index.
fn watch_request() -> Message {
    Message::Request(Request::new(
        RequestId::from(WATCH.to_string()),
        "client/registerCapability".into(),
        serde_json::json!({
            "registrations": [{
                "id": WATCH,
                "method": "workspace/didChangeWatchedFiles",
                "registerOptions": {
                    "watchers": [
                        { "globPattern": "**/*.{rb,rake,ru,gemspec,rbi,jbuilder}" },
                        // A migration rewrites the app's SQL dump (DEC-480).
                        { "globPattern": "**/db/*structure.sql" },
                    ],
                },
            }],
        }),
    ))
}

/// A document URI as the one path this session will key it by. Canonical, so
/// `/var` and `/private/var` are the same document.
fn document_path(uri: &str) -> Option<PathBuf> {
    let path = convert::uri_to_path(uri)?;
    Some(std::fs::canonicalize(&path).unwrap_or(path))
}
