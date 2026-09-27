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
use super::require::{self, Found, Origin};
use super::state::{Located, Session};
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
use std::collections::HashMap;
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
fn absolute_site(root: &Path, path: &str) -> Option<std::path::PathBuf> {
    if path == crate::tree::CORE_PATH {
        // Core is compiled into the binary; it is written out beside the
        // database so that `require` and `Array#each` land on a readable
        // signature instead of answering nothing.
        crate::store::core_stub_path().ok()
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
    let located = session.locate(&path)?;
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
        return Ok(required_definition(session.definition_links, required));
    }
    let Some((located, pos)) = target(session, &uri, position) else {
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
    let root = session.locate(&file).map(|located| located.root);
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
    let root = session.locate(&file).map(|located| located.root);
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
        return Ok(Vec::new());
    };
    let Some(under) = position::at_facts(&facts, pos.line, pos.col) else {
        return Ok(Vec::new());
    };
    let path = located.relative.clone();
    let tree = session.tree(&located.root)?;
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
    session: &mut Session,
    params: ReferenceParams,
    cancel: &dyn Fn() -> bool,
) -> anyhow::Result<Option<Vec<Location>>> {
    let uri = params.text_document_position.text_document.uri;
    let position = params.text_document_position.position;
    let declarations = params.context.include_declaration;
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
        return constant_references(session, &located, &name, &nesting, declarations).map(Some);
    }

    let root = located.root.clone();
    let root_str = root.to_string_lossy().into_owned();
    let path = located.relative.clone();
    let (name, own_site) = match &under {
        Under::Definition(def) => (def.name.clone(), Some(def.pos)),
        Under::Call(call) => (call.name.clone(), None),
        Under::Constant(_) => unreachable!("answered above"),
    };
    let paths = session.store().files_calling(&root_str, &name)?;
    let overlay = overlay(session, &root);
    let tree = session.tree(&root)?;

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
            (
                refs::Query {
                    owner: answer.owner,
                    singleton: call.singleton,
                    name: name.clone(),
                },
                answer.sites,
            )
        }
        Under::Constant(_) => unreachable!("answered above"),
    };
    let target = query.owner.clone();

    let mut found: Vec<(refs::Reference, Location)> = Vec::new();
    scan_files(&overlay, &root, paths, &name, cancel, |file| {
        let Some(uri) = file_uri(&root, &file.path) else {
            return;
        };
        let lines = LineIndex::new(&file.text);
        for call in file.facts.calls.iter().filter(|c| c.name == query.name) {
            let reference = refs::tier_call(
                tree,
                &file.facts,
                call,
                &file.path,
                &query,
                target.as_deref(),
            );
            if reference.tier == refs::Tier::Excluded {
                continue;
            }
            let range = lines.span(reference.line, reference.col, name.len());
            found.push((
                reference,
                Location {
                    uri: uri.clone(),
                    range,
                },
            ));
        }
    })?;
    // Confirmed before possible: LSP has no tier field, so the order of the
    // list is the disclosure.
    found.sort_by_key(|(reference, _)| refs::order(reference));

    let mut locations: Vec<Location> = Vec::new();
    if declarations {
        match own_site {
            Some(pos) => locations.extend(location(
                &root,
                &path,
                pos.line,
                pos.col,
                name.len(),
                overlay.get(&path).map(String::as_str),
            )),
            None => locations.extend(defined_at.iter().filter_map(|site| {
                location(&root, &site.path, site.line, site.col, name.len(), None)
            })),
        }
    }
    locations.extend(found.into_iter().map(|(_, at)| at));
    Ok(Some(locations))
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
) -> anyhow::Result<Vec<Location>> {
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
        let facts = crate::extract::extract(text.as_bytes());
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

/// One file read for a question that spans the checkout.
struct Scanned {
    path: String,
    text: String,
    facts: crate::core::Facts,
}

/// How many files to parse between cancellation checks. Large enough that the
/// parallel parse has something to chew on, small enough that a withdrawn
/// request stops within a few tens of milliseconds.
const SCAN_CHUNK: usize = 128;

/// Read and parse the files a question needs — the editor's copy where it has
/// one — in parallel, handing each to `visit` in order.
///
/// The parse is the expensive part and it is a pure function of the bytes, so
/// it fans out; `visit` runs on this thread, because the tree it consults is
/// not shareable across threads. An open buffer that mentions `needle` is
/// scanned even when the index does not list its file: the index is as of the
/// last save, and the buffer is what the user is looking at.
fn scan_files(
    overlay: &HashMap<String, String>,
    root: &Path,
    mut candidates: Vec<String>,
    needle: &str,
    cancel: &dyn Fn() -> bool,
    mut visit: impl FnMut(Scanned),
) -> anyhow::Result<()> {
    use rayon::prelude::*;
    let listed: std::collections::HashSet<String> = candidates.iter().cloned().collect();
    candidates.extend(
        overlay
            .iter()
            .filter(|(path, text)| !listed.contains(*path) && text.contains(needle))
            .map(|(path, _)| path.clone()),
    );
    for chunk in candidates.chunks(SCAN_CHUNK) {
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
                let facts = crate::extract::extract(text.as_bytes());
                Some(Scanned {
                    path: path.clone(),
                    text,
                    facts,
                })
            })
            .collect();
        scanned.into_iter().for_each(&mut visit);
    }
    Ok(())
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
    let Some((located, pos)) = target(session, &uri, position) else {
        return Ok(None);
    };
    let Some((facts, source)) = session
        .document(&located.absolute)
        .map(|document| (document.facts().clone(), document.text.clone()))
    else {
        return Ok(None);
    };
    let Some(under) = position::at_facts(&facts, pos.line, pos.col) else {
        return Ok(None);
    };
    let card = match under {
        Under::Definition(def) => hover_definition(session, &located.root, &def, &source)?,
        Under::Constant(reference) => hover_constant(session, &located.root, &reference)?,
        Under::Call(call) => hover_call(session, &located, &facts, &call)?,
    };
    let mut text = card.markdown();
    // Said where the answer is read: an unindexed checkout answers from core
    // and gems alone, and a residue there is a gap in the index, not a
    // finding about the code.
    if !session.indexed(&located.root) {
        text.push_str(
            "\n\n_This checkout is not indexed yet, so answers come from core and gems alone. trekr indexes it in the background; `trekr --index` does it now._",
        );
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
        parts.join("\n\n")
    }
}

