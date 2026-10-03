//! The nine operations, answered from the cached tree.
//!
//! Each is the CLI's answer in LSP's clothing — the same ladder, the same
//! tiers, the same disclosure. Where LSP has no field for what this engine
//! knows, the answer carries it anyway: `hover` says in words when an answer
//! is a guess, and `references` orders confirmed before possible so the list
//! itself is the disclosure.
//!
//! Two kinds of question, and they need different things. Outlining a file or
//! reporting its syntax errors needs only the file's own bytes, so it is
//! answered for **any** readable path. Resolving a name needs the checkout the
//! file belongs to, which the session finds per file rather than assuming the
//! client's root (DEC-024).

use super::convert::{self, LineIndex, path_to_uri, point, to_pos};
use super::doc::{self, Doc};
use super::gather;
use super::require::{self, Found, Origin};
use super::state::{Located, Session};
use super::variables;
use crate::cli::position::{self, Under};
use crate::core::{Def, Kind};
use crate::resolve::refs;
use lsp_types::Uri as Url;
use lsp_types::{
    CallHierarchyIncomingCall, CallHierarchyIncomingCallsParams, CallHierarchyItem,
    CallHierarchyOutgoingCall, CallHierarchyOutgoingCallsParams, CallHierarchyPrepareParams,
    Diagnostic, DiagnosticSeverity, DocumentLink, DocumentLinkParams, DocumentSymbol,
    DocumentSymbolParams, DocumentSymbolResponse, GotoDefinitionParams, GotoDefinitionResponse,
    Hover, HoverContents, HoverParams, Location, MarkupContent, MarkupKind, ReferenceParams,
    SymbolKind, WorkspaceSymbolParams,
};
use std::collections::{HashMap, HashSet};
use std::ops::ControlFlow;
use std::path::Path;

/// How many ranked guesses `goToDefinition` offers when the receiver did not
/// resolve.
///
/// Five, because an editor shows a picker and a human scans it — Ruby LSP's
/// fallback is the first ten methods with the name, which is where "ranked"
/// stops meaning anything. `hover` at the same position says these are guesses.
const MAX_GUESSES: usize = 5;

/// A location from our `path:line:col`, resolved against the checkout the path
/// came out of, spanning `len` bytes of the name there.
///
/// `text` is the file's contents when the caller already has them. Without
/// them the file is read, because a column is bytes and LSP counts UTF-16 — a
/// line with an `é` before the name is off by one otherwise.
fn location(
    root: &Path,
    path: &str,
    line: u32,
    col: u32,
    len: usize,
    text: Option<&str>,
) -> Option<Location> {
    let absolute = absolute_site(root, path)?;
    let read;
    let text = match text {
        Some(text) => Some(text),
        None => {
            read = std::fs::read_to_string(&absolute).ok();
            read.as_deref()
        }
    };
    let uri: Url = path_to_uri(&absolute).parse().ok()?;
    // Sized by what is written there, which is not always the name asked
    // about; a zero width is a point, and stays one.
    let (col, len) = match text.filter(|_| len > 0) {
        Some(text) => convert::written_at(text, line, col).unwrap_or((col, len)),
        None => (col, len),
    };
    Some(Location {
        uri,
        range: convert::span(text, line, col, len),
    })
}

/// A location without reading the file, for answers too numerous to read
/// every file behind: columns are taken as ASCII.
fn unread_location(root: &Path, path: &str, line: u32, col: u32, len: usize) -> Option<Location> {
    let absolute = absolute_site(root, path)?;
    Some(Location {
        uri: path_to_uri(&absolute).parse().ok()?,
        range: convert::span(None, line, col, len),
    })
}

/// The URI of a site path.
fn file_uri(root: &Path, path: &str) -> Option<Url> {
    path_to_uri(&absolute_site(root, path)?).parse().ok()
}

/// Where a site path lives on disk.
pub(super) fn absolute_site(root: &Path, path: &str) -> Option<std::path::PathBuf> {
    if crate::tree::is_core(path) {
        // Core's stubs live in the store; they are written out beside the
        // database so that `require` and `Array#each` land on a readable
        // signature instead of answering nothing.
        let dir = crate::store::core_dir().ok()?;
        crate::tree::core_file_of(&dir, path).map(|(dir, file)| dir.join(file))
    } else if path.starts_with('<') {
        None
    } else if Path::new(path).is_absolute() {
        // A gem site is already an absolute path.
        Some(std::path::PathBuf::from(path))
    } else {
        Some(root.join(path))
    }
}

/// The last segment of a constant path — what is written at a reference's
/// recorded position.
fn last_segment(name: &str) -> &str {
    name.rsplit("::").next().unwrap_or(name)
}

/// The file a request names, wherever it lives. No checkout required — enough
/// for the questions that are answered from the file's own bytes.
fn file_of(uri: &Url) -> Option<std::path::PathBuf> {
    let path = convert::uri_to_path(uri.as_str())?;
    Some(std::fs::canonicalize(&path).unwrap_or(path))
}

/// The checkout and position a resolving request is about.
fn target(
    session: &mut Session,
    uri: &Url,
    position: lsp_types::Position,
) -> Option<(Located, crate::core::Pos)> {
    let path = convert::uri_to_path(uri.as_str())?;
    let located = session.locate_query(&path)?;
    let text = session.document(&located.absolute)?.text.clone();
    Some((located, to_pos(&text, position)))
}

pub(crate) fn definition(
    session: &mut Session,
    params: GotoDefinitionParams,
) -> anyhow::Result<Option<GotoDefinitionResponse>> {
    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;
    if let Some(required) = required_at(session, &uri, position) {
        crate::usage::flag("require");
        return Ok(required_definition(session.definition_links, required));
    }
    if let Some(under) = variables::under(session, &uri, position) {
        let locations = variables::definition(session, &under);
        if locations.is_empty() {
            super::miss::why("a variable with no write in reach");
        }
        return Ok((!locations.is_empty()).then_some(GotoDefinitionResponse::Array(locations)));
    }
    let Some((located, pos)) = target(session, &uri, position) else {
        super::miss::why(NO_CHECKOUT);
        return Ok(None);
    };
    let name_len = name_at(session, &located, pos).map_or(0, |n| last_segment(&n).len());
    let sites = resolve_at(session, &located, pos)?;
    let locations: Vec<Location> = sites
        .into_iter()
        .filter_map(|(p, line, col)| location(&located.root, &p, line, col, name_len, None))
        .collect();
    Ok((!locations.is_empty()).then_some(GotoDefinitionResponse::Array(locations)))
}

/// Say once per checkout, the first time a definition, references or
/// implementation is asked there while its first index fills the store, that
/// those answers come from what is read so far (DEC-331). A hover says it in
/// its text and completion with `isIncomplete`; these have no field for it.
pub(crate) fn tell_warming(
    session: &mut Session,
    path: &std::path::Path,
    out: &super::Outbound,
) -> anyhow::Result<()> {
    let Some(located) = session.locate_query(path) else {
        return Ok(());
    };
    if session.told_warming.contains(&located.root) {
        return Ok(());
    }
    let Some(warming) = session.warming(&located.root) else {
        return Ok(());
    };
    session.told_warming.insert(located.root);
    let read = super::fresh::how_far(&warming);
    let message = match warming.interrupted {
        false => format!(
            "trekr is still indexing this checkout ({read}). Until it finishes, go to \
             definition and references answer from what is read so far, and may miss or change."
        ),
        true => format!(
            "trekr's index of this checkout was cut short ({read}). Until it is indexed \
             again, go to definition and references answer from what was read, and may miss \
             or change."
        ),
    };
    out.notify(
        "window/showMessage",
        serde_json::json!({ "type": 3, "message": message }),
    )
}

/// A `require` string under the cursor, and the files it names.
struct Required {
    /// The whole string literal: where the cursor is inside it does not
    /// matter, and a path segment is not a target of its own — Ruby resolves
    /// the whole string, and a directory is not something to open.
    range: lsp_types::Range,
    path: String,
    found: Vec<Found>,
    root: Option<std::path::PathBuf>,
}

fn required_at(
    session: &mut Session,
    uri: &Url,
    position: lsp_types::Position,
) -> Option<Required> {
    let file = file_of(uri)?;
    let document = session.document(&file)?;
    let offset = convert::offset_of(&document.text, position);
    let require = document
        .requires()
        .iter()
        .find(|r| r.span.contains(&offset))?
        .clone();
    let range = LineIndex::new(&document.text).range(require.span.clone());
    let root = session.locate_query(&file).map(|located| located.root);
    let cx = require::Context {
        file: &file,
        root: root.as_deref(),
        load_path: session.load_path(root.as_deref()),
    };
    let found = require::resolve(&require, &cx, Path::is_file);
    Some(Required {
        range,
        path: require.path,
        found,
        root,
    })
}

/// The required file, opened at its top. A compiled extension has nothing to
/// open, and is not swapped for a `.rb` Ruby would not load.
fn required_definition(links: bool, required: Required) -> Option<GotoDefinitionResponse> {
    let top = lsp_types::Range::default();
    let targets: Vec<Url> = required
        .found
        .iter()
        .filter(|found| !found.native)
        .filter_map(|found| path_to_uri(&found.file).parse().ok())
        .collect();
    if targets.is_empty() {
        return None;
    }
    Some(match links {
        true => GotoDefinitionResponse::Link(
            targets
                .into_iter()
                .map(|target_uri| lsp_types::LocationLink {
                    origin_selection_range: Some(required.range),
                    target_uri,
                    target_range: top,
                    target_selection_range: top,
                })
                .collect(),
        ),
        false => GotoDefinitionResponse::Array(
            targets
                .into_iter()
                .map(|uri| Location { uri, range: top })
                .collect(),
        ),
    })
}

