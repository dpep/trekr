//! `textDocument/completion` — what can be written here, ranked by where it
//! comes from (DEC-040).
//!
//! Three contexts, read off the text before the cursor:
//!
//! * **`recv.`** — the members of whatever the receiver ladder says `recv` is:
//!   its own methods first, then each ancestor's in lookup order, private ones
//!   only when the receiver is `self`. An untyped receiver gets a short,
//!   disclosed list of same-prefix names rather than a flood.
//! * **`Scope::`** — the constants declared inside that namespace.
//! * **a bare word** — locals and parameters in scope, then the enclosing
//!   class's methods up its chain, then constants from the innermost lexical
//!   scope outward.
//!
//! The buffer is mid-edit and often does not parse where the cursor is, so the
//! word being typed is swapped for a placeholder identifier before parsing:
//! `foo.` becomes `foo.trekr_…`, a call the extractor records with its
//! receiver, nesting and singleton-ness — exactly what the ladder needs.

use super::state::Session;
use crate::cli::position::{self, Under};
use crate::core::{Facts, RecvShape};
use crate::tree::Tree;
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionItemLabelDetails, CompletionList,
    CompletionParams, CompletionResponse,
};
use std::collections::{HashMap, HashSet};

/// What the cursor is completing.
#[derive(Debug, PartialEq)]
enum Context {
    /// After `.` or `&.`.
    Member,
    /// After `Scope::` — the scope as written, empty for a leading `::`.
    Scope(String),
    /// A word on its own.
    Bare,
}

/// The most items one answer carries. A Rails model's chain has thousands of
/// methods; past this the list is marked incomplete and the client asks again
/// as the prefix narrows it.
const MAX_ITEMS: usize = 300;

/// How many names an untyped receiver is offered — enough to be useful, few
/// enough that it cannot pass for knowledge.
const MAX_GUESSES: usize = 20;

/// Stands in for the word being typed so the buffer parses there. A method
/// name nothing defines, so it never matches a real lookup.
const PLACEHOLDER: &str = "trekr_completion_placeholder";
const CONST_PLACEHOLDER: &str = "TrekrCompletionPlaceholder";

/// A checkout's tree listed for completion: namespaces by scope, methods by
/// owner. Built once per tree.
pub(crate) struct Members {
    methods: HashMap<(String, bool), Vec<Member>>,
    /// Scope FQN (empty for top level) → its direct constants and their kinds.
    children: HashMap<String, Vec<(String, String)>>,
    /// Every method name and how many definitions carry it, sorted by name —
    /// the pool for an untyped receiver.
    names: Vec<(String, usize)>,
}

/// What a completion item needs of a method, and no more. The whole
/// `MethodDef` — site path, owner, signature — held for every method in a
/// checkout was most of an LSP session's memory.
struct Member {
    name: String,
    private: bool,
    via: Option<String>,
    rbi: bool,
}

impl Members {
    pub(crate) fn of(tree: &Tree) -> Members {
        let mut methods: HashMap<(String, bool), Vec<Member>> = HashMap::new();
        let mut counts: HashMap<String, usize> = HashMap::new();
        for (owner, singleton, method) in tree.method_table() {
            *counts.entry(method.name.clone()).or_default() += 1;
            let member = Member {
                private: method.visibility == "private",
                rbi: method.site.is_rbi(),
                via: method.via,
                name: method.name,
            };
            methods.entry((owner, singleton)).or_default().push(member);
        }
        let mut children: HashMap<String, Vec<(String, String)>> = HashMap::new();
        for (fqn, kind) in tree.declared() {
            let (scope, name) = match fqn.rsplit_once("::") {
                Some((scope, name)) => (scope.to_string(), name.to_string()),
                None => (String::new(), fqn.clone()),
            };
            children.entry(scope).or_default().push((name, kind));
        }
        // By name, so the list cut at `MAX_ITEMS` is the same list every time
        // and the one the client's own sort puts first. The table is a hash
        // map, and its order changed from one process to the next. Stable,
        // so a name's definitions keep the order a lookup would see them in.
        for members in methods.values_mut() {
            members.sort_by(|a, b| a.name.cmp(&b.name));
        }
        for constants in children.values_mut() {
            constants.sort();
        }
        let mut names: Vec<(String, usize)> = counts.into_iter().collect();
        names.sort();
        Members {
            methods,
            children,
            names,
        }
    }
}