/// The cursor is on a definition: it is the answer, read from the buffer.
fn hover_definition(
    session: &mut Session,
    root: &Path,
    def: &Def,
    source: &str,
) -> anyhow::Result<Card> {
    let tree = session.tree(root)?;
    let qualified = match def.kind {
        Kind::Method => def
            .target
            .as_ref()
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
    Ok(Card {
        code: Some(doc::signature(def, &display, source)),
        doc: doc::doc_above(source, def.pos.line),
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
    reference: &crate::core::ConstRef,
) -> anyhow::Result<Card> {
    let tree = session.tree(root)?;
    let resolution = tree.resolve(&reference.name, &reference.nesting);
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
    Ok(Card {
        code: Some(code),
        doc,
        location,
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
    let fallback = || {
        format!(
            "def {}",
            doc::method_name(owner.as_deref(), singleton.unwrap_or(false), name)
        )
    };
    let line = described.as_ref().map_or(site.line, |d| d.line);
    let location = defined_in(
        session,
        root,
        &site.path,
        line,
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
        Some(module) if answer.receiver_kind.as_deref() == Some("module") => format!(
            "{lead}called inside module `{module}`, and no class that includes it defines `{name}`."
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
            .or(def.nesting.first().map(String::as_str)),
        _ => def.nesting.first().map(String::as_str),
    };
    scope.map(last_segment) == written.map(last_segment)
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
    if path == crate::tree::CORE_PATH {
        return format!("{verb} Ruby core");
    }
    let Some(absolute) = absolute_site(root, path) else {
        return format!("{verb} `{path}`");
    };
    let uri = format!("{}#L{line}", path_to_uri(&absolute));
    let gem = Path::new(path)
        .is_absolute()
        .then(|| session.store().checkout_containing(path).ok().flatten())
        .flatten()
        // A gem is unpacked into a `gems/` directory under its `name-version`;
        // any other checkout an answer points into is just a path.
        .filter(|gem| {
            Path::new(gem)
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|dir| dir == "gems")
        });
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
    let range = convert::span(Some(text), call.pos.line, call.pos.col, call.name.len());
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
    let located = session.locate(path)?;
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
    let Some(located) = session.locate(&path) else {
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
    let paths = session.store().files_calling(&root_str, &name)?;
    let overlay = overlay(session, &root);
    let tree = session.tree(&root)?;
    let query = refs::Query {
        owner: owner_def
            .as_ref()
            .and_then(|def| tree.scope_fqn(&def.nesting)),
        singleton: owner_def.as_ref().is_some_and(|def| def.singleton),
        name: name.clone(),
    };
    let target = query.owner.clone();

    // Keyed by (file, the caller's def line), in first-seen order.
    let mut callers: Vec<CallHierarchyIncomingCall> = Vec::new();
    let mut index: HashMap<(String, u32), usize> = HashMap::new();
    scan_files(&overlay, &root, paths, &name, cancel, |file| {
        let Some(uri) = file_uri(&root, &file.path) else {
            return;
        };
        let lines = LineIndex::new(&file.text);
        for call in file.facts.calls.iter().filter(|c| c.name == name) {
            let reference = refs::tier_call(
                tree,
                &file.facts,
                call,
                &file.path,
                &query,
                target.as_deref(),
            );
            if reference.tier != refs::Tier::Confirmed {
                continue;
            }
            let at = Location {
                uri: uri.clone(),
                range: lines.span(call.pos.line, call.pos.col, name.len()),
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
    })?;
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
        let at = convert::span(Some(&text), call.pos.line, call.pos.col, call.name.len());
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