/// Which file a `require` loads, and from where — a gem by name and version.
/// Several matches are listed, since which one Ruby loads depends on a load
/// path this engine only knows by convention.
fn required_hover(required: &Required) -> String {
    let root = required.root.as_deref();
    match required.found.as_slice() {
        [] => format!(
            "_No file found for `{}` — a gem that is not installed, or a path set up at runtime._",
            required.path
        ),
        [one] => format!("Loads {}", found_line(one, root)),
        several => {
            let listed: Vec<String> = several
                .iter()
                .map(|found| format!("- {}", found_line(found, root)))
                .collect();
            format!(
                "{} files match `{}`; Ruby loads the first on its load path at runtime:\n\n{}",
                several.len(),
                required.path,
                listed.join("\n")
            )
        }
    }
}

/// One found file: linked, shown within its gem or checkout, and named.
fn found_line(found: &Found, root: Option<&Path>) -> String {
    let (shown, whence) = found_place(found, root);
    let shown = match found.native {
        true => format!("`{shown}`, a compiled extension"),
        false => format!("[`{shown}`]({})", path_to_uri(&found.file)),
    };
    match whence {
        Some(whence) => format!("{shown} · {whence}"),
        None => shown,
    }
}

/// A found file's path within whatever holds it, and what that is when it is
/// not the checkout.
fn found_place(found: &Found, root: Option<&Path>) -> (String, Option<String>) {
    let within = |base: &Path| {
        found
            .file
            .strip_prefix(base)
            .map(|p| p.to_string_lossy().into_owned())
            .ok()
    };
    let name = |dir: &Path| {
        dir.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let whole = found.file.to_string_lossy();
    match &found.origin {
        Origin::Gem(gem) => (
            within(gem).unwrap_or_else(|| whole.to_string()),
            Some(format!("gem `{}`", name(gem))),
        ),
        Origin::Stdlib(dir) => (
            within(dir).unwrap_or_else(|| whole.to_string()),
            Some(format!("Ruby {} standard library", name(dir))),
        ),
        Origin::Checkout => (
            root.and_then(within)
                .unwrap_or_else(|| crate::core::paths::pretty(&whole)),
            None,
        ),
    }
}

/// `require` strings as links, so they can be followed without asking.
///
/// Only a string with exactly one file behind it is a link: a link has one
/// target, and picking one of several would be a guess made silently.
/// Those — and anything unresolved — are still answered by `definition`,
/// with every match.
pub(crate) fn document_link(
    session: &mut Session,
    params: DocumentLinkParams,
) -> anyhow::Result<Option<Vec<DocumentLink>>> {
    let Some(file) = file_of(&params.text_document.uri) else {
        return Ok(None);
    };
    let Some(document) = session.document(&file) else {
        return Ok(None);
    };
    let requires = document.requires().to_vec();
    if requires.is_empty() {
        return Ok(None);
    }
    let text = document.text.clone();
    let lines = LineIndex::new(&text);
    let root = session.locate_query(&file).map(|located| located.root);
    let cx = require::Context {
        file: &file,
        root: root.as_deref(),
        load_path: session.load_path(root.as_deref()),
    };
    let mut links = Vec::new();
    for require in requires {
        let found = require::resolve(&require, &cx, Path::is_file);
        let [one] = found.as_slice() else {
            continue;
        };
        if one.native {
            continue;
        }
        let (shown, whence) = found_place(one, root.as_deref());
        links.push(DocumentLink {
            range: lines.range(require.span),
            target: path_to_uri(&one.file).parse().ok(),
            tooltip: Some(match whence {
                Some(whence) => format!("{shown} ({})", whence.replace('`', "")),
                None => shown,
            }),
            data: None,
        });
    }
    Ok(Some(links))
}

/// The name under a position, as written.
fn name_at(session: &mut Session, located: &Located, pos: crate::core::Pos) -> Option<String> {
    let facts = session.document(&located.absolute)?.facts().clone();
    Some(match position::at_facts(&facts, pos.line, pos.col)? {
        Under::Definition(def) => def.name,
        Under::Call(call) => call.name,
        Under::Constant(reference) => reference.name,
    })
}

/// Where the name at a position is defined — the CLI's `--def`, as sites.
fn resolve_at(
    session: &mut Session,
    located: &Located,
    pos: crate::core::Pos,
) -> anyhow::Result<Vec<(String, u32, u32)>> {
    let facts = session
        .document(&located.absolute)
        .map(|document| document.facts().clone());
    let Some(facts) = facts else {
        super::miss::why(UNREADABLE);
        return Ok(Vec::new());
    };
    let Some(under) = position::at_facts(&facts, pos.line, pos.col) else {
        super::miss::why(NO_NAME);
        return Ok(Vec::new());
    };
    let path = located.relative.clone();
    let unresolved = session.unresolved;
    let tree = session.tree(&located.root)?;
    Ok(match under {
        Under::Definition(def) => vec![(path, def.pos.line, def.pos.col)],
        Under::Constant(reference) => {
            let sites = tree
                .resolve_at(&reference.name, &reference.nesting, &path)
                .sites;
            if sites.is_empty() {
                super::miss::why(format!("constant `{}` not found", reference.name));
            }
            sites
                .into_iter()
                .map(|site| (site.path, site.line, site.col))
                .collect()
        }
        Under::Call(call) => {
            let mut answer = crate::resolve::method_at(tree, &facts, &call, &path);
            // A shared group's body reads what its includers define (DEC-490).
            // Only then are the open buffers copied: never for app code.
            if crate::resolve::members::includers_may_answer(&call, &answer) {
                let open = overlay(session, &located.root);
                let (tree, store) = session.tree_and_store(&located.root)?;
                let root = located.root.to_string_lossy().into_owned();
                let files = crate::cli::members::CheckoutFiles::with_open(
                    store,
                    &located.root,
                    &root,
                    open,
                );
                if let Some(includers) =
                    crate::cli::members::includer_answer(tree, &files, &path, &call, &answer)
                {
                    answer = includers;
                }
            }
            note_uncertain(&answer);
            if !answer.sites.is_empty() {
                answer
                    .sites
                    .into_iter()
                    .map(|site| (site.path, site.line, site.col))
                    .collect()
            } else {
                // Residue is not "nothing known": the ranked candidates are an
                // answer, and order is the disclosure, as it is for references.
                // But an editor shows a guess as it shows an answer, so how
                // many get through is the client's setting (DEC-443); `hover`
                // at the same position says the receiver was never resolved.
                let keep = unresolved.keep(answer.confidence);
                if keep == 0 && !answer.candidates.is_empty() {
                    super::miss::why(unresolved.withheld());
                }
                answer
                    .candidates
                    .into_iter()
                    .take(MAX_GUESSES.min(keep))
                    .map(|candidate| (candidate.site.path, candidate.site.line, candidate.site.col))
                    .collect()
            }
        }
    })
}

/// Why a click found nothing, when nothing more specific was noted.
const NO_CHECKOUT: &str = "the file is in no checkout trekr knows";
const UNREADABLE: &str = "the file could not be read";
const NO_NAME: &str = "no name at this position";

/// A call answer short of certain is counted `uncertain`, with its reason
/// noted for the miss log — the same rule for definition and hover.
fn note_uncertain(answer: &crate::resolve::MethodAnswer) {
    use crate::tree::Status;
    if answer.status == Status::Resolved && answer.confidence >= crate::usage::LOW_CONFIDENCE {
        return;
    }
    crate::usage::outcome(crate::usage::Outcome::Uncertain);
    let status = match answer.status {
        Status::Resolved => "low confidence",
        Status::Ambiguous => "ambiguous",
        Status::Residue => "residue",
    };
    let typed = answer
        .receiver_type
        .as_deref()
        .map(|t| format!(" `{t}`"))
        .unwrap_or_default();
    let mut why = format!("{status}; receiver {}{typed}", answer.receiver);
    if let Some(via) = &answer.resolved_via {
        why.push_str(&format!(" via {via}"));
    }
    if let Some(reason) = &answer.reason {
        why.push_str(&format!(": {reason}"));
    }
    super::miss::why(why);
}

/// Every mention of the variable under the cursor in this file, reads and
/// writes told apart. Anything else is left to the editor's word matching.
pub(crate) fn document_highlight(
    session: &mut Session,
    params: lsp_types::DocumentHighlightParams,
) -> anyhow::Result<Option<Vec<lsp_types::DocumentHighlight>>> {
    let position = params.text_document_position_params;
    Ok(
        variables::under(session, &position.text_document.uri, position.position)
            .map(|under| variables::highlight(&under)),
    )
}

/// Call sites of the method at a position, confirmed before possible, at most
/// `reference_limit` of them — and a word to the user when that cut anything.
///
/// With a `partialResultToken` the answer streams as `$/progress` batches, one
/// per chunk of files read, nearest the definition first; each batch is
/// ordered by evidence, and the response itself is empty, as the protocol
/// requires. Without one, the answer is the best `limit` of the whole scan
/// (DEC-056).
pub(crate) fn references(
    session: &mut Session,
    params: ReferenceParams,
    out: &super::Outbound,
    cancel: &dyn Fn() -> bool,
) -> anyhow::Result<Option<Vec<Location>>> {
    let token = params.partial_result_params.partial_result_token;
    let uri = params.text_document_position.text_document.uri;
    let position = params.text_document_position.position;
    let declarations = params.context.include_declaration;
    if let Some(under) = variables::under(session, &uri, position) {
        return Ok(Some(variables::references(session, &under)));
    }
    let Some((located, pos)) = target(session, &uri, position) else {
        return Ok(None);
    };
    let facts = session
        .document(&located.absolute)
        .map(|document| document.facts().clone());
    let Some(facts) = facts else { return Ok(None) };
    let Some(under) = position::at_facts(&facts, pos.line, pos.col) else {
        return Ok(None);
    };
    // An example group's own `let`, `subject` or `def`: read by Ruby's lookup
    // at runtime, as `--refs` lists it (DEC-490).
    if let Some(found) = member_references(session, &located, pos, declarations)? {
        return Ok(Some(found));
    }

    // A class, module or constant is a different question from a method: its
    // references are constant references, resolved by Ruby's lookup rather
    // than by a receiver.
    let constant = match &under {
        Under::Definition(def) if def.kind != crate::core::Kind::Method => {
            Some((def.name.clone(), def.nesting.clone()))
        }
        Under::Constant(reference) => Some((reference.name.clone(), reference.nesting.clone())),
        _ => None,
    };
    if let Some((name, nesting)) = constant {
        return constant_references(session, &located, &name, &nesting, declarations, out)
            .map(Some);
    }

    let root = located.root.clone();
    let root_str = root.to_string_lossy().into_owned();
    let path = located.relative.clone();
    let (name, own_site) = match &under {
        Under::Definition(def) => (def.name.clone(), Some(def.pos)),
        Under::Call(call) => (call.name.clone(), None),
        Under::Constant(_) => unreachable!("answered above"),
    };
    let overlay = overlay(session, &root);
    let limit = session.reference_limit;
    let warming = session.warming(&root);
    let (tree, store) = session.tree_and_store(&root)?;

    // Which method is being asked about, not just which name. Standing on a
    // definition, the owner is the scope that declares it; standing on a call,
    // it is wherever that call resolves. Without this the answer merges every
    // same-named method in the repo — which is the grep this exists to beat.
    let (query, defined_at) = match &under {
        Under::Definition(def) => (
            refs::Query {
                owner: tree.scope_fqn(&def.nesting),
                singleton: def.singleton,
                name: name.clone(),
            },
            Vec::new(),
        ),
        Under::Call(call) => {
            let answer = crate::resolve::method_at(tree, &facts, call, &path);
            let (name, singleton) = crate::resolve::asked_at(tree, call, &answer);
            (
                refs::Query {
                    owner: answer.owner,
                    singleton,
                    name,
                },
                answer.sites,
            )
        }
        Under::Constant(_) => unreachable!("answered above"),
    };
    // An `X.new` asks about the `initialize` it runs (DEC-541).
    let name = query.name.clone();
    let target = query.owner.clone();
    let bare = target.is_none();

    let mut declared: Vec<Location> = Vec::new();
    if declarations {
        match own_site {
            Some(pos) => declared.extend(location(
                &root,
                &path,
                pos.line,
                pos.col,
                name.len(),
                overlay.get(&path).map(String::as_str),
            )),
            None => declared.extend(defined_at.iter().filter_map(|site| {
                location(&root, &site.path, site.line, site.col, name.len(), None)
            })),
        }
    }
    let send = |locations: Vec<Location>| -> anyhow::Result<()> {
        match &token {
            Some(token) if !locations.is_empty() => out.notify(
                "$/progress",
                serde_json::json!({ "token": token, "value": locations }),
            ),
            _ => Ok(()),
        }
    };
    if token.is_some() {
        // The definition is known before any file is read: it is the first
        // thing a streaming client can show.
        send(std::mem::take(&mut declared))?;
    }

    let source = if bare {
        // Nothing to be near, and the first `limit` will do: read the index
        // only as far as they take.
        Source::Paged {
            store,
            root: &root_str,
            name: &name,
            after: 0,
            done: false,
            pending: Default::default(),
        }
    } else {
        // Nearest the definition first, so what a stream shows first is the
        // code most likely to be about this method.
        let anchor = match &own_site {
            Some(_) => path.as_str(),
            None => defined_at
                .iter()
                .map(|site| site.path.as_str())
                .find(|p| !Path::new(p).is_absolute())
                .unwrap_or(path.as_str()),
        };
        let mut paths = store.files_calling_any(&root_str, &refs::called_as(&query))?;
        gather::nearest_first(&mut paths, anchor);
        Source::Listed(paths.into_iter())
    };
    let policy = if token.is_some() || bare {
        gather::Policy::First
    } else {
        gather::Policy::Best
    };
    let mut gathered = gather::Gather::new(limit, policy);
    let partial = warming.is_some();
    let tier = |facts: &crate::core::Facts, call: &crate::core::Call, path: &str| {
        refs::tier_call(tree, facts, call, path, &query, target.as_deref())
    };
    let reach = scan_files(&overlay, &root, source, &query, cancel, &tier, |files| {
        for file in files {
            let Some(uri) = file_uri(&root, &file.path) else {
                continue;
            };
            let lines = LineIndex::new(&file.text);
            for (_, mut reference) in file.tiered {
                // A partial index rules nothing out (DEC-320).
                if partial {
                    reference.unrule();
                }
                if reference.tier == refs::Tier::Excluded {
                    continue;
                }
                // A `super` site is named after its method but spelled `super`.
                let written = match reference.receiver {
                    "super" => "super".len(),
                    _ => reference.called_as.unwrap_or(&name).len(),
                };
                let range = lines.span(reference.line, reference.col, written);
                let (tier, proximity, path, line) = refs::order(&reference);
                gathered.offer(
                    (tier, proximity, path, line, reference.col),
                    Location {
                        uri: uri.clone(),
                        range,
                    },
                );
            }
        }
        if token.is_some() {
            // A withdrawn request sends nothing more, not one more batch.
            if cancel() {
                return Err(super::Cancelled.into());
            }
            send(gathered.batch())?;
        }
        Ok(if gathered.settled() {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        })
    })?;

    let written = match &query.owner {
        Some(owner) => format!("{owner}{}{name}", if query.singleton { "." } else { "#" }),
        None => name.clone(),
    };
    let found = gathered.found;
    let shown = gathered.kept();
    let cut = gather::Cut {
        name: &name,
        query: &written,
        shown,
        found,
        read: reach.read,
        files: reach.files,
        stopped: reach.stopped,
        of: if bare {
            gather::Of::BareName
        } else {
            gather::Of::Method
        },
    };
    out.log.event(
        "references",
        serde_json::json!({
            "query": written,
            "streamed": token.is_some(),
            "shown": shown,
            "found": found,
            "files_read": reach.read,
            "files": reach.files,
            "cut": cut.is_cut(),
        }),
    );
    if cut.is_cut() {
        crate::usage::flag("cut");
        out.notify(
            "window/showMessage",
            serde_json::json!({ "type": 3, "message": cut.message() }),
        )?;
    }
    if token.is_some() {
        // Everything went out as partial results; the protocol has the final
        // response carry none of them.
        return Ok(Some(Vec::new()));
    }
    declared.extend(gathered.finish());
    Ok(Some(declared))
}

/// The reads of the example group member at `pos`, or `None` when no member
/// is there: the same reads `--refs` and `--dead` find.
fn member_references(
    session: &mut Session,
    located: &Located,
    pos: crate::core::Pos,
    declarations: bool,
) -> anyhow::Result<Option<Vec<Location>>> {
    use crate::cli::members::{CheckoutFiles, member_at_position};
    use crate::resolve::members::{Asked, Context, reads};
    let root = located.root.clone();
    let root_str = root.to_string_lossy().into_owned();
    let open = overlay(session, &root);
    let (tree, store) = session.tree_and_store(&root)?;
    let files = CheckoutFiles::with_open(store, &root, &root_str, open);
    let Some((path, def)) = member_at_position(tree, &files, &located.relative, pos.line, pos.col)
    else {
        return Ok(None);
    };
    let context = Context::new(tree, &files);
    let found = reads(
        &context,
        &Asked {
            path: &path,
            def: &def,
        },
        false,
    );
    let text = |path: &str| files.open_text(path);
    let mut locations: Vec<Location> = Vec::new();
    if declarations {
        locations.extend(location(
            &root,
            &path,
            def.pos.line,
            def.pos.col,
            def.name.len(),
            text(&path),
        ));
    }
    for reference in &found.found {
        let len = written_len(
            &root,
            &reference.path,
            reference.line,
            reference.col,
            text(&reference.path),
        );
        locations.extend(location(
            &root,
            &reference.path,
            reference.line,
            reference.col,
            len,
            text(&reference.path),
        ));
    }
    Ok(Some(locations))
}

/// How long the name written at a position is: a read of a member may be
/// spelled `super` or `is_expected` rather than its name.
fn written_len(root: &Path, path: &str, line: u32, col: u32, text: Option<&str>) -> usize {
    let read;
    let text = match text {
        Some(text) => text,
        None => {
            read = absolute_site(root, path).and_then(|at| std::fs::read_to_string(at).ok());
            read.as_deref().unwrap_or_default()
        }
    };
    let Some(written) = text.lines().nth(line.saturating_sub(1) as usize) else {
        return 0;
    };
    written
        .get(col.saturating_sub(1) as usize..)
        .unwrap_or_default()
        .chars()
        .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '?' | '!'))
        .map(char::len_utf8)
        .sum()
}

