//! A variable under the cursor, answered: where a local got its value, where
//! an instance variable is set, every mention of either, and a one-line hover.
//!
//! Locals never leave the file and come whole from [`vars`]. An instance or
//! class variable belongs to an object, so its writes are looked for in every
//! file that opens its class — and the class's ancestors, as far as the tree
//! resolves them — each parsed when asked (DEC-064). When the class cannot be
//! named, the file at hand is all that is searched; nothing is guessed.

use super::convert::{self, LineIndex, path_to_uri};
use super::handlers::absolute_site;
use super::state::Session;
use crate::resolve::vars::{self, Binding, Occurrence, Sigil, Vars};
use lsp_types::Uri as Url;
use lsp_types::{DocumentHighlight, DocumentHighlightKind, Location, Position};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// How many files are read for one ivar. A class reopened across more than
/// this — a core extension, a god object — is read in the tree's site order
/// and cut there; a definition among the rest is not found.
const MAX_FILES: usize = 64;

/// A variable under the cursor.
pub(super) struct Under {
    file: PathBuf,
    text: String,
    vars: Rc<Vars>,
    occurrence: Occurrence,
}

impl Under {
    fn range(&self) -> lsp_types::Range {
        LineIndex::new(&self.text).range(self.occurrence.span.clone())
    }

    fn uri(&self) -> Option<Url> {
        path_to_uri(&self.file).parse().ok()
    }
}

pub(super) fn under(session: &mut Session, uri: &Url, position: Position) -> Option<Under> {
    let path = convert::uri_to_path(uri.as_str())?;
    let file = std::fs::canonicalize(&path).unwrap_or(path);
    let document = session.document(&file)?;
    let offset = convert::offset_of(&document.text, position);
    let vars = document.vars();
    let occurrence = vars.at(offset)?.clone();
    // `--usage` tells a variable's answer from a method's.
    crate::usage::flag("variable");
    Some(Under {
        file,
        text: document.text.clone(),
        vars,
        occurrence,
    })
}

/// A mention found in some file, with that file's text to place it.
struct Found {
    file: PathBuf,
    range: lsp_types::Range,
    occurrence: Occurrence,
}

impl Found {
    fn location(&self) -> Option<Location> {
        Some(Location {
            uri: path_to_uri(&self.file).parse().ok()?,
            range: self.range,
        })
    }
}

/// Where the value comes from: a local's reaching writes, or every place an
/// instance variable is set — `initialize` first. Empty when none is found.
pub(super) fn definition(session: &mut Session, under: &Under) -> Vec<Location> {
    if under.occurrence.sigil == Sigil::Local {
        let Some(uri) = under.uri() else {
            return Vec::new();
        };
        let lines = LineIndex::new(&under.text);
        return under
            .vars
            .local_definitions(&under.occurrence)
            .iter()
            .map(|o| Location {
                uri: uri.clone(),
                range: lines.range(o.span.clone()),
            })
            .collect();
    }
    writes(session, under)
        .iter()
        .filter_map(Found::location)
        .collect()
}

/// Every read and write of the variable: a local's in its scope, a member's
/// across its class's files.
pub(super) fn references(session: &mut Session, under: &Under) -> Vec<Location> {
    let found = match under.occurrence.sigil {
        Sigil::Local => here(under),
        Sigil::Instance | Sigil::Class => members(session, under),
    };
    found.iter().filter_map(Found::location).collect()
}

/// Every mention in this file, reads and writes told apart. An ivar is
/// matched by the class it is written in, which needs no index.
pub(super) fn highlight(under: &Under) -> Vec<DocumentHighlight> {
    here(under)
        .into_iter()
        .map(|found| DocumentHighlight {
            range: found.range,
            kind: Some(match found.occurrence.write {
                Some(_) => DocumentHighlightKind::WRITE,
                None => DocumentHighlightKind::READ,
            }),
        })
        .collect()
}

/// One line: what it is and where it was set.
pub(super) fn hover(session: &mut Session, under: &Under) -> (String, lsp_types::Range) {
    let occurrence = &under.occurrence;
    let what = match occurrence.sigil {
        Sigil::Local => "local",
        Sigil::Instance => "ivar",
        Sigil::Class => "class variable",
    };
    let head = format!("{what} `{}`", occurrence.name);
    let text = match occurrence.sigil {
        Sigil::Local => {
            let lines = LineIndex::new(&under.text);
            let defs = under.vars.local_definitions(occurrence);
            match defs.first() {
                None => head,
                Some(first) => format!(
                    "{head} · {} at line {}{}",
                    first.write.map_or("assigned", Binding::describe),
                    lines.at(first.span.start).line + 1,
                    more(defs.len())
                ),
            }
        }
        Sigil::Instance | Sigil::Class => {
            let found = writes(session, under);
            match found.first() {
                None => format!("{head} · no assignment found"),
                Some(first) => {
                    let o = &first.occurrence;
                    let place = match (o.write, &o.method) {
                        (Some(Binding::Attr), _) => "set by an attribute writer".to_string(),
                        (_, Some(method)) => format!("set in `{method}`"),
                        (_, None) => "set in the class body".to_string(),
                    };
                    format!("{head} · {place}{}", more(found.len()))
                }
            }
        }
    };
    (text, under.range())
}

fn more(count: usize) -> String {
    match count {
        0 | 1 => String::new(),
        n => format!(" (and {} more)", n - 1),
    }
}

/// The variable's mentions in the file at hand.
fn here(under: &Under) -> Vec<Found> {
    let lines = LineIndex::new(&under.text);
    under
        .vars
        .same(&under.occurrence)
        .into_iter()
        .map(|o| Found {
            file: under.file.clone(),
            range: lines.range(o.span.clone()),
            occurrence: o.clone(),
        })
        .collect()
}