pub(crate) fn completion(
    session: &mut Session,
    params: CompletionParams,
) -> anyhow::Result<Option<CompletionResponse>> {
    let uri = params.text_document_position.text_document.uri;
    let position = params.text_document_position.position;
    let Some(path) = super::convert::uri_to_path(uri.as_str()) else {
        return Ok(None);
    };
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    let Some(text) = session.document(&path).map(|d| d.text.clone()) else {
        return Ok(None);
    };
    let offset = super::convert::offset_of(&text, position);
    let Some((start, prefix, context)) = context(&text, offset) else {
        return Ok(Some(empty(false)));
    };
    let Some(located) = session.locate(&path) else {
        return Ok(Some(empty(false)));
    };

    let placeholder = match context {
        Context::Scope(_) => CONST_PLACEHOLDER,
        _ => PLACEHOLDER,
    };
    let patched = format!("{}{placeholder}{}", &text[..start], &text[offset..]);
    let facts = crate::extract::extract(patched.as_bytes());
    let line = text[..start].matches('\n').count() as u32 + 1;
    let col = (start - text[..start].rfind('\n').map_or(0, |n| n + 1)) as u32 + 1;
    let under = position::at_facts(&facts, line, col);

    let (tree, members) = session.members(&located.root)?;
    let mut list = Ranked::new(&prefix);
    match (&context, under) {
        (Context::Member, Some(Under::Call(call))) => {
            match crate::resolve::receiver_type(tree, &facts, &call) {
                Some((fqn, singleton)) => {
                    let private = call.recv == RecvShape::SelfRecv;
                    add_methods(&mut list, tree, members, &fqn, singleton, private, 1);
                }
                None => {
                    // Untyped: a few names that fit, said to be guesses, and
                    // never on an empty prefix — that would be the whole index.
                    if prefix.is_empty() {
                        return Ok(Some(empty(true)));
                    }
                    add_guesses(&mut list, members, &prefix);
                    return Ok(Some(list.finish(true)));
                }
            }
        }
        (Context::Scope(written), Some(Under::Constant(reference))) => {
            let scope = if written.is_empty() {
                Some(String::new())
            } else {
                tree.resolve(written, &reference.nesting).fqn
            };
            if let Some(scope) = scope {
                add_constants(&mut list, tree, members, &scope, 0);
            }
        }
        (Context::Bare, Some(Under::Call(call))) => {
            add_locals(&mut list, &facts, line);
            if let Some(fqn) = tree.scope_fqn(&call.nesting) {
                add_methods(&mut list, tree, members, &fqn, call.singleton, true, 1);
            } else {
                // Top level: `self` is `main`, an Object.
                add_methods(&mut list, tree, members, "Object", false, true, 1);
            }
            if prefix.is_empty() || prefix.starts_with(|c: char| c.is_ascii_uppercase()) {
                // Innermost lexical scope first, then outward to the top.
                for depth in 0..=call.nesting.len() {
                    let scope = if depth == call.nesting.len() {
                        Some(String::new())
                    } else {
                        tree.scope_fqn(&call.nesting[depth..])
                    };
                    if let Some(scope) = scope {
                        add_constants(&mut list, tree, members, &scope, depth as u32);
                    }
                }
            }
        }
        _ => {}
    }
    Ok(Some(list.finish(false)))
}

/// Read the context off the text before the cursor: where the word being
/// typed starts, the word so far, and what precedes it. `None` where nothing
/// should be offered — a comment, a string, a symbol.
fn context(text: &str, offset: usize) -> Option<(usize, String, Context)> {
    let before = &text[..offset];
    let line_start = before.rfind('\n').map_or(0, |n| n + 1);
    if in_comment_or_string(&before[line_start..]) {
        return None;
    }
    let start = before
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_alphanumeric() || *c == '_')
        .last()
        .map_or(offset, |(i, _)| i);
    let prefix = before[start..].to_string();
    if prefix.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let lead = &before[..start];
    let context = if let Some(rest) = lead.strip_suffix("::") {
        let path_start = rest
            .char_indices()
            .rev()
            .take_while(|(_, c)| c.is_alphanumeric() || *c == '_' || *c == ':')
            .last()
            .map_or(rest.len(), |(i, _)| i);
        Context::Scope(rest[path_start..].trim_start_matches("::").to_string())
    } else if let Some(receiver) = lead.strip_suffix('.') {
        // `1.` is a float being typed, and `..` a range.
        if receiver.ends_with(|c: char| c == '.' || c.is_ascii_digit()) {
            return None;
        }
        Context::Member
    } else if lead.ends_with(':') || lead.ends_with('@') || lead.ends_with('$') {
        // A symbol, or an instance/global variable — not ours to list.
        return None;
    } else {
        Context::Bare
    };
    Some((start, prefix, context))
}

