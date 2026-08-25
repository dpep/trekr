//! `trekr --serve` — the nine operations an agent actually uses, over stdio.
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
mod state;

use lsp_server::{Connection, ExtractError, Message, Notification, Request, RequestId, Response};
use lsp_types::notification::Notification as _;
use lsp_types::request::Request as _;
use lsp_types::{
    HoverProviderCapability, OneOf, ServerCapabilities, TextDocumentSyncCapability,
    TextDocumentSyncKind,
};
use state::Workspace;
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

pub(crate) fn run() -> anyhow::Result<()> {
    let (connection, threads) = Connection::stdio();
    // The connection owns the sender half of the writer thread's channel, so
    // it has to be *dropped* before joining — otherwise the writer never sees
    // the channel close and the join blocks forever. Taking it by value here
    // is what makes that happen.
    serve(connection)?;
    threads.join()?;
    Ok(())
}

fn serve(connection: Connection) -> anyhow::Result<()> {
    let params = connection.initialize(serde_json::to_value(capabilities())?)?;
    let root = workspace_root(&params);

    let store = crate::store::open_default()?;
    let mut workspace = Workspace::open(root, store);

    for message in &connection.receiver {
        match message {
            Message::Request(request) => {
                if connection.handle_shutdown(&request)? {
                    break;
                }
                let response = dispatch(&mut workspace, request);
                connection.sender.send(Message::Response(response))?;
            }
            Message::Notification(notification) => {
                if let Some(diagnostics) = notify(&mut workspace, notification) {
                    connection.sender.send(diagnostics)?;
                }
            }
            Message::Response(_) => {}
        }
    }
    Ok(())
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

fn dispatch(workspace: &mut Workspace, request: Request) -> Response {
    use lsp_types::request as req;
    let id = request.id.clone();
    let result = match request.method.as_str() {
        req::GotoDefinition::METHOD => run_handler(request, |p| handlers::definition(workspace, p)),
        req::References::METHOD => run_handler(request, |p| handlers::references(workspace, p)),
        req::DocumentSymbolRequest::METHOD => {
            run_handler(request, |p| handlers::document_symbol(workspace, p))
        }
        req::WorkspaceSymbolRequest::METHOD => {
            run_handler(request, |p| handlers::workspace_symbol(workspace, p))
        }
        req::HoverRequest::METHOD => run_handler(request, |p| handlers::hover(workspace, p)),
        req::GotoImplementation::METHOD => {
            run_handler(request, |p| handlers::implementation(workspace, p))
        }
        req::CallHierarchyPrepare::METHOD => {
            run_handler(request, |p| handlers::prepare_call_hierarchy(workspace, p))
        }
        req::CallHierarchyIncomingCalls::METHOD => {
            run_handler(request, |p| handlers::incoming_calls(workspace, p))
        }
        req::CallHierarchyOutgoingCalls::METHOD => {
            run_handler(request, |p| handlers::outgoing_calls(workspace, p))
        }
        // Anything else: null rather than an error, so a client probing for a
        // capability it did not read gets a civil answer.
        _ => Ok(serde_json::Value::Null),
    };
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
fn notify(workspace: &mut Workspace, notification: Notification) -> Option<Message> {
    use lsp_types::notification as note;
    match notification.method.as_str() {
        note::DidOpenTextDocument::METHOD => {
            let params: lsp_types::DidOpenTextDocumentParams =
                serde_json::from_value(notification.params).ok()?;
            let path = convert::uri_to_path(params.text_document.uri.as_str())?;
            let relative = workspace.relative(&path)?;
            workspace.did_open(relative.clone(), params.text_document.text);
            handlers::diagnostics(workspace, &relative, params.text_document.uri)
        }
        note::DidChangeTextDocument::METHOD => {
            let params: lsp_types::DidChangeTextDocumentParams =
                serde_json::from_value(notification.params).ok()?;
            let path = convert::uri_to_path(params.text_document.uri.as_str())?;
            let relative = workspace.relative(&path)?;
            // FULL sync, so the last change carries the whole document.
            let text = params.content_changes.into_iter().next_back()?.text;
            workspace.did_change(relative.clone(), text);
            handlers::diagnostics(workspace, &relative, params.text_document.uri)
        }
        note::DidCloseTextDocument::METHOD => {
            let params: lsp_types::DidCloseTextDocumentParams =
                serde_json::from_value(notification.params).ok()?;
            let path = convert::uri_to_path(params.text_document.uri.as_str())?;
            workspace.did_close(&workspace.relative(&path)?);
            None
        }
        _ => None,
    }
}

/// Kept so the unused-import lint does not fire on the error types the
/// dispatcher's shape implies.
#[allow(dead_code)]
fn _unused(_: ExtractError<Request>, _: RequestId) {}
