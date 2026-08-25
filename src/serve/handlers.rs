//! The nine operations, answered from the cached tree.
//!
//! Each is the CLI's answer in LSP's clothing — the same ladder, the same
//! tiers, the same disclosure. Where LSP has no field for what this engine
//! knows, the answer carries it anyway: `hover` says which rung resolved a
//! receiver and how confident that makes it, and `references` orders confirmed
//! before possible so the list itself is the disclosure.

use super::convert::{self, path_to_uri, point, to_pos};
use super::state::Workspace;
use crate::cli::position::{self, Under};
use crate::resolve::refs;
use lsp_types::Uri as Url;
use lsp_types::{
    CallHierarchyIncomingCall, CallHierarchyIncomingCallsParams, CallHierarchyItem,
    CallHierarchyOutgoingCall, CallHierarchyOutgoingCallsParams, CallHierarchyPrepareParams,
    Diagnostic, DiagnosticSeverity, DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse,
    GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverContents, HoverParams, Location,
    MarkupContent, MarkupKind, ReferenceParams, SymbolKind, WorkspaceSymbolParams,
};

/// How many ranked guesses `goToDefinition` offers when the receiver did not
/// resolve.
///
/// Five, because an editor shows a picker and a human scans it — Ruby LSP's
/// fallback is the first ten methods with the name, which is where "ranked"
/// stops meaning anything. `hover` at the same position says these are guesses.
const MAX_GUESSES: usize = 5;

/// A location in the workspace, from our `path:line:col`.
fn location(workspace: &Workspace, path: &str, line: u32, col: u32) -> Option<Location> {
    // A site in the core stub or a gem has no file in this workspace.
    if path.starts_with('<') {
        return None;
    }
    let absolute = workspace.root.join(path);
    let uri: Url = path_to_uri(&absolute).parse().ok()?;
    Some(Location {
        uri,
        range: point(None, line, col),
    })
}

/// The path and position a text-document request is about.
fn target(
    workspace: &mut Workspace,
    uri: &Url,
    position: lsp_types::Position,
) -> Option<(String, crate::core::Pos)> {
    let path = convert::uri_to_path(uri.as_str())?;
    let relative = workspace.relative(&path)?;
    let text = workspace.document(&relative)?.text.clone();
    Some((relative, to_pos(&text, position)))
}

pub(crate) fn definition(
    workspace: &mut Workspace,
    params: GotoDefinitionParams,
) -> anyhow::Result<Option<GotoDefinitionResponse>> {
    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;
    let Some((path, pos)) = target(workspace, &uri, position) else {
        return Ok(None);
    };
    let sites = resolve_at(workspace, &path, pos)?;
    let locations: Vec<Location> = sites
        .into_iter()
        .filter_map(|(p, line, col)| location(workspace, &p, line, col))
        .collect();
    Ok((!locations.is_empty()).then_some(GotoDefinitionResponse::Array(locations)))
}

/// Where the name at a position is defined — the CLI's `--def`, as sites.
fn resolve_at(
    workspace: &mut Workspace,
    path: &str,
    pos: crate::core::Pos,
) -> anyhow::Result<Vec<(String, u32, u32)>> {
    let facts = workspace
        .document(path)
        .map(|document| document.facts().clone());
    let Some(facts) = facts else {
        return Ok(Vec::new());
    };
    let Some(under) = position::at_facts(&facts, pos.line, pos.col) else {
        return Ok(Vec::new());
    };
    let path = path.to_string();
    let tree = workspace.tree()?;
    Ok(match under {
        Under::Definition(def) => vec![(path, def.pos.line, def.pos.col)],
        Under::Constant(reference) => tree
            .resolve(&reference.name, &reference.nesting)
            .sites
            .into_iter()
            .map(|site| (site.path, site.line, site.col))
            .collect(),
        Under::Call(call) => {
            let answer = crate::resolve::method_at(tree, &facts, &call, &path);
            if !answer.sites.is_empty() {
                answer
                    .sites
                    .into_iter()
                    .map(|site| (site.path, site.line, site.col))
                    .collect()
            } else {
                // Residue is not "nothing known". The CLI has always returned
                // ranked candidates here; returning null instead was the LSP
                // surface throwing away an answer the engine already had.
                // Order is the disclosure, as it is for references, and `hover`
                // at the same position says the receiver was never resolved.
                answer
                    .candidates
                    .into_iter()
                    .take(MAX_GUESSES)
                    .map(|candidate| (candidate.site.path, candidate.site.line, candidate.site.col))
                    .collect()
            }
        }
    })
}