/// Is the end of this line prefix inside a comment or a string literal?
/// Deliberately simple — quotes and `#` outside them — because a wrong "no"
/// costs a missing list and a wrong "yes" costs one in a comment.
fn in_comment_or_string(line: &str) -> bool {
    let mut quote: Option<char> = None;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                if c == '\\' {
                    chars.next();
                } else if c == q {
                    quote = None;
                }
            }
            None => match c {
                '"' | '\'' | '`' => quote = Some(c),
                '#' => return true,
                _ => {}
            },
        }
    }
    quote.is_some()
}

/// Items collected with their rank, filtered by the prefix as they arrive.
struct Ranked {
    prefix: String,
    seen: HashSet<String>,
    items: Vec<CompletionItem>,
    truncated: bool,
}

impl Ranked {
    fn new(prefix: &str) -> Ranked {
        Ranked {
            prefix: prefix.to_string(),
            seen: HashSet::new(),
            items: Vec::new(),
            truncated: false,
        }
    }

    /// First come wins: an override shadows what it overrides, and a local
    /// shadows a method of the same name.
    fn add(
        &mut self,
        tier: u32,
        depth: u32,
        name: &str,
        kind: CompletionItemKind,
        detail: String,
        from: Option<String>,
    ) {
        if !matches(name, &self.prefix) || self.seen.contains(name) {
            return;
        }
        if self.items.len() >= MAX_ITEMS {
            self.truncated = true;
            return;
        }
        self.seen.insert(name.to_string());
        self.items.push(CompletionItem {
            label: name.to_string(),
            kind: Some(kind),
            detail: Some(detail),
            label_details: from.map(|description| CompletionItemLabelDetails {
                detail: None,
                description: Some(description),
            }),
            sort_text: Some(format!("{tier}{depth:04}{name}")),
            ..Default::default()
        });
    }

    fn finish(self, incomplete: bool) -> CompletionResponse {
        CompletionResponse::List(CompletionList {
            is_incomplete: incomplete || self.truncated,
            items: self.items,
        })
    }
}

fn empty(incomplete: bool) -> CompletionResponse {
    CompletionResponse::List(CompletionList {
        is_incomplete: incomplete,
        items: Vec::new(),
    })
}

/// The client filters fuzzily; this only has to keep what it could match.
/// The first character must agree (ignoring case), the rest must appear in
/// order — the same rule an editor's own filter starts from.
fn matches(name: &str, prefix: &str) -> bool {
    let mut wanted = prefix.chars().map(|c| c.to_ascii_lowercase());
    let Some(first) = wanted.next() else {
        return true;
    };
    let mut have = name.chars().map(|c| c.to_ascii_lowercase());
    if have.next() != Some(first) {
        return false;
    }
    let mut want = wanted.peekable();
    for c in have {
        if want.peek() == Some(&c) {
            want.next();
        }
    }
    want.peek().is_none()
}

/// A method name someone could type after a dot: not an operator, not a
/// setter (`name=` is written `name = …`).
fn typeable(name: &str) -> bool {
    let body = name.trim_end_matches(['?', '!']);
    !body.is_empty()
        && body.chars().all(|c| c.is_alphanumeric() || c == '_')
        && !body.starts_with(|c: char| c.is_ascii_digit())
}

/// Methods reachable on `fqn`, in lookup order: its own, then each ancestor's.
fn add_methods(
    list: &mut Ranked,
    tree: &Tree,
    members: &Members,
    fqn: &str,
    singleton: bool,
    private: bool,
    tier: u32,
) {
    for (depth, (owner, owner_singleton)) in tree.lookup_chain(fqn, singleton).iter().enumerate() {
        let Some(methods) = members.methods.get(&(owner.clone(), *owner_singleton)) else {
            continue;
        };
        // Real source before a Sorbet declaration of the same method, as a
        // lookup would prefer it.
        let mut methods: Vec<&Member> = methods.iter().collect();
        methods.sort_by_key(|m| m.rbi);
        for method in methods {
            if !typeable(&method.name) || (!private && method.private) {
                continue;
            }
            let marker = if *owner_singleton { "." } else { "#" };
            let detail = match &method.via {
                Some(via) => format!("{owner}{marker}{} ({via})", method.name),
                None => format!("{owner}{marker}{}", method.name),
            };
            list.add(
                tier,
                depth as u32,
                &method.name,
                CompletionItemKind::METHOD,
                detail,
                Some(owner.clone()),
            );
        }
    }
}