/// References to a class, module or constant: every written constant that
/// Ruby's lookup resolves to the same fully-qualified name.
///
/// The index records a reference under every name it could be written as —
/// `Base`, `ActiveRecord::Base` — so each suffix of the target is asked for,
/// and each row resolved in the nesting it was written in. Same-named
/// constants elsewhere resolve elsewhere and drop out, which is the point.
fn constant_references(
    session: &mut Session,
    located: &Located,
    name: &str,
    nesting: &[String],
    declarations: bool,
    out: &super::Outbound,
) -> anyhow::Result<Vec<Location>> {
    let limit = session.reference_limit;
    let root = located.root.clone();
    let root_str = root.to_string_lossy().into_owned();
    let overlay = overlay(session, &root);
    let Some(fqn) = session.tree(&root)?.resolve(name, nesting).fqn else {
        return Ok(Vec::new());
    };

    let segments: Vec<&str> = fqn.split("::").collect();
    let mut rows = Vec::new();
    for start in 0..segments.len() {
        let suffix = segments[start..].join("::");
        let mut spellings = vec![suffix.clone()];
        if start == 0 {
            spellings.push(format!("::{suffix}"));
        }
        for written in spellings {
            let found = session.store().refs(&root_str, &written)?;
            rows.extend(found.into_iter().map(|row| (written.clone(), row)));
        }
    }
    let tree = session.tree(&root)?;
    let tail = last_segment(&fqn);

    // (path, line, col) — the index's view, except for files the editor has
    // open, which are read from the buffer so an unsaved edit counts.
    let mut sites: Vec<(String, u32, u32)> = rows
        .into_iter()
        .filter(|(_, row)| row.role == "constant" && !overlay.contains_key(&row.path))
        .filter(|(written, row)| tree.resolve(written, &row.nesting).fqn.as_deref() == Some(&fqn))
        .map(|(_, row)| (row.path, row.line, row.col))
        .collect();
    for (path, text) in &overlay {
        let facts = crate::extract::extract_file(path, text.as_bytes());
        sites.extend(
            facts
                .const_refs
                .iter()
                .filter(|r| last_segment(&r.name) == tail)
                .filter(|r| tree.resolve(&r.name, &r.nesting).fqn.as_deref() == Some(&fqn))
                .map(|r| (path.clone(), r.pos.line, r.pos.col)),
        );
    }
    sites.sort();
    sites.dedup();
    let cut = gather::Cut {
        name: tail,
        query: tail,
        shown: sites.len().min(limit),
        found: sites.len(),
        read: 0,
        files: None,
        stopped: false,
        of: gather::Of::Constant,
    };
    if cut.is_cut() {
        crate::usage::flag("cut");
        out.notify(
            "window/showMessage",
            serde_json::json!({ "type": 3, "message": cut.message() }),
        )?;
    }
    sites.truncate(limit);

    let mut locations = Vec::new();
    if declarations {
        locations.extend(
            tree.sites(&fqn).iter().filter_map(|site| {
                location(&root, &site.path, site.line, site.col, tail.len(), None)
            }),
        );
    }
    // One read per file, not per reference: the column conversion needs the
    // line, and a popular constant has hundreds of references in one file.
    for group in sites.chunk_by(|a, b| a.0 == b.0) {
        let path = &group[0].0;
        let Some(uri) = file_uri(&root, path) else {
            continue;
        };
        let text = match overlay.get(path) {
            Some(text) => text.clone(),
            None => std::fs::read_to_string(root.join(path)).unwrap_or_default(),
        };
        let lines = LineIndex::new(&text);
        locations.extend(group.iter().map(|(_, line, col)| Location {
            uri: uri.clone(),
            range: lines.span(*line, *col, tail.len()),
        }));
    }
    Ok(locations)
}