pub(crate) fn references(
    workspace: &mut Workspace,
    params: ReferenceParams,
) -> anyhow::Result<Option<Vec<Location>>> {
    let uri = params.text_document_position.text_document.uri;
    let position = params.text_document_position.position;
    let Some((path, pos)) = target(workspace, &uri, position) else {
        return Ok(None);
    };
    let facts = workspace
        .document(&path)
        .map(|document| document.facts().clone());
    let Some(facts) = facts else { return Ok(None) };
    let Some(under) = position::at_facts(&facts, pos.line, pos.col) else {
        return Ok(None);
    };

    let root = workspace.root.clone();
    let root_str = root.to_string_lossy().into_owned();
    let name = match &under {
        Under::Definition(def) => def.name.clone(),
        Under::Call(call) => call.name.clone(),
        Under::Constant(reference) => reference.name.clone(),
    };
    let paths = workspace.store().files_calling(&root_str, &name)?;
    let tree = workspace.tree()?;

    // Which method is being asked about, not just which name. Standing on a
    // definition, the owner is the scope that declares it; standing on a call,
    // it is wherever that call resolves. Without this the answer merges every
    // same-named method in the repo — which is the grep this exists to beat.
    let query = match &under {
        Under::Definition(def) => refs::Query {
            owner: tree.scope_fqn(&def.nesting),
            singleton: def.singleton,
            name,
        },
        Under::Call(call) => {
            let answer = crate::resolve::method_at(tree, &facts, call, &path);
            refs::Query {
                owner: answer.owner,
                singleton: call.singleton,
                name,
            }
        }
        Under::Constant(_) => refs::Query {
            owner: None,
            singleton: false,
            name,
        },
    };
    let target = query.owner.clone();

    let mut found: Vec<refs::Reference> = Vec::new();
    for candidate in paths {
        let Ok(bytes) = std::fs::read(root.join(&candidate)) else {
            continue;
        };
        let file_facts = crate::extract::extract(&bytes);
        for call in file_facts.calls.iter().filter(|c| c.name == query.name) {
            let reference = refs::tier_call(
                tree,
                &file_facts,
                call,
                &candidate,
                &query,
                target.as_deref(),
            );
            if reference.tier != refs::Tier::Excluded {
                found.push(reference);
            }
        }
    }
    // Confirmed before possible: LSP has no tier field, so the order of the
    // list is the disclosure.
    found.sort_by_key(refs::order);

    let locations: Vec<Location> = found
        .into_iter()
        .filter_map(|r| location(workspace, &r.path, r.line, r.col))
        .collect();
    Ok(Some(locations))
}

pub(crate) fn document_symbol(
    workspace: &mut Workspace,
    params: DocumentSymbolParams,
) -> anyhow::Result<Option<DocumentSymbolResponse>> {
    let Some(path) = convert::uri_to_path(params.text_document.uri.as_str()) else {
        return Ok(None);
    };
    let Some(relative) = workspace.relative(&path) else {
        return Ok(None);
    };
    let facts = workspace
        .document(&relative)
        .map(|document| document.facts().clone());
    let Some(facts) = facts else { return Ok(None) };

    #[allow(deprecated)]
    let symbols: Vec<DocumentSymbol> = facts
        .defs
        .iter()
        .map(|def| DocumentSymbol {
            name: def.name.clone(),
            detail: def.via.clone(),
            kind: symbol_kind(def.kind),
            tags: None,
            deprecated: None,
            range: point(None, def.pos.line, def.pos.col),
            selection_range: point(None, def.pos.line, def.pos.col),
            children: None,
        })
        .collect();
    Ok(Some(DocumentSymbolResponse::Nested(symbols)))
}

fn symbol_kind(kind: crate::core::Kind) -> SymbolKind {
    use crate::core::Kind;
    match kind {
        Kind::Class => SymbolKind::CLASS,
        Kind::Module => SymbolKind::MODULE,
        Kind::Method => SymbolKind::METHOD,
        Kind::Constant => SymbolKind::CONSTANT,
    }
}

pub(crate) fn workspace_symbol(
    workspace: &mut Workspace,
    params: WorkspaceSymbolParams,
) -> anyhow::Result<Option<Vec<lsp_types::SymbolInformation>>> {
    let root = workspace.root.to_string_lossy().into_owned();
    let rows = workspace.store().symbols_named(&root, &params.query, 200)?;
    #[allow(deprecated)]
    let symbols = rows
        .into_iter()
        .filter_map(|row| {
            Some(lsp_types::SymbolInformation {
                name: row.name,
                kind: match row.kind.as_str() {
                    "class" => SymbolKind::CLASS,
                    "module" => SymbolKind::MODULE,
                    "constant" => SymbolKind::CONSTANT,
                    _ => SymbolKind::METHOD,
                },
                tags: None,
                deprecated: None,
                location: location(workspace, &row.path, row.line, row.col)?,
                container_name: row.nesting.first().cloned(),
            })
        })
        .collect();
    Ok(Some(symbols))
}