/// A member's writes, `initialize` first, then by file and line.
fn writes(session: &mut Session, under: &Under) -> Vec<Found> {
    let mut found: Vec<Found> = members(session, under)
        .into_iter()
        .filter(|f| f.occurrence.is_write())
        .collect();
    if found.is_empty() && under.occurrence.sigil == Sigil::Instance {
        found = template_writes(session, under);
    }
    found.sort_by_key(|f| {
        (
            f.occurrence.method.as_deref() != Some("initialize"),
            f.file.clone(),
            f.range.start.line,
            f.range.start.character,
        )
    });
    found
}

/// A template's `@ivar` with no write of its own: the writes of the
/// controllers that render it (DEC-522).
fn template_writes(session: &mut Session, under: &Under) -> Vec<Found> {
    let path = under.file.to_string_lossy();
    if crate::tree::views::ViewTemplate::of(&path).is_none() {
        return Vec::new();
    }
    let Some(located) = session.locate_query(&under.file) else {
        return Vec::new();
    };
    let sites = match session.tree(&located.root) {
        Ok(tree) => crate::resolve::views::template_ivar_writes(
            tree,
            &located.relative,
            &under.occurrence.name,
        ),
        Err(_) => return Vec::new(),
    };
    let mut found = Vec::new();
    for site in sites {
        let Some(file) = absolute_site(&located.root, &site.path) else {
            continue;
        };
        let Some(document) = session.document(&file) else {
            continue;
        };
        let vars = document.vars();
        let lines = LineIndex::new(&document.text);
        let write = vars.occurrences.iter().find(|o| {
            o.is_write()
                && o.name == under.occurrence.name
                && lines.at(o.span.start).line + 1 == site.line
        });
        if let Some(o) = write {
            found.push(Found {
                file: file.clone(),
                range: lines.range(o.span.clone()),
                occurrence: o.clone(),
            });
        }
    }
    found
}

/// Every mention of an instance or class variable in the files of the class
/// it belongs to and of that class's ancestors.
///
/// An instance ivar can be set by anything in the object's ancestry; a
/// class-level one (`@x` in a class body or `def self.x`) belongs to that one
/// class object; a class variable is shared down the ancestry like an ivar.
fn members(session: &mut Session, under: &Under) -> Vec<Found> {
    let Some(owner) = under.vars.owner(&under.occurrence).cloned() else {
        return Vec::new();
    };
    let Some(scope) = class_files(session, &under.file, &owner, under.occurrence.sigil) else {
        return here(under);
    };
    let want = &under.occurrence;
    let mut found = Vec::new();
    for file in scope.files {
        let Some(document) = session.document(&file) else {
            continue;
        };
        let text = document.text.clone();
        let vars = document.vars();
        let lines = LineIndex::new(&text);
        for o in &vars.occurrences {
            if o.sigil != want.sigil || o.name != want.name {
                continue;
            }
            let inside = vars.owner(o).is_some_and(|theirs| {
                theirs.singleton == owner.singleton
                    && scope.inside.get(&theirs.nesting).copied().unwrap_or(false)
            });
            if inside {
                found.push(Found {
                    file: file.clone(),
                    range: lines.range(o.span.clone()),
                    occurrence: o.clone(),
                });
            }
        }
    }
    found
}

/// The files to read for a member, and whether each written nesting in them
/// is its class or an ancestor.
struct ClassFiles {
    files: Vec<PathBuf>,
    inside: HashMap<Vec<String>, bool>,
}

/// `None` when the class cannot be named — no checkout, a top-level ivar, or
/// a nesting the tree does not place — and the file at hand is all there is.
fn class_files(
    session: &mut Session,
    file: &Path,
    owner: &vars::Owner,
    sigil: Sigil,
) -> Option<ClassFiles> {
    if owner.nesting.is_empty() {
        return None;
    }
    let located = session.locate_query(file)?;
    let tree = session.tree(&located.root).ok()?;
    let fqn = tree.scope_fqn(&owner.nesting)?;
    tree.kind_of(&fqn)?;
    let chain: Vec<String> = match sigil == Sigil::Instance && owner.singleton {
        true => vec![fqn.clone()],
        false => tree.ancestors(&fqn).chain.clone(),
    };
    let mut files = vec![located.absolute.clone()];
    let mut seen: HashSet<PathBuf> = files.iter().cloned().collect();
    for name in &chain {
        for site in tree.sites(name) {
            if !tree.in_checkout(&site.path) || files.len() >= MAX_FILES {
                continue;
            }
            if let Some(path) = absolute_site(&located.root, &site.path)
                && seen.insert(path.clone())
            {
                files.push(path);
            }
        }
    }
    let chain: HashSet<String> = chain.into_iter().collect();
    // Placing a nesting needs the tree, which the session will not lend while
    // a document is borrowed; so every nesting the files write is placed up
    // front.
    let inside = placements(session, &located.root, &files, &chain);
    Some(ClassFiles { files, inside })
}

/// Each written nesting in these files, and whether the tree places it in
/// `chain`.
fn placements(
    session: &mut Session,
    root: &Path,
    files: &[PathBuf],
    chain: &HashSet<String>,
) -> HashMap<Vec<String>, bool> {
    let mut nestings: HashSet<Vec<String>> = HashSet::new();
    for file in files {
        if let Some(document) = session.document(file) {
            nestings.extend(document.vars().owners.iter().map(|o| o.nesting.clone()));
        }
    }
    let Ok(tree) = session.tree(root) else {
        return HashMap::new();
    };
    nestings
        .into_iter()
        .map(|nesting| {
            let inside = tree
                .scope_fqn(&nesting)
                .is_some_and(|fqn| chain.contains(&fqn));
            (nesting, inside)
        })
        .collect()
}