/// The editor's unsaved buffers in a checkout, keyed by checkout-relative
/// path — what a question that reads many files consults before disk.
fn overlay(session: &Session, root: &Path) -> HashMap<String, String> {
    session
        .editor_documents_under(root)
        .into_iter()
        .filter_map(|(path, text)| {
            let relative = path.strip_prefix(root).ok()?.to_string_lossy().into_owned();
            Some((relative, text))
        })
        .collect()
}

/// One file read for a question that spans the checkout: each call of the
/// name asked about, by its index in `facts.calls`, with its tier.
struct Scanned {
    path: String,
    text: String,
    facts: crate::core::Facts,
    tiered: Vec<(usize, refs::Reference)>,
}

/// How many files to parse between cancellation checks. Large enough that the
/// parallel parse has something to chew on, small enough that a withdrawn
/// request stops within a few tens of milliseconds.
const SCAN_CHUNK: usize = 128;

/// Where a scan's files come from.
enum Source<'a> {
    /// All of them, listed up front, for a scan that reads every one.
    Listed(std::vec::IntoIter<String>),
    /// Page by page from the index, only as far as the scan gets — listing
    /// every file that calls a common name costs more than the answer does.
    Paged {
        store: &'a crate::store::Store,
        root: &'a str,
        name: &'a str,
        after: i64,
        done: bool,
        /// Read from the index and not yet handed out.
        pending: std::collections::VecDeque<String>,
    },
}

/// Blobs per page of a paged listing — a few chunks of files' worth.
const PAGE_ROWS: i64 = 4096;

impl Source<'_> {
    /// Up to `n` paths not yet in `seen`; empty when there are no more.
    fn next(&mut self, seen: &mut HashSet<String>, n: usize) -> anyhow::Result<Vec<String>> {
        let mut paths = Vec::new();
        match self {
            Source::Listed(listed) => {
                for path in listed.by_ref() {
                    if seen.insert(path.clone()) {
                        paths.push(path);
                        if paths.len() == n {
                            break;
                        }
                    }
                }
            }
            Source::Paged {
                store,
                root,
                name,
                after,
                done,
                pending,
            } => {
                while paths.len() < n {
                    if let Some(path) = pending.pop_front() {
                        paths.push(path);
                        continue;
                    }
                    if *done {
                        break;
                    }
                    // A path already `seen` — an open buffer, read first —
                    // is not read again.
                    let (last, page) = store.files_calling_page(root, name, *after, PAGE_ROWS)?;
                    *done = page.is_empty();
                    *after = last;
                    pending.extend(page.into_iter().filter(|path| seen.insert(path.clone())));
                }
            }
        }
        Ok(paths)
    }

    /// How many files there are with those already `seen`, when that is
    /// known without reading them.
    fn total(&self, seen: &HashSet<String>) -> Option<usize> {
        match self {
            Source::Listed(listed) => {
                let unseen = listed.as_slice().iter().filter(|p| !seen.contains(*p));
                Some(seen.len() + unseen.count())
            }
            Source::Paged { .. } => None,
        }
    }
}

/// How far a scan got.
struct Reach {
    /// Files read.
    read: usize,
    /// Files there were to read, when the source knew.
    files: Option<usize>,
    /// Stopped with files still unread.
    stopped: bool,
}

/// Read, parse and tier the files a question needs — the editor's copy where
/// it has one — in parallel, handing each chunk to `visit` in order until it
/// says stop.
///
/// The parse and the tiering are the expensive part, and the tree `tier`
/// consults is shared by every worker (DEC-250), so both fan out; `visit`
/// runs on this thread, in file order, so a stream and the cap (DEC-056)
/// see what they did. An open buffer that mentions a name the query is
/// called as is read first,
/// even when the index does not list its file: the index is as of the last
/// save, and the buffer is what the user is looking at.
fn scan_files(
    overlay: &HashMap<String, String>,
    root: &Path,
    mut source: Source,
    query: &refs::Query,
    cancel: &dyn Fn() -> bool,
    tier: &(dyn Fn(&crate::core::Facts, &crate::core::Call, &str) -> refs::Reference + Sync),
    mut visit: impl FnMut(Vec<Scanned>) -> anyhow::Result<ControlFlow<()>>,
) -> anyhow::Result<Reach> {
    use rayon::prelude::*;
    let mut seen = HashSet::new();
    let mut open: Vec<String> = overlay
        .iter()
        .filter(|(_, text)| refs::called_as(query).iter().any(|n| text.contains(n)))
        .map(|(path, _)| path.clone())
        .collect();
    open.sort();
    seen.extend(open.iter().cloned());
    // Only needed if the scan stops short; a finished one read them all.
    let files = source.total(&seen);
    let mut read = 0;
    let mut chunk = open;
    loop {
        if chunk.len() < SCAN_CHUNK {
            chunk.extend(source.next(&mut seen, SCAN_CHUNK - chunk.len())?);
        }
        if chunk.is_empty() {
            return Ok(Reach {
                read,
                files: Some(read),
                stopped: false,
            });
        }
        if cancel() {
            return Err(super::Cancelled.into());
        }
        let scanned: Vec<Scanned> = chunk
            .par_iter()
            .filter_map(|path| {
                let text = match overlay.get(path) {
                    Some(text) => text.clone(),
                    None => {
                        String::from_utf8_lossy(&std::fs::read(root.join(path)).ok()?).into_owned()
                    }
                };
                let facts = crate::extract::extract_file(path, text.as_bytes());
                let tiered = facts
                    .calls
                    .iter()
                    .enumerate()
                    .filter(|(_, call)| refs::names_it(call, query))
                    .map(|(at, call)| (at, tier(&facts, call, path)))
                    .collect();
                Some(Scanned {
                    path: path.clone(),
                    text,
                    facts,
                    tiered,
                })
            })
            .collect();
        read += chunk.len();
        chunk = Vec::new();
        if visit(scanned)?.is_break() {
            let stopped = !source.next(&mut seen, 1)?.is_empty();
            return Ok(Reach {
                read,
                files: if stopped { files } else { Some(read) },
                stopped,
            });
        }
    }
}