/// Hover is where the disclosure lives: LSP has no confidence field, so the
/// answer says it in words.
pub(crate) fn hover(
    workspace: &mut Workspace,
    params: HoverParams,
) -> anyhow::Result<Option<Hover>> {
    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;
    let Some((path, pos)) = target(workspace, &uri, position) else {
        return Ok(None);
    };
    let facts = workspace
        .document(&path)
        .map(|document| document.facts().clone());
    let Some(facts) = facts else { return Ok(None) };
    let Some(under) = position::at_facts(&facts, pos.line, pos.col) else {
        return Ok(None);
    };
    let tree = workspace.tree()?;

    let text = match under {
        Under::Definition(def) => {
            let params = def
                .params
                .iter()
                .map(|p| format!("{}: {}", p.name, p.kind.as_str()))
                .collect::<Vec<_>>()
                .join(", ");
            let mut out = format!("**{}**\n\n`{}`", def.name, params);
            if let Some(returns) = &def.sig_returns {
                out.push_str(&format!("\n\nreturns `{returns}`"));
            }
            out
        }
        Under::Constant(reference) => {
            let resolution = tree.resolve(&reference.name, &reference.nesting);
            format!(
                "**{}**\n\nstatus: `{:?}` · confidence: {:.1}{}",
                reference.name,
                resolution.status,
                resolution.confidence,
                resolution
                    .resolved_via
                    .map(|via| format!(" · via `{via:?}`"))
                    .unwrap_or_default()
            )
        }
        Under::Call(call) => {
            let answer = crate::resolve::method_at(tree, &facts, &call, &path);
            let mut out = format!(
                "**{}**\n\nreceiver: `{}`{}\n\nstatus: `{:?}` · confidence: {:.2}",
                call.name,
                answer.receiver,
                answer
                    .receiver_type
                    .map(|t| format!(" → `{t}`"))
                    .unwrap_or_default(),
                answer.status,
                answer.confidence,
            );
            if let Some(via) = answer.resolved_via {
                out.push_str(&format!(" · via `{via}`"));
            }
            if let Some(owner) = answer.owner {
                out.push_str(&format!("\n\ndefined in `{owner}`"));
            }
            if let Some(reason) = answer.reason {
                out.push_str(&format!("\n\n{reason}"));
            }
            out
        }
    };
    Ok(Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: text,
        }),
        range: None,
    }))
}

/// Descendants of the class or module at the cursor.
pub(crate) fn implementation(
    workspace: &mut Workspace,
    params: lsp_types::request::GotoImplementationParams,
) -> anyhow::Result<Option<GotoDefinitionResponse>> {
    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;
    let Some((path, pos)) = target(workspace, &uri, position) else {
        return Ok(None);
    };
    let facts = workspace
        .document(&path)
        .map(|document| document.facts().clone());
    let Some(facts) = facts else { return Ok(None) };
    let Some(under) = position::at_facts(&facts, pos.line, pos.col) else {
        return Ok(None);
    };
    let name = match under {
        Under::Definition(def) => def.name,
        Under::Constant(reference) => reference.name,
        Under::Call(_) => return Ok(None),
    };
    let tree = workspace.tree()?;
    let Some(fqn) = tree.resolve(&name, &[]).fqn else {
        return Ok(None);
    };
    let sites: Vec<(String, u32, u32)> = tree
        .includers_of(&fqn)
        .iter()
        .flat_map(|descendant| tree.sites(descendant).to_vec())
        .map(|site| (site.path, site.line, site.col))
        .collect();
    let locations: Vec<Location> = sites
        .into_iter()
        .filter_map(|(p, line, col)| location(workspace, &p, line, col))
        .collect();
    Ok((!locations.is_empty()).then_some(GotoDefinitionResponse::Array(locations)))
}

pub(crate) fn prepare_call_hierarchy(
    workspace: &mut Workspace,
    params: CallHierarchyPrepareParams,
) -> anyhow::Result<Option<Vec<CallHierarchyItem>>> {
    let uri = params
        .text_document_position_params
        .text_document
        .uri
        .clone();
    let position = params.text_document_position_params.position;
    let Some((path, pos)) = target(workspace, &uri, position) else {
        return Ok(None);
    };
    let facts = workspace
        .document(&path)
        .map(|document| document.facts().clone());
    let Some(facts) = facts else { return Ok(None) };
    let Some(under) = position::at_facts(&facts, pos.line, pos.col) else {
        return Ok(None);
    };
    let (name, line, col) = match under {
        Under::Definition(def) => (def.name, def.pos.line, def.pos.col),
        Under::Call(call) => (call.name, call.pos.line, call.pos.col),
        Under::Constant(_) => return Ok(None),
    };
    #[allow(deprecated)]
    Ok(Some(vec![CallHierarchyItem {
        name,
        kind: SymbolKind::METHOD,
        tags: None,
        detail: None,
        uri,
        range: point(None, line, col),
        selection_range: point(None, line, col),
        data: None,
    }]))
}