/// Constants declared directly in `scope`, and in its ancestors — `Foo::X`
/// finds an `X` Foo inherits, as Ruby's lookup does.
fn add_constants(list: &mut Ranked, tree: &Tree, members: &Members, scope: &str, depth: u32) {
    let scopes: Vec<String> = if scope.is_empty() {
        vec![String::new()]
    } else {
        tree.ancestors(scope).chain.clone()
    };
    for (step, owner) in scopes.iter().enumerate() {
        let Some(children) = members.children.get(owner) else {
            continue;
        };
        for (name, kind) in children {
            let item_kind = match kind.as_str() {
                "class" => CompletionItemKind::CLASS,
                "module" => CompletionItemKind::MODULE,
                _ => CompletionItemKind::CONSTANT,
            };
            let detail = if owner.is_empty() {
                name.clone()
            } else {
                format!("{owner}::{name}")
            };
            list.add(2, depth * 100 + step as u32, name, item_kind, detail, None);
        }
    }
}

/// Locals assigned above the cursor, and the enclosing method's parameters.
fn add_locals(list: &mut Ranked, facts: &Facts, line: u32) {
    let enclosing = facts
        .defs
        .iter()
        .filter(|d| d.kind == crate::core::Kind::Method && d.pos.line <= line && line <= d.end_line)
        .min_by_key(|d| d.end_line - d.pos.line);
    let (from, to) = enclosing.map_or((1, line), |d| (d.pos.line, line));
    if let Some(def) = enclosing {
        for param in &def.params {
            if typeable(&param.name) {
                list.add(
                    0,
                    0,
                    &param.name,
                    CompletionItemKind::VARIABLE,
                    "parameter".into(),
                    None,
                );
            }
        }
    }
    for assign in facts.assigns.iter().rev() {
        if assign.pos.line < from || assign.pos.line > to || !typeable(&assign.target) {
            continue;
        }
        if assign.target.starts_with(PLACEHOLDER) {
            continue;
        }
        list.add(
            0,
            0,
            &assign.target,
            CompletionItemKind::VARIABLE,
            "local".into(),
            None,
        );
    }
}

/// An untyped receiver: the names that fit, most-defined first, each saying
/// it is a guess.
fn add_guesses(list: &mut Ranked, members: &Members, prefix: &str) {
    let lower = prefix.to_ascii_lowercase();
    let mut fits: Vec<&(String, usize)> = members
        .names
        .iter()
        .filter(|(name, _)| typeable(name) && name.to_ascii_lowercase().starts_with(&lower))
        .collect();
    fits.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    for (rank, (name, count)) in fits.into_iter().take(MAX_GUESSES).enumerate() {
        list.add(
            3,
            rank as u32,
            name,
            CompletionItemKind::METHOD,
            format!("receiver type unknown — {count} definitions of this name"),
            Some("?".into()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at_end(text: &str) -> Option<(String, Context)> {
        context(text, text.len()).map(|(_, prefix, context)| (prefix, context))
    }

    #[test]
    fn reads_the_context_off_the_text_before_the_cursor() {
        assert_eq!(at_end("w.sa"), Some(("sa".into(), Context::Member)));
        assert_eq!(at_end("w&."), Some(("".into(), Context::Member)));
        assert_eq!(
            at_end("A::B::Wi"),
            Some(("Wi".into(), Context::Scope("A::B".into())))
        );
        assert_eq!(
            at_end("::Wi"),
            Some(("Wi".into(), Context::Scope("".into())))
        );
        assert_eq!(at_end("  sav"), Some(("sav".into(), Context::Bare)));
    }

    #[test]
    fn offers_nothing_in_a_comment_a_string_or_a_symbol() {
        assert_eq!(at_end("x = 1 # w."), None);
        assert_eq!(at_end("puts \"w."), None);
        assert_eq!(at_end("before_save :sa"), None);
        assert_eq!(at_end("@wid"), None);
        assert_eq!(at_end("x = 1."), None, "a float being typed");
        // A `#` inside a string is not a comment.
        assert!(at_end("puts '#'; w.").is_some());
    }

    #[test]
    fn keeps_what_a_fuzzy_filter_could_match() {
        assert!(matches("find_by_name", "fbn"));
        assert!(matches("Widget", "wid"));
        assert!(!matches("save", "ave"), "the first character has to agree");
        assert!(matches("anything", ""));
    }

    #[test]
    fn operators_and_setters_are_not_offered_after_a_dot() {
        assert!(typeable("valid?"));
        assert!(typeable("save!"));
        assert!(!typeable("=="));
        assert!(!typeable("name="));
        assert!(!typeable("[]"));
    }
}