/// An outline needs the file's bytes and nothing else — no index, no checkout,
/// no `--index` ever having been run.
pub(crate) fn document_symbol(
    session: &mut Session,
    params: DocumentSymbolParams,
) -> anyhow::Result<Option<DocumentSymbolResponse>> {
    let Some(path) = file_of(&params.text_document.uri) else {
        return Ok(None);
    };
    let facts = session
        .document(&path)
        .map(|document| document.facts().clone());
    let Some(facts) = facts else { return Ok(None) };

    let text = session
        .document(&path)
        .map(|document| document.text.clone())
        .unwrap_or_default();
    Ok(Some(DocumentSymbolResponse::Nested(outline(
        &facts.defs,
        &text,
    ))))
}

/// Definitions nested by containment — methods inside their class, a class
/// inside its module — each spanning its whole body and selecting its name.
///
/// Flat, zero-width symbols made the outline a list and broke breadcrumbs and
/// sticky scroll, which read the range to know what the cursor is inside.
fn outline(defs: &[crate::core::Def], text: &str) -> Vec<DocumentSymbol> {
    // Source order, outermost first on a shared line, so a parent is always
    // seen before its children.
    let mut order: Vec<&crate::core::Def> = defs.iter().collect();
    order.sort_by_key(|def| (def.pos.line, std::cmp::Reverse(def.end_line), def.pos.col));

    // Each entry: the symbol being built and the last line it covers.
    let mut stack: Vec<(DocumentSymbol, u32)> = Vec::new();
    let mut roots: Vec<DocumentSymbol> = Vec::new();
    let close = |stack: &mut Vec<(DocumentSymbol, u32)>, roots: &mut Vec<DocumentSymbol>| {
        let (done, _) = stack.pop().expect("only called on a non-empty stack");
        match stack.last_mut() {
            Some((parent, _)) => parent.children.get_or_insert_with(Vec::new).push(done),
            None => roots.push(done),
        }
    };
    for def in order {
        while stack.last().is_some_and(|(_, last)| def.pos.line > *last) {
            close(&mut stack, &mut roots);
        }
        let name = if def.singleton && def.kind == crate::core::Kind::Method {
            format!("self.{}", def.name)
        } else {
            def.name.clone()
        };
        #[allow(deprecated)]
        let symbol = DocumentSymbol {
            name,
            detail: def.via.clone(),
            kind: symbol_kind(def.kind),
            tags: None,
            deprecated: None,
            range: convert::block(Some(text), def.pos.line, def.end_line),
            selection_range: convert::span(
                Some(text),
                def.pos.line,
                def.pos.col,
                last_segment(&def.name).len(),
            ),
            children: None,
        };
        stack.push((symbol, def.end_line.max(def.pos.line)));
    }
    while !stack.is_empty() {
        close(&mut stack, &mut roots);
    }
    roots
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
    session: &mut Session,
    params: WorkspaceSymbolParams,
) -> anyhow::Result<Option<Vec<lsp_types::SymbolInformation>>> {
    let root = session.root.to_string_lossy().into_owned();
    // A client's root is often not a checkout this engine has ever indexed —
    // Claude Code's is whatever directory the session started in. Answering
    // nothing there is technically defensible and practically useless, so an
    // unindexed root widens the search to every checkout instead.
    let scope = session.store().has_checkout(&root)?.then_some(root);
    let rows = session
        .store()
        .symbols_named(scope.as_deref(), &params.query, 200)?;
    #[allow(deprecated)]
    let symbols = rows
        .into_iter()
        .filter_map(|row| {
            let len = last_segment(&row.name).len();
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
                // Not read to convert columns: this fires per keystroke in a
                // symbol picker, and a picker's jump lands on the line either way.
                location: unread_location(Path::new(&row.root), &row.path, row.line, row.col, len)?,
                container_name: row.nesting.first().cloned(),
            })
        })
        .collect();
    Ok(Some(symbols))
}

/// What a reader wants at a glance: the signature as written, what its doc
/// comment says, and where it lives. How the answer was reached stays out of
/// it — except when the answer is a guess, which is said in words, because
/// LSP has no confidence field and a confident-looking guess is the one thing
/// this engine exists not to give.
pub(crate) fn hover(session: &mut Session, params: HoverParams) -> anyhow::Result<Option<Hover>> {
    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;
    if let Some(required) = required_at(session, &uri, position) {
        return Ok(Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: required_hover(&required),
            }),
            range: Some(required.range),
        }));
    }
    if let Some(under) = variables::under(session, &uri, position) {
        let (value, range) = variables::hover(session, &under);
        return Ok(Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value,
            }),
            range: Some(range),
        }));
    }
    let Some((located, pos)) = target(session, &uri, position) else {
        super::miss::why(NO_CHECKOUT);
        return Ok(None);
    };
    let Some((facts, source)) = session
        .document(&located.absolute)
        .map(|document| (document.facts().clone(), document.text.clone()))
    else {
        super::miss::why(UNREADABLE);
        return Ok(None);
    };
    let Some(under) = position::at_facts(&facts, pos.line, pos.col) else {
        super::miss::why(NO_NAME);
        return Ok(None);
    };
    let card = match under {
        Under::Definition(def) => {
            let mut card =
                hover_definition(session, &located.root, &located.absolute, &def, &source)?;
            // A `def` in a string read once per value makes one method per
            // value, all written here (DEC-167).
            let made: Vec<&Def> = facts
                .defs
                .iter()
                .filter(|other| {
                    other.kind == Kind::Method
                        && other.pos == def.pos
                        && other.singleton == def.singleton
                        && other.nesting == def.nesting
                })
                .collect();
            if made.len() > 1 {
                let tree = session.tree(&located.root)?;
                let names: Vec<String> = made
                    .iter()
                    .map(|made| {
                        let owner = tree.scope_fqn(&made.nesting);
                        format!("`{}`", display_name(made, owner.as_deref()))
                    })
                    .collect();
                card.caveat = Some(format!(
                    "One of {} methods this line makes: {}",
                    made.len(),
                    names.join(", ")
                ));
            }
            card
        }
        Under::Constant(reference) => {
            hover_constant(session, &located.root, &located.relative, &reference)?
        }
        Under::Call(call) => hover_call(session, &located, &facts, &call)?,
    };
    let mut text = card.markdown();
    // Said where the answer is read: an unindexed checkout answers from core
    // and gems alone, and a residue there is a gap in the index, not a
    // finding about the code. After an upgrade dropped the store, not even
    // those are there until the background index refills it.
    let warming = session.warming(&located.root);
    let read = warming
        .as_ref()
        .map(|w| format!(" ({})", super::fresh::how_far(w)))
        .unwrap_or_default();
    if let Some((_, since)) = session
        .reindexing
        .as_ref()
        .filter(|(root, _)| *root == located.root)
    {
        text.push_str(&format!(
            "\n\n_trekr is reindexing this checkout after an upgrade (started {} s ago){read}. \
             Until it finishes, answers are partial — this checkout's code, its gems and Ruby \
             core may not be read yet._",
            since.elapsed().as_secs()
        ));
    } else if let Some(warming) = &warming {
        // The checkout's own files may be in and its gems not: an answer
        // that looks whole and may change (DEC-320).
        text.push_str(&match warming.interrupted {
            false => format!(
                "\n\n_trekr is still indexing this checkout{read}, so this answer may change._"
            ),
            true => format!(
                "\n\n_trekr's index of this checkout was cut short{read}, so answers are \
                 partial until it is indexed again._"
            ),
        });
    } else if !session.indexed(&located.root) {
        use super::fresh::Background;
        text.push_str(&format!(
            "\n\n_This checkout is not indexed yet, so answers come from core and gems alone. {}_",
            match session.background.of(&located.root) {
                Background::Indexing => "trekr is indexing it in the background.",
                Background::Waiting =>
                    "trekr indexes it once another trekr process writing the index is done; \
                     `trekr --index` waits for that now.",
                Background::Off => "`trekr --index` indexes it.",
            }
        ));
    }
    Ok(Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: text,
        }),
        range: None,
    }))
}

/// A hover's parts, in the order they are read.
#[derive(Default)]
pub(super) struct Card {
    pub(super) code: Option<String>,
    /// Why this answer may not be the one that runs, in words.
    pub(super) caveat: Option<String>,
    pub(super) doc: Option<Doc>,
    pub(super) location: Option<String>,
    /// A model's table, from the schema (DEC-481).
    pub(super) schema: Option<String>,
}

impl Card {
    pub(super) fn markdown(&self) -> String {
        let mut parts = Vec::new();
        if let Some(code) = &self.code {
            parts.push(format!("```ruby\n{code}\n```"));
        }
        if let Some(caveat) = &self.caveat {
            parts.push(format!("_{caveat}_"));
        }
        if let Some(doc) = self.doc.as_ref().map(Doc::markdown)
            && !doc.is_empty()
        {
            parts.push(doc);
        }
        if let Some(location) = &self.location {
            parts.push(location.clone());
        }
        if let Some(schema) = &self.schema {
            parts.push(schema.clone());
        }
        parts.join("\n\n")
    }
}

/// The cursor is on a definition: it is the answer, read from the buffer.
fn hover_definition(
    session: &mut Session,
    root: &Path,
    file: &Path,
    def: &Def,
    source: &str,
) -> anyhow::Result<Card> {
    let tree = session.tree(root)?;
    let qualified = match def.kind {
        // Only `def Foo.x` names its owner in `target`; a macro's is what
        // it aliases or delegates to.
        Kind::Method => def
            .target
            .as_ref()
            .filter(|_| def.via.is_none())
            .map(|t| tree.resolve(t, &def.nesting).fqn.unwrap_or(t.clone()))
            .or_else(|| tree.scope_fqn(&def.nesting)),
        Kind::Class | Kind::Module => {
            let own: Vec<String> = std::iter::once(def.name.clone())
                .chain(def.nesting.iter().cloned())
                .collect();
            tree.scope_fqn(&own)
        }
        Kind::Constant => Some(match tree.scope_fqn(&def.nesting) {
            Some(scope) if !scope.is_empty() => format!("{scope}::{}", def.name),
            _ => def.name.clone(),
        }),
    };
    let display = display_name(def, qualified.as_deref());
    // On a column in the schema itself: what the column is.
    let location = (def.via.as_deref() == Some("schema"))
        .then(|| {
            let file = file.to_string_lossy();
            super::schema::column_line(session, root, &file, def.pos.line, &def.name, false)
        })
        .flatten();
    let schema = match (def.kind, &qualified) {
        (Kind::Class, Some(fqn)) => super::schema::model_section(session, root, fqn),
        _ => None,
    };
    Ok(Card {
        code: Some(doc::signature(def, &display, source)),
        doc: doc::doc_above(source, def.pos.line),
        location,
        schema,
        ..Card::default()
    })
}