/// Incoming calls are the confirmed tier of a references query — the whole
/// point of having tiers.
pub(crate) fn incoming_calls(
    workspace: &mut Workspace,
    params: CallHierarchyIncomingCallsParams,
) -> anyhow::Result<Option<Vec<CallHierarchyIncomingCall>>> {
    let name = params.item.name.clone();
    let root = workspace.root.clone();
    let root_str = root.to_string_lossy().into_owned();
    let query = refs::Query {
        owner: None,
        singleton: false,
        name: name.clone(),
    };
    let paths = workspace.store().files_calling(&root_str, &name)?;
    let tree = workspace.tree()?;

    let mut confirmed: Vec<refs::Reference> = Vec::new();
    for candidate in paths {
        let Ok(bytes) = std::fs::read(root.join(&candidate)) else {
            continue;
        };
        let file_facts = crate::extract::extract(&bytes);
        for call in file_facts.calls.iter().filter(|c| c.name == name) {
            let reference = refs::tier_call(tree, &file_facts, call, &candidate, &query, None);
            if reference.tier == refs::Tier::Confirmed {
                confirmed.push(reference);
            }
        }
    }

    #[allow(deprecated)]
    let calls = confirmed
        .into_iter()
        .filter_map(|reference| {
            let at = location(workspace, &reference.path, reference.line, reference.col)?;
            Some(CallHierarchyIncomingCall {
                from: CallHierarchyItem {
                    name: reference.owner.clone().unwrap_or_else(|| name.clone()),
                    kind: SymbolKind::METHOD,
                    tags: None,
                    detail: Some(reference.why.to_string()),
                    uri: at.uri.clone(),
                    range: at.range,
                    selection_range: at.range,
                    data: None,
                },
                from_ranges: vec![at.range],
            })
        })
        .collect();
    Ok(Some(calls))
}

/// Outgoing calls are the call-site facts inside the method's own body.
pub(crate) fn outgoing_calls(
    workspace: &mut Workspace,
    params: CallHierarchyOutgoingCallsParams,
) -> anyhow::Result<Option<Vec<CallHierarchyOutgoingCall>>> {
    let uri = params.item.uri.clone();
    let Some(path) = convert::uri_to_path(uri.as_str()) else {
        return Ok(None);
    };
    let Some(relative) = workspace.relative(&path) else {
        return Ok(None);
    };
    let facts = workspace
        .document(&relative)
        .map(|document| document.facts().clone());
    let Some(facts) = facts else { return Ok(None) };

    // The method whose body we are listing: the innermost def containing the
    // item's line.
    let line = params.item.range.start.line + 1;
    let Some(enclosing) = facts
        .defs
        .iter()
        .filter(|def| def.pos.line <= line && line <= def.end_line)
        .min_by_key(|def| def.end_line - def.pos.line)
    else {
        return Ok(None);
    };
    let (start, end) = (enclosing.pos.line, enclosing.end_line);

    #[allow(deprecated)]
    let calls = facts
        .calls
        .iter()
        .filter(|call| call.pos.line > start && call.pos.line <= end)
        .filter_map(|call| {
            let at = location(workspace, &relative, call.pos.line, call.pos.col)?;
            Some(CallHierarchyOutgoingCall {
                to: CallHierarchyItem {
                    name: call.name.clone(),
                    kind: SymbolKind::METHOD,
                    tags: None,
                    detail: Some(format!("receiver: {}", call.recv.as_str())),
                    uri: at.uri.clone(),
                    range: at.range,
                    selection_range: at.range,
                    data: None,
                },
                from_ranges: vec![at.range],
            })
        })
        .collect();
    Ok(Some(calls))
}

/// Syntax diagnostics, free from a parse we already did.
///
/// Syntax only. Everything else this engine knows is a *ranked* answer with a
/// confidence, and publishing those as diagnostics would turn disclosure into
/// noise in the editor's gutter.
pub(crate) fn diagnostics(
    workspace: &mut Workspace,
    path: &str,
    uri: Url,
) -> Option<lsp_server::Message> {
    let errors = workspace.document(path)?.parse_errors();
    let diagnostics: Vec<Diagnostic> = errors
        .into_iter()
        .map(|(line, col, message)| Diagnostic {
            range: point(None, line, col),
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("trekr".into()),
            message,
            ..Default::default()
        })
        .collect();
    let params = lsp_types::PublishDiagnosticsParams {
        uri,
        diagnostics,
        version: None,
    };
    Some(lsp_server::Message::Notification(
        lsp_server::Notification {
            method: lsp_types::notification::PublishDiagnostics::METHOD.to_string(),
            params: serde_json::to_value(params).ok()?,
        },
    ))
}

use lsp_types::notification::Notification as _;