/// A method is shown as `Owner#name`; anything else by its own FQN.
fn display_name(def: &Def, qualified: Option<&str>) -> String {
    match def.kind {
        Kind::Method => doc::method_name(qualified, def.singleton, &def.name),
        _ => qualified.map_or_else(|| def.name.clone(), str::to_string),
    }
}

/// How many declaration sites of a reopened class are read looking for its
/// doc. `ActiveSupport` is reopened in hundreds of files, and a hover must
/// not read them all.
const DOC_SITES: usize = 5;

fn hover_constant(
    session: &mut Session,
    root: &Path,
    path: &str,
    reference: &crate::core::ConstRef,
) -> anyhow::Result<Card> {
    let tree = session.tree(root)?;
    let resolution = tree.resolve_at(&reference.name, &reference.nesting, path);
    let (Some(fqn), crate::tree::Status::Resolved) = (resolution.fqn.clone(), resolution.status)
    else {
        return Ok(Card {
            caveat: Some(format!(
                "`{}` is not defined anywhere trekr has indexed — it may be built at runtime, or come from a gem that is not installed.",
                reference.name
            )),
            ..Card::default()
        });
    };
    let kind = tree.kind_of(&fqn).unwrap_or("constant").to_string();
    let sites = resolution.sites;
    let mut read = Vec::new();
    for site in sites.iter().take(DOC_SITES) {
        if let Some(described) = describe(session, root, site, &fqn, Some(&fqn), None) {
            read.push((site, described));
        }
    }
    let primary = read
        .iter()
        .position(|(_, d)| d.doc.is_some())
        .or((!read.is_empty()).then_some(0));
    let (code, doc, location) = match primary.map(|i| read.swap_remove(i)) {
        Some((site, described)) => (
            described.signature,
            described.doc,
            Some(defined_in(session, root, &site.path, described.line, None)),
        ),
        None => (
            match kind.as_str() {
                "class" | "module" => format!("{kind} {fqn}"),
                _ => fqn.clone(),
            },
            None,
            sites
                .first()
                .map(|site| defined_in(session, root, &site.path, site.line, None)),
        ),
    };
    let location = location.map(|l| match sites.len() {
        0 | 1 => l,
        2 => format!("{l} and 1 other place"),
        n => format!("{l} and {} other places", n - 1),
    });
    let schema = match kind.as_str() {
        "class" => super::schema::model_section(session, root, &fqn),
        _ => None,
    };
    Ok(Card {
        code: Some(code),
        doc,
        location,
        schema,
        ..Card::default()
    })
}

fn hover_call(
    session: &mut Session,
    located: &Located,
    facts: &crate::core::Facts,
    call: &crate::core::Call,
) -> anyhow::Result<Card> {
    use crate::tree::Status;
    let root = &located.root;
    let tree = session.tree(root)?;
    let answer = crate::resolve::method_at(tree, facts, call, &located.relative);
    note_uncertain(&answer);
    let named = tree.named(&call.name);
    let name = &call.name;

    let site = match answer.status {
        Status::Resolved | Status::Ambiguous => answer.sites.first().cloned(),
        Status::Residue => None,
    };
    let Some(site) = site else {
        return Ok(Card {
            caveat: Some(residue_words(&answer, name, &named)),
            ..Card::default()
        });
    };
    let singleton = named
        .iter()
        .find(|m| m.site.path == site.path && m.site.line == site.line)
        .map(|m| m.singleton);
    let caveat = match answer.status {
        // Where the call really lands is the delegate; say so (DEC-211).
        _ if answer.resolved_via.as_deref() == Some("delegate") => {
            let reason = answer
                .reason
                .as_deref()
                .unwrap_or("sent on by a `delegate`");
            let mut words = format!("{}{}.", reason[..1].to_uppercase(), &reason[1..]);
            if answer.status == Status::Ambiguous {
                words.push_str(&format!(
                    " It may be a subclass that overrides `{name}`: {}.",
                    count(answer.candidates.len(), "other definition")
                ));
            }
            Some(words)
        }
        Status::Ambiguous => Some(match named.len().saturating_sub(1) {
            0 => "Best guess — the receiver's type is inferred, not declared.".to_string(),
            n => format!(
                "Best guess — the receiver's type is inferred, and {} of `{name}` {}.",
                count(n, "other definition"),
                if n == 1 { "exists" } else { "exist" }
            ),
        }),
        Status::Resolved if answer.confidence < 1.0 => Some(match answer.resolved_via.as_deref() {
            Some("includer") => format!(
                "Called inside a module: found through the classes that include it, and not all of them define `{name}`."
            ),
            _ => "The receiver's type is inferred from assignments that do not all agree."
                .to_string(),
        }),
        _ => None,
    };
    let owner = answer.owner.clone();
    let described = describe(session, root, &site, name, owner.as_deref(), singleton);
    let fallback = || doc::method_name(owner.as_deref(), singleton.unwrap_or(false), name);
    let line = described.as_ref().map_or(site.line, |d| d.line);
    let location = declared_location(
        session,
        root,
        &site.path,
        line,
        name,
        answer.defined_via.as_deref(),
    );
    let (code, doc) = match described {
        Some(d) => (d.signature, d.doc),
        None => (fallback(), None),
    };
    Ok(Card {
        code: Some(code),
        caveat,
        doc,
        location: Some(location),
        schema: None,
    })
}

/// A residue, in words: what is not known, and what it could still be.
fn residue_words(
    answer: &crate::resolve::MethodAnswer,
    name: &str,
    named: &[crate::tree::MethodDef],
) -> String {
    let lead = format!("**`{name}`** — ");
    match &answer.receiver_type {
        // The resolver's words: which side of the includers was asked is
        // the difference between true and false here (DEC-127).
        Some(_) if answer.receiver_kind.as_deref() == Some("module") => format!(
            "{lead}{}.",
            answer.reason.as_deref().unwrap_or("called inside a module")
        ),
        Some(known) => {
            let mut out = format!(
                "{lead}`{known}` has no `{name}` in anything trekr has indexed; it may come from a gem, a DSL, or `method_missing`."
            );
            if !answer.unresolved_ancestors.is_empty() {
                let missing: Vec<String> = answer
                    .unresolved_ancestors
                    .iter()
                    .take(2)
                    .map(|a| format!("`{a}`"))
                    .collect();
                out.push_str(&format!(
                    " Some of its ancestors are not indexed ({}).",
                    missing.join(", ")
                ));
            }
            out
        }
        None if named.is_empty() => {
            format!("{lead}receiver type unknown, and nothing trekr has indexed defines `{name}`.")
        }
        None => {
            const SHOWN: usize = 3;
            let mut shown: Vec<String> = answer
                .candidates
                .iter()
                .take(SHOWN)
                .map(|c| format!("`{}`", doc::method_name(Some(&c.owner), c.singleton, name)))
                .collect();
            if named.len() > shown.len() {
                shown.push(format!("{} more", named.len() - shown.len()));
            }
            format!(
                "{lead}receiver type unknown — {}: {}",
                count(named.len(), "possible definition"),
                shown.join(", ")
            )
        }
    }
}

fn count(n: usize, noun: &str) -> String {
    match n {
        1 => format!("1 {noun}"),
        n => format!("{n} {noun}s"),
    }
}

/// A definition read from its own file, as it is now.
pub(super) struct Described {
    pub(super) signature: String,
    pub(super) doc: Option<Doc>,
    /// Where the definition is today, which an edit since the index moved.
    pub(super) line: u32,
}

/// Read the definition an index site names, from its file as it is now: the
/// editor's buffer if open, else disk (cached while unchanged).
///
/// `qualified` is the owner's FQN for a method and the name's own FQN
/// otherwise; `singleton`, when known, picks between `module_function`'s two
/// copies. `None` when the file no longer says unmistakably which definition
/// the site meant — a doc attached to the wrong definition is worse than none.
pub(super) fn describe(
    session: &mut Session,
    root: &Path,
    site: &crate::tree::Site,
    name: &str,
    qualified: Option<&str>,
    singleton: Option<bool>,
) -> Option<Described> {
    let absolute = absolute_site(root, &site.path)?;
    let absolute = std::fs::canonicalize(&absolute).unwrap_or(absolute);
    let document = session.document(&absolute)?;
    let name = last_segment(name);
    let fits = |d: &&Def| {
        last_segment(&d.name) == name
            && d.kind.as_str() == site.kind
            && singleton.is_none_or(|s| d.singleton == s)
    };
    let def = {
        let facts = document.facts();
        match facts
            .defs
            .iter()
            .filter(fits)
            .find(|d| d.pos.line == site.line)
        {
            Some(def) => def.clone(),
            // Edited since it was indexed. One definition of the name in the
            // same scope is still unmistakably the one; two are a guess.
            None => {
                let mut moved = facts
                    .defs
                    .iter()
                    .filter(fits)
                    .filter(|d| same_scope(d, qualified));
                let only = moved.next()?.clone();
                if moved.next().is_some() {
                    return None;
                }
                only
            }
        }
    };
    let text = &document.text;
    Some(Described {
        signature: doc::signature(&def, &display_name(&def, qualified), text),
        doc: doc::doc_above(text, def.pos.line),
        line: def.pos.line,
    })
}

/// Was `def` written in the scope `qualified` names? By the last segment only,
/// since a written nesting is not resolved — enough to tell `Widget#save`
/// from `Gadget#save` in one file, which is all a moved definition needs.
fn same_scope(def: &Def, qualified: Option<&str>) -> bool {
    let scope = match def.kind {
        Kind::Method => qualified,
        _ => qualified
            .and_then(|q| q.rsplit_once("::"))
            .map(|(scope, _)| scope),
    };
    let written = match def.kind {
        Kind::Method => def
            .target
            .as_deref()
            .filter(|_| def.via.is_none())
            .or(def.nesting.first().map(String::as_str)),
        _ => def.nesting.first().map(String::as_str),
    };
    scope.map(last_segment) == written.map(last_segment)
}

/// Where a method is defined, in words: a schema column says what the column
/// is, which is what "declared by the schema" left a reader to go and look up.
pub(super) fn declared_location(
    session: &mut Session,
    root: &Path,
    path: &str,
    line: u32,
    name: &str,
    declared_via: Option<&str>,
) -> String {
    declared_via
        .filter(|via| *via == "schema")
        .and_then(|_| super::schema::column_line(session, root, path, line, name, true))
        .unwrap_or_else(|| defined_in(session, root, path, line, declared_via))
}

/// "Defined in `path:line`", linked. A gem says which, since its path alone
/// is a long way from telling you; a declaration says what declared it,
/// because the code that runs is somewhere else.
pub(super) fn defined_in(
    session: &Session,
    root: &Path,
    path: &str,
    line: u32,
    declared_via: Option<&str>,
) -> String {
    let verb = match declared_via {
        None => "Defined in".to_string(),
        Some("rbi") => "Declared by a Sorbet stub in".to_string(),
        Some(via) => format!("Declared by `{via}` in"),
    };
    if crate::tree::is_rspec_stub(path) {
        return "Made by RSpec when a suite boots, as trekr's RSpec stub states".to_string();
    }
    if crate::tree::is_stdlib_stub(path) {
        return "Compiled into Ruby's stdlib, as its RBS signature states".to_string();
    }
    if crate::tree::is_core(path) {
        return format!("{verb} Ruby core");
    }
    let Some(absolute) = absolute_site(root, path) else {
        return format!("{verb} `{path}`");
    };
    let uri = format!("{}#L{line}", path_to_uri(&absolute));
    let gem = Path::new(path)
        .is_absolute()
        // Any other checkout an answer points into is just a path.
        .then(|| session.store().gem_containing(path).ok().flatten())
        .flatten();
    match gem {
        Some(gem) => {
            let within = path.get(gem.len() + 1..).unwrap_or(path);
            let label = Path::new(&gem)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            format!("{verb} [`{within}:{line}`]({uri}) · gem `{label}`")
        }
        None => {
            let root = root.to_string_lossy();
            let shown = match Path::new(path).is_absolute() {
                true if crate::core::paths::under(&root, path) => {
                    path[root.len()..].trim_start_matches('/').to_string()
                }
                true => crate::core::paths::pretty(path),
                false => path.to_string(),
            };
            format!("{verb} [`{shown}:{line}`]({uri})")
        }
    }
}

/// Descendants of the class or module at the cursor.
pub(crate) fn implementation(
    session: &mut Session,
    params: lsp_types::request::GotoImplementationParams,
) -> anyhow::Result<Option<GotoDefinitionResponse>> {
    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;
    let Some((located, pos)) = target(session, &uri, position) else {
        return Ok(None);
    };
    let facts = session
        .document(&located.absolute)
        .map(|document| document.facts().clone());
    let Some(facts) = facts else { return Ok(None) };
    let Some(under) = position::at_facts(&facts, pos.line, pos.col) else {
        return Ok(None);
    };
    // Two different questions share this operation, and only one of them was
    // answered. On a class or module, "implementations" means the types that
    // mix it in. On a **method**, it means the overrides — which is what an
    // abstract method's definition is standing on, and it returned nothing.
    let tree = session.tree(&located.root)?;
    let sites: Vec<(String, u32, u32)> = match under {
        Under::Definition(def) if def.kind == crate::core::Kind::Method => {
            let owner = tree.scope_fqn(&def.nesting);
            let Some(owner) = owner else { return Ok(None) };
            overrides_of(tree, &owner, &def.name, def.singleton, def.pos.line)
        }
        Under::Definition(def) => implementers_of(tree, &def.name),
        Under::Constant(reference) => implementers_of(tree, &reference.name),
        Under::Call(_) => return Ok(None),
    };
    let locations: Vec<Location> = sites
        .into_iter()
        .filter_map(|(p, line, col)| location(&located.root, &p, line, col, 0, None))
        .collect();
    Ok((!locations.is_empty()).then_some(GotoDefinitionResponse::Array(locations)))
}

/// Every class that mixes in this module or inherits this class.
fn implementers_of(tree: &crate::tree::Tree, name: &str) -> Vec<(String, u32, u32)> {
    let Some(fqn) = tree.resolve(name, &[]).fqn else {
        return Vec::new();
    };
    tree.includers_of(&fqn)
        .iter()
        .flat_map(|descendant| tree.sites(descendant).to_vec())
        .map(|site| (site.path, site.line, site.col))
        .collect()
}

/// The innermost method definition containing a line.
///
/// Innermost, because nested classes and blocks mean the outermost match is
/// usually the file's class, which is not what the reader asked about.
fn enclosing_def(facts: &crate::core::Facts, line: u32) -> Option<&crate::core::Def> {
    facts
        .defs
        .iter()
        .filter(|def| {
            def.kind == crate::core::Kind::Method && def.pos.line <= line && line <= def.end_line
        })
        .min_by_key(|def| def.end_line - def.pos.line)
}

/// `Owner#method`, or `Owner.method` for a singleton — how a reader names it.
fn label(def: &crate::core::Def) -> String {
    let marker = if def.singleton { "." } else { "#" };
    match def.nesting.first() {
        Some(owner) => format!("{owner}{marker}{}", def.name),
        None => def.name.clone(),
    }
}

/// Every definition that wins over this one somewhere below it.
///
/// Not "same name in a subclass": Rails' concrete adapters put `write_query?`
/// in a *sibling module* — `SQLite3::DatabaseStatements` beside the abstract
/// `ConnectionAdapters::DatabaseStatements` — and mix each into a class in the
/// same hierarchy. The owners are unrelated; the classes are not.
///
/// So the question is asked the way Ruby answers it: for every type carrying
/// the abstract owner, look the method up and see whose definition actually
/// wins. Anything but this one is an override.
fn overrides_of(
    tree: &crate::tree::Tree,
    owner: &str,
    name: &str,
    singleton: bool,
    line: u32,
) -> Vec<(String, u32, u32)> {
    let mut sites: Vec<(String, u32, u32)> = tree
        .includers_of(owner)
        .iter()
        .filter_map(|class| tree.lookup(class, singleton, name))
        .filter(|found| found.owner != owner && found.site.line != line)
        .map(|found| (found.site.path.clone(), found.site.line, found.site.col))
        .collect();
    sites.sort();
    sites.dedup();
    sites
}

pub(crate) fn prepare_call_hierarchy(
    session: &mut Session,
    params: CallHierarchyPrepareParams,
) -> anyhow::Result<Option<Vec<CallHierarchyItem>>> {
    let uri = params
        .text_document_position_params
        .text_document
        .uri
        .clone();
    let position = params.text_document_position_params.position;
    let Some(path) = file_of(&uri) else {
        return Ok(None);
    };
    let Some((text, facts)) = session
        .document(&path)
        .map(|document| (document.text.clone(), document.facts().clone()))
    else {
        return Ok(None);
    };
    let pos = to_pos(&text, position);
    let Some(under) = position::at_facts(&facts, pos.line, pos.col) else {
        return Ok(None);
    };
    let item = match under {
        Under::Definition(def) if def.kind == crate::core::Kind::Method => {
            def_item(uri, &text, &def)
        }
        // On a call, the item is the method it calls — that is what the
        // hierarchy is *of*. Expanding the call site itself would find no
        // definition there and answer nothing.
        Under::Call(call) => match callee_item(session, &path, &facts, &call) {
            Some(item) => item,
            None => call_item(uri, &text, &call),
        },
        _ => return Ok(None),
    };
    Ok(Some(vec![item]))
}

/// A call-hierarchy item for a method definition: named the way a reader
/// names it (`Owner#method`), spanning the whole body, selecting the name.
#[allow(deprecated)]
fn def_item(uri: Url, text: &str, def: &crate::core::Def) -> CallHierarchyItem {
    CallHierarchyItem {
        name: label(def),
        kind: SymbolKind::METHOD,
        tags: None,
        detail: def.via.clone(),
        uri,
        range: convert::block(Some(text), def.pos.line, def.end_line),
        selection_range: convert::span(Some(text), def.pos.line, def.pos.col, def.name.len()),
        data: None,
    }
}

/// An item for a call whose target could not be found: the call itself.
#[allow(deprecated)]
fn call_item(uri: Url, text: &str, call: &crate::core::Call) -> CallHierarchyItem {
    let range = convert::span(Some(text), call.pos.line, call.pos.col, call.written_len());
    CallHierarchyItem {
        name: call.name.clone(),
        kind: SymbolKind::METHOD,
        tags: None,
        detail: Some(format!("receiver: {}", call.recv.as_str())),
        uri,
        range,
        selection_range: range,
        data: None,
    }
}

/// The definition a call resolves to, as an item — or `None` when the call
/// does not resolve, or its file is not in an indexed checkout.
fn callee_item(
    session: &mut Session,
    path: &Path,
    facts: &crate::core::Facts,
    call: &crate::core::Call,
) -> Option<CallHierarchyItem> {
    let located = session.locate_query(path)?;
    let tree = session.tree(&located.root).ok()?;
    let answer = crate::resolve::method_at(tree, facts, call, &located.relative);
    let site = answer.sites.first()?;
    let absolute = absolute_site(&located.root, &site.path)?;
    let text = std::fs::read_to_string(&absolute).ok()?;
    let uri: Url = path_to_uri(&absolute).parse().ok()?;
    let target = crate::extract::extract(text.as_bytes());
    Some(
        match target
            .defs
            .iter()
            .find(|def| def.pos.line == site.line && def.name == call.name)
        {
            Some(def) => def_item(uri, &text, def),
            // A macro-made method (`attr_reader`, `delegate`) has a site but no
            // body: point at the line that made it.
            None => {
                let range = convert::span(Some(&text), site.line, site.col, call.name.len());
                #[allow(deprecated)]
                CallHierarchyItem {
                    name: answer
                        .owner
                        .map(|owner| format!("{owner}#{}", call.name))
                        .unwrap_or_else(|| call.name.clone()),
                    kind: SymbolKind::METHOD,
                    tags: None,
                    detail: answer.defined_via,
                    uri,
                    range,
                    selection_range: range,
                    data: None,
                }
            }
        },
    )
}

/// The method an item names. Items carry the reader's label — `Job#run`,
/// `Job.sweep` — and the lookup wants the bare name after the marker.
fn item_method(name: &str) -> &str {
    name.rsplit_once('#')
        .or_else(|| name.rsplit_once('.'))
        .map_or(name, |(_, method)| method)
}

/// Incoming calls are the confirmed tier of a references query — the whole
/// point of having tiers.
///
/// Each caller is an item for the *method the call sits in*, with every call
/// from it as a range — so the client can expand it again and walk up the
/// tree, which it could not when the item was the call site itself.
pub(crate) fn incoming_calls(
    session: &mut Session,
    params: CallHierarchyIncomingCallsParams,
    cancel: &dyn Fn() -> bool,
) -> anyhow::Result<Option<Vec<CallHierarchyIncomingCall>>> {
    let name = item_method(&params.item.name).to_string();
    let Some(path) = convert::uri_to_path(params.item.uri.as_str()) else {
        return Ok(None);
    };
    // The item names a file, and that file's checkout is the one to search —
    // the client's workspace is not necessarily either.
    let Some(located) = session.locate_query(&path) else {
        return Ok(None);
    };
    let root = located.root.clone();
    let root_str = root.to_string_lossy().into_owned();

    // Which method the item names, not just which name. Asking with no owner
    // is the bare-name question `--refs` exists to beat, and it cannot reach
    // the `confirmed` tier this operation reports — so it answered nothing.
    let line = params.item.selection_range.start.line + 1;
    let facts = session
        .document(&located.absolute)
        .map(|document| document.facts().clone());
    let owner_def = facts.as_ref().and_then(|facts| {
        facts
            .defs
            .iter()
            .find(|def| {
                def.kind == crate::core::Kind::Method && def.pos.line == line && def.name == name
            })
            .cloned()
    });
    let owner = owner_def
        .as_ref()
        .and_then(|def| session.tree(&root).ok()?.scope_fqn(&def.nesting));
    let query = refs::Query {
        owner,
        singleton: owner_def.as_ref().is_some_and(|def| def.singleton),
        name: name.clone(),
    };
    let target = query.owner.clone();
    let paths = session
        .store()
        .files_calling_any(&root_str, &refs::called_as(&query))?;
    let overlay = overlay(session, &root);
    let tree = session.tree(&root)?;

    // Keyed by (file, the caller's def line), in first-seen order.
    let mut callers: Vec<CallHierarchyIncomingCall> = Vec::new();
    let mut index: HashMap<(String, u32), usize> = HashMap::new();
    let tier = |facts: &crate::core::Facts, call: &crate::core::Call, path: &str| {
        refs::tier_call(tree, facts, call, path, &query, target.as_deref())
    };
    scan_files(
        &overlay,
        &root,
        Source::Listed(paths.into_iter()),
        &query,
        cancel,
        &tier,
        |files| {
            for file in files {
                let Some(uri) = file_uri(&root, &file.path) else {
                    continue;
                };
                let lines = LineIndex::new(&file.text);
                for (at, reference) in &file.tiered {
                    let call = &file.facts.calls[*at];
                    if reference.tier != refs::Tier::Confirmed {
                        continue;
                    }
                    let at = Location {
                        uri: uri.clone(),
                        range: lines.span(call.pos.line, call.pos.col, call.written_len()),
                    };
                    let caller = enclosing_def(&file.facts, call.pos.line);
                    let key = (file.path.clone(), caller.map_or(0, |def| def.pos.line));
                    if let Some(&i) = index.get(&key) {
                        callers[i].from_ranges.push(at.range);
                        continue;
                    }
                    #[allow(deprecated)]
                    let from = match caller {
                        Some(def) => def_item(at.uri.clone(), &file.text, def),
                        // A call at the top level of a file: there is no method to
                        // walk up to, so the item is the call itself.
                        None => CallHierarchyItem {
                            name: file.path.clone(),
                            kind: SymbolKind::FILE,
                            tags: None,
                            detail: Some(reference.why.to_string()),
                            uri: at.uri.clone(),
                            range: at.range,
                            selection_range: at.range,
                            data: None,
                        },
                    };
                    index.insert(key, callers.len());
                    callers.push(CallHierarchyIncomingCall {
                        from,
                        from_ranges: vec![at.range],
                    });
                }
            }
            Ok(ControlFlow::Continue(()))
        },
    )?;
    Ok(Some(callers))
}

/// Outgoing calls are the call-site facts inside the method's own body.
pub(crate) fn outgoing_calls(
    session: &mut Session,
    params: CallHierarchyOutgoingCallsParams,
) -> anyhow::Result<Option<Vec<CallHierarchyOutgoingCall>>> {
    let uri = params.item.uri.clone();
    let Some(path) = file_of(&uri) else {
        return Ok(None);
    };
    let Some((text, facts)) = session
        .document(&path)
        .map(|document| (document.text.clone(), document.facts().clone()))
    else {
        return Ok(None);
    };

    // The method whose body we are listing: the innermost def containing the
    // item's name.
    let line = params.item.selection_range.start.line + 1;
    let Some(enclosing) = facts
        .defs
        .iter()
        .filter(|def| def.pos.line <= line && line <= def.end_line)
        .min_by_key(|def| def.end_line - def.pos.line)
    else {
        return Ok(None);
    };
    let (start, end) = (enclosing.pos.line, enclosing.end_line);

    // Each callee once, with every call to it as a range. A callee that
    // resolves is an item at its definition, so the client can keep walking
    // down; one that does not is the call itself.
    let mut calls: Vec<CallHierarchyOutgoingCall> = Vec::new();
    let mut index: HashMap<(String, u32, String), usize> = HashMap::new();
    for call in facts
        .calls
        .iter()
        .filter(|call| call.pos.line >= start && call.pos.line <= end)
        .filter(|call| call.recv != crate::core::RecvShape::Symbol)
    {
        let at = convert::span(Some(&text), call.pos.line, call.pos.col, call.written_len());
        let to = callee_item(session, &path, &facts, call)
            .unwrap_or_else(|| call_item(uri.clone(), &text, call));
        let key = (
            to.uri.to_string(),
            to.selection_range.start.line,
            to.name.clone(),
        );
        match index.get(&key) {
            Some(&i) => calls[i].from_ranges.push(at),
            None => {
                index.insert(key, calls.len());
                calls.push(CallHierarchyOutgoingCall {
                    to,
                    from_ranges: vec![at],
                });
            }
        }
    }
    Ok(Some(calls))
}

/// Syntax diagnostics, free from a parse we already did.
///
/// Syntax only. Everything else this engine knows is a *ranked* answer with a
/// confidence, and publishing those as diagnostics would turn disclosure into
/// noise in the editor's gutter.
pub(crate) fn diagnostics(
    session: &mut Session,
    path: &Path,
    uri: Url,
) -> Option<lsp_server::Message> {
    let document = session.document(path)?;
    let version = document.version();
    let errors = document.parse_errors();
    let text = document.text.as_str();
    let diagnostics: Vec<Diagnostic> = errors
        .into_iter()
        .map(|(line, col, message)| Diagnostic {
            range: point(Some(text), line, col),
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("trekr".into()),
            message,
            ..Default::default()
        })
        .collect();
    Some(publish(uri, diagnostics, version))
}

/// A `publishDiagnostics` notification. An empty list is meaningful: it clears
/// what was published before.
pub(crate) fn publish(
    uri: Url,
    diagnostics: Vec<Diagnostic>,
    version: Option<i32>,
) -> lsp_server::Message {
    let params = lsp_types::PublishDiagnosticsParams {
        uri,
        diagnostics,
        version,
    };
    lsp_server::Message::Notification(lsp_server::Notification {
        method: lsp_types::notification::PublishDiagnostics::METHOD.to_string(),
        params: serde_json::to_value(params).unwrap_or_default(),
    })
}

use lsp_types::notification::Notification as _;
