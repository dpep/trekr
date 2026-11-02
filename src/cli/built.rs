//! The method names a checkout builds at runtime, which no call site writes
//! and no index records: what `--dead` cannot see a caller in, and says so
//! per row (DEC-363).
//!
//! Read from the checkout's Ruby as text, like the views (DEC-315): an
//! interpolated symbol `:"report_#{type}"` is a name of the shape `report_*`,
//! as is a string sent by name, `Q.new.public_send("report_#{type}")`, for
//! `Q`'s methods only (DEC-655), `Mailer.public_send(type, …)` sends a
//! computed name to `Mailer`, and
//! `Tools.instance_methods` lists `Tools`' methods to call by name (DEC-370).
//! Evidence of a way in, never of a caller, so a row it touches keeps its
//! tier and is graded `lower`.

use super::dead_consts::{in_db, in_tests};
use crate::core::ruby::SENDS;
use crate::tree::Tree;
use rayon::prelude::*;
use std::path::Path;
use std::process::Command;

/// A place the checkout writes it, relative to the checkout.
pub(super) type At = (String, u32);

#[derive(Default)]
pub(super) struct Built {
    /// Each interpolated symbol's shape (`report_*`), where first written.
    shapes: Vec<(String, At)>,
    /// Each constant a computed name is sent to, where first sent.
    sent_to: Vec<(String, At)>,
    /// Each constant whose methods are listed, by the listing call, where
    /// first listed.
    listed: Vec<(String, String, At)>,
    /// Each interpolated string sent as a name (`public_send("list_#{x}")`):
    /// its shape, the class it is sent to, and where.
    sent: Vec<(String, String, At)>,
}

/// An interpolated string sent as a name, before its receiver is placed.
struct Sending {
    shape: String,
    to: To,
    /// The nesting of the send, as the parse records it.
    nesting: Option<Vec<String>>,
    at: At,
}

impl Sending {
    /// The class it is sent to, if the tree can place it.
    fn class(&self, tree: &Tree) -> Option<String> {
        let nesting = self.nesting.as_deref().unwrap_or_default();
        match &self.to {
            To::Own => tree.scope_fqn(nesting),
            To::Constant(written) => tree
                .resolve(written, nesting)
                .fqn
                .or_else(|| Some(written.clone())),
        }
    }
}

/// What a sent string's receiver is, as written. A value's is not read: a
/// string sent to a record (`row.public_send("total_#{kind}")`) is an
/// attribute's name far more often than a method's.
#[derive(Debug, PartialEq)]
enum To {
    /// No receiver, `self`, or a bare `new(…)`: the class the send is in.
    Own,
    /// A chain that starts at a constant: `TopicQuery.new(…).public_send`.
    Constant(String),
}

/// The calls that list a module's methods, for a caller to call by name.
const LISTS: [&str; 2] = ["public_instance_methods", "instance_methods"];

/// The checkout's text files `--dead` reads beside its index, each read
/// once for every reader: Ruby, rake, gemspec and rackup files, templates,
/// and scripts with no extension, less tests and locales.
pub(super) struct Texts {
    /// Path relative to the checkout, and contents, in path order.
    pub(super) files: Vec<(String, String)>,
}

/// The extensions read as Ruby.
pub(super) const RUBY: [&str; 4] = [".rb", ".rake", ".gemspec", ".ru"];

/// The templates whose Ruby is read too.
pub(super) const TEMPLATES: [&str; 6] =
    [".erb", ".haml", ".slim", ".jbuilder", ".rabl", ".builder"];

impl Texts {
    pub(super) fn read(root: &Path) -> Texts {
        let Ok(out) = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["ls-files", "-z"])
            .output()
        else {
            return Texts { files: Vec::new() };
        };
        let wanted = |path: &str| {
            let name = path.rsplit('/').next().unwrap_or(path);
            !name.contains('.') || RUBY.iter().chain(&TEMPLATES).any(|ext| name.ends_with(ext))
        };
        let paths: Vec<String> = out
            .stdout
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
            .map(|path| String::from_utf8_lossy(path).into_owned())
            .filter(|path| {
                wanted(path) && !in_tests(path) && !in_db(path) && !path.contains("/locales/")
            })
            .collect();
        let files = paths
            .into_par_iter()
            .filter_map(|path| {
                let bytes = crate::scan::read_source(root.join(&path)).ok()?;
                Some((path, String::from_utf8_lossy(&bytes).into_owned()))
            })
            .collect();
        Texts { files }
    }
}

impl Built {
    /// Every Ruby file of the checkout, less its tests and migrations: a
    /// name a spec builds is a test's value, and one a migration builds is a
    /// column's, not a way into the app.
    pub(super) fn read(texts: &Texts, tree: &Tree) -> Built {
        // Each file on its own worker; merged in path order, so what is
        // "first written" does not depend on which worker finished first.
        let parts: Vec<(Built, Vec<Sending>)> = texts
            .files
            .par_iter()
            .filter(|(path, _)| path.ends_with(".rb") || path.ends_with(".rake"))
            .filter_map(|(path, text)| {
                if !text.contains("#{")
                    && !SENDS.iter().any(|s| text.contains(&format!(".{s}(")))
                    && !text.contains("instance_methods")
                {
                    return None;
                }
                let mut part = Built::default();
                let sendings = part.read_file(path, text);
                Some((part, sendings))
            })
            .collect();
        let mut built = Built::default();
        for (part, sendings) in parts {
            built.absorb(part);
            for sending in sendings {
                let Some(class) = sending.class(tree) else {
                    continue;
                };
                let (shape, at) = (sending.shape, sending.at);
                if !built
                    .sent
                    .iter()
                    .any(|(s, c, _)| *s == shape && *c == class)
                {
                    built.sent.push((shape, class, at));
                }
            }
        }
        built
    }

    /// Another file's finds, keeping each name where it was first written.
    fn absorb(&mut self, part: Built) {
        for (shape, at) in part.shapes {
            if !self.shapes.iter().any(|(known, _)| *known == shape) {
                self.shapes.push((shape, at));
            }
        }
        for (constant, at) in part.sent_to {
            if !self.sent_to.iter().any(|(known, _)| *known == constant) {
                self.sent_to.push((constant, at));
            }
        }
        for (constant, call, at) in part.listed {
            if !self.listed.iter().any(|(known, _, _)| *known == constant) {
                self.listed.push((constant, call, at));
            }
        }
    }

    /// A file's finds, and its sent strings, whose receiver is placed once
    /// the files are merged.
    fn read_file(&mut self, path: &str, text: &str) -> Vec<Sending> {
        let mut sendings = Vec::new();
        let mut held = Vec::new();
        for (at, line) in text.lines().enumerate() {
            // A comment's example is no name the code builds.
            if line.trim_start().starts_with('#') {
                continue;
            }
            let at = (path.to_string(), at as u32 + 1);
            for shape in interpolated_symbols(line) {
                if !self.shapes.iter().any(|(known, _)| *known == shape) {
                    self.shapes.push((shape, at.clone()));
                }
            }
            let indent = line.len() - line.trim_start().len();
            for statement in statements(line) {
                // A local is the method's: another method's of the same name
                // is another local.
                if statement.trim_start().starts_with("def ") {
                    held.clear();
                }
                hold(&mut held, statement, indent);
                for (shape, to) in sent_strings(statement, &held) {
                    sendings.push(Sending {
                        shape,
                        to,
                        nesting: None,
                        at: at.clone(),
                    });
                }
            }
            for constant in computed_sends(line) {
                if !self.sent_to.iter().any(|(known, _)| *known == constant) {
                    self.sent_to.push((constant, at.clone()));
                }
            }
            for (constant, call) in listings(line) {
                if !self.listed.iter().any(|(known, _, _)| *known == constant) {
                    self.listed.push((constant, call, at.clone()));
                }
            }
        }
        // The class a send is written in, from the parse: few files send a
        // string, so few pay for it.
        if !sendings.is_empty() {
            let facts = crate::extract::extract(text.as_bytes());
            for sending in &mut sendings {
                sending.nesting = facts
                    .calls
                    .iter()
                    .find(|c| c.pos.line == sending.at.1 && SENDS.contains(&c.name.as_str()))
                    .map(|c| c.nesting.clone());
            }
        }
        sendings
    }

    /// Why a method of this name, on this owner, may be reached by a name
    /// built at runtime: the shape or the constant, and where.
    pub(super) fn reaching(&self, tree: &Tree, owner: &str, name: &str) -> Option<String> {
        if let Some((shape, (path, line))) = self
            .shapes
            .iter()
            .find(|(shape, _)| crate::core::shape_matches(shape, name))
        {
            return Some(format!(
                "a name of its shape is built at runtime (`{shape}` at {path}:{line})"
            ));
        }
        let names = |constant: &str| owner == constant || owner.ends_with(&format!("::{constant}"));
        // A string sent to a class reaches its chain, and its subclasses'.
        let related = |class: &str| {
            names(class) || tree.inherits(class, owner) || tree.inherits(owner, class)
        };
        if let Some((shape, _, (path, line))) = self
            .sent
            .iter()
            .find(|(shape, class, _)| crate::core::shape_matches(shape, name) && related(class))
        {
            return Some(format!(
                "a name of its shape is sent at runtime (`{shape}` at {path}:{line})"
            ));
        }
        if let Some((constant, (path, line))) = self.sent_to.iter().find(|(c, _)| names(c)) {
            return Some(format!(
                "a name computed at runtime is sent to {constant} at {path}:{line}"
            ));
        }
        let (constant, call, (path, line)) = self.listed.iter().find(|(c, _, _)| names(c))?;
        Some(format!(
            "its module's methods are listed at runtime ({constant}.{call} at {path}:{line})"
        ))
    }
}

/// The shapes of a line's `:"…#{…}…"` symbols that start with three name
/// characters or more, `*` for each interpolation. One that starts with its
/// interpolation is an attribute's derived name far more often than a
/// method's (`:"#{field}_count"`, `:"#{period}_score"`), and a short prefix
/// would make nearly every name a candidate, as DEC-163 found.
fn interpolated_symbols(line: &str) -> Vec<String> {
    let mut shapes = Vec::new();
    let mut rest = line;
    while let Some(open) = rest.find(":\"") {
        let after = &rest[open + 2..];
        let (shape, used) = read_shape(after);
        rest = &after[used..];
        let Some(shape) = shape else { continue };
        let prefix = shape.find('*').unwrap_or(shape.len());
        if shape.contains('*') && prefix >= 3 {
            shapes.push(shape);
        }
    }
    shapes
}

/// The shapes of a line's interpolated strings handed to a send as the name:
/// `public_send("list_#{action}")`, on any receiver or none, under the
/// symbols' rule, with what it is sent to. A string elsewhere is a key or a
/// message far more often.
fn sent_strings(line: &str, held: &[Held]) -> Vec<(String, To)> {
    let mut shapes = Vec::new();
    for send in SENDS {
        let mut rest = line;
        let mut offset = 0;
        while let Some(found) = rest.find(send) {
            let at = offset + found;
            let before = &line[..at];
            let after = &line[at + send.len()..];
            rest = after;
            offset = at + send.len();
            // `resend(` and `try_send(` are other methods.
            if before.ends_with(word) || after.starts_with(word) {
                continue;
            }
            let argument = after.strip_prefix('(').unwrap_or(after).trim_start();
            let shape = match argument.strip_prefix('"') {
                Some(body) => read_shape(body).0.filter(|shape| named_shape(shape)),
                // A local the method last assigned a string of the shape.
                None => {
                    let local: String = argument.chars().take_while(|c| word(*c)).collect();
                    held.iter()
                        .rev()
                        .find(|h| h.local == local)
                        .and_then(|h| h.shape.clone())
                }
            };
            if let (Some(shape), Some(to)) = (shape, receiver(before)) {
                shapes.push((shape, to));
            }
        }
    }
    shapes
}

/// A shape that spells three name characters or more before its first
/// interpolation, as DEC-363 counts a symbol's.
fn named_shape(shape: &str) -> bool {
    shape.find('*').is_some_and(|prefix| prefix >= 3)
}

/// A local a method assigned: the shape of the string it holds, or `None`
/// once something else replaced it, and the indentation it was assigned at.
#[derive(Debug, PartialEq)]
struct Held {
    local: String,
    shape: Option<String>,
    indent: usize,
}

/// Records what a statement assigns a local. A string of a shape is held;
/// anything else replaces it, unless the assignment may not run: a trailing
/// `if`/`unless`, or written deeper than the string was (inside a branch).
fn hold(held: &mut Vec<Held>, statement: &str, indent: usize) {
    if let Some((local, shape)) = held_string(statement) {
        held.push(Held {
            local,
            shape: Some(shape),
            indent,
        });
        return;
    }
    let Some((local, value)) = statement.trim_start().split_once(" = ") else {
        return;
    };
    let replaced = held
        .iter()
        .rev()
        .find(|h| h.local == local)
        .is_some_and(|h| h.shape.is_some() && indent <= h.indent);
    if replaced && !value.contains(" if ") && !value.contains(" unless ") {
        held.push(Held {
            local: local.to_string(),
            shape: None,
            indent,
        });
    }
}

/// A line's statements: split at each `;` outside a string, up to a
/// trailing comment.
fn statements(line: &str) -> Vec<&str> {
    let bytes = line.as_bytes();
    let mut found = Vec::new();
    let (mut start, mut i, mut quote) = (0, 0, None);
    while i < bytes.len() {
        let c = bytes[i];
        match quote {
            Some(_) if c == b'\\' => i += 1,
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == b'"' || c == b'\'' => quote = Some(c),
            None if c == b'#' => break,
            None if c == b';' => {
                found.push(&line[start..i]);
                start = i + 1;
            }
            None => {}
        }
        i += 1;
    }
    found.push(&line[start..i.min(bytes.len())]);
    found
}

/// `validate_method = "validate_#{name}"`: the local a statement assigns an
/// interpolated string of a shape to, whole, and the shape.
fn held_string(statement: &str) -> Option<(String, String)> {
    let (local, value) = statement.trim_start().split_once(" = \"")?;
    if local.is_empty()
        || !local
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return None;
    }
    let (shape, used) = read_shape(value);
    let rest = value[used..].trim_start();
    (rest.is_empty() || rest.starts_with('#'))
        .then_some(shape)
        .flatten()
        .filter(|shape| named_shape(shape))
        .map(|shape| (local.to_string(), shape))
}

/// A character of a method's name.
fn word(c: char) -> bool {
    c.is_ascii_alphanumeric() || "_?!".contains(c)
}

/// What the text before a call writes as its receiver: nothing, `self`, a
/// constant, or a chain from one through calls whose answer is known; `None`
/// for a value. `new` answers an instance; `with` is ActionMailer's, whose
/// answer runs the mailer's actions (`AdminMailer.with(…).public_send`). Any
/// other call may answer anything (`Widget.for(1)`).
fn receiver(before: &str) -> Option<To> {
    let Some(mut chain) = before.strip_suffix('.') else {
        return Some(To::Own);
    };
    let mut calls = Vec::new();
    loop {
        if let Some(open) = chain.strip_suffix(')').and_then(opening) {
            chain = &chain[..open];
        }
        let start = chain
            .char_indices()
            .rfind(|(_, c)| !(word(*c) || *c == ':'))
            .map_or(0, |(at, c)| at + c.len_utf8());
        let (head, segment) = chain.split_at(start);
        if let Some(rest) = head.strip_suffix('.') {
            calls.push(segment);
            chain = rest;
            continue;
        }
        let segment = segment.trim_start_matches("::");
        let through = |known: &[&str]| calls.iter().all(|call| known.contains(call));
        return match segment {
            "self" | "new" if through(&["class", "new"]) => Some(To::Own),
            _ if through(&["new", "with"])
                && constant_before(segment).is_some_and(|c| c == segment) =>
            {
                Some(To::Constant(segment.to_string()))
            }
            _ => None,
        };
    }
}

/// Where the `(` opens whose `)` ends `text` (already stripped), if it does.
fn opening(text: &str) -> Option<usize> {
    let mut depth = 1;
    for (at, c) in text.char_indices().rev() {
        match c {
            ')' => depth += 1,
            '(' => depth -= 1,
            _ => {}
        }
        if depth == 0 {
            return Some(at);
        }
    }
    None
}

/// A double-quoted literal's body as a shape, up to its closing quote, and
/// how many bytes it took. None when its text is no method name's.
fn read_shape(body: &str) -> (Option<String>, usize) {
    let mut shape = String::new();
    let mut named = true;
    let mut chars = body.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        match c {
            '"' => return (named.then_some(shape), at + 1),
            '\\' => {
                chars.next();
                named = false;
            }
            '#' if chars.peek().is_some_and(|(_, n)| *n == '{') => {
                chars.next();
                let mut depth = 1;
                for (_, inner) in chars.by_ref() {
                    match inner {
                        '{' => depth += 1,
                        '}' => depth -= 1,
                        _ => {}
                    }
                    if depth == 0 {
                        break;
                    }
                }
                if !shape.ends_with('*') {
                    shape.push('*');
                }
            }
            c if c.is_ascii_alphanumeric() || "_?!=".contains(c) => shape.push(c),
            _ => named = false,
        }
    }
    (None, body.len())
}

/// The constants a line sends a computed name to: `Const.public_send(type`,
/// `Const.try(type` and the like (`core::ruby::SENDS`), whose first
/// argument is no literal.
fn computed_sends(line: &str) -> Vec<String> {
    let mut found = Vec::new();
    for send in SENDS {
        let call = format!(".{send}(");
        let mut rest = line;
        while let Some(at) = rest.find(&call) {
            let receiver = constant_before(&rest[..at]);
            let argument = rest[at + call.len()..].trim_start();
            rest = &rest[at + call.len()..];
            let literal = argument.starts_with([':', '"', '\'', ')']) || argument.is_empty();
            if let Some(receiver) = receiver.filter(|_| !literal) {
                found.push(receiver);
            }
        }
    }
    found
}

/// The constants a line lists the instance methods of: `Tools.instance_methods`.
fn listings(line: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut rest = line;
    while let Some(dot) = rest.find('.') {
        let after = &rest[dot + 1..];
        let call = LISTS.iter().find(|call| {
            after.starts_with(*call)
                && !after[call.len()..].starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
        });
        if let (Some(call), Some(constant)) = (call, constant_before(&rest[..dot])) {
            found.push((constant, call.to_string()));
        }
        rest = after;
    }
    found
}

/// The constant path that ends `text`, if it ends in one: `Mailers::Notifier`.
fn constant_before(text: &str) -> Option<String> {
    let start = text
        .char_indices()
        .rfind(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '_' || *c == ':'))
        .map_or(0, |(at, c)| at + c.len_utf8());
    let written = text[start..].trim_start_matches("::");
    written
        .split("::")
        .all(|part| part.starts_with(|c: char| c.is_ascii_uppercase()))
        .then(|| written.to_string())
        .filter(|w| !w.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_interpolated_symbol_is_a_shape_when_it_spells_enough() {
        assert_eq!(
            interpolated_symbols(r##"m = :"report_#{type}""##),
            ["report_*"]
        );
        assert!(interpolated_symbols(r##":"#{a}_rtl" + :"#{b}?""##).is_empty());
        assert!(interpolated_symbols(r##"x = "report_#{type}""##).is_empty());
        assert!(interpolated_symbols(r##":"a b #{c}""##).is_empty());
    }

    #[test]
    fn an_interpolated_string_sent_by_name_is_a_shape() {
        let shapes =
            |line| -> Vec<String> { sent_strings(line, &[]).into_iter().map(|s| s.0).collect() };
        assert_eq!(
            shapes(r##"Q.new.public_send("list_#{filter}", user)"##),
            ["list_*"]
        );
        assert_eq!(shapes(r##"send "validate_#{name}""##), ["validate_*"]);
        assert_eq!(shapes(r##"self.try("to_#{kind}")"##), ["to_*"]);
        assert!(shapes(r##"try("to#{kind}")"##).is_empty());
        assert!(shapes(r##"row.public_send("total_#{kind}")"##).is_empty());
        assert!(shapes(r##"send("#{name}_handler")"##).is_empty());
        assert!(shapes(r##"resend("list_#{a}")"##).is_empty());
        assert!(shapes(r##"log("list_#{a}")"##).is_empty());
    }

    #[test]
    fn a_local_holding_a_shaped_string_sends_its_shape() {
        let mut held = Vec::new();
        hold(&mut held, r##"    check = "validate_#{name}""##, 4);
        assert_eq!(held[0].shape.as_deref(), Some("validate_*"));
        assert_eq!(
            sent_strings("public_send(check, val) if respond_to?(check)", &held),
            [("validate_*".to_string(), To::Own)]
        );
        assert!(sent_strings("public_send(checked)", &held).is_empty());
        assert!(held_string(r##"key = "validate_#{name}" + x"##).is_none());
        assert!(held_string(r##"@key = "validate_#{name}""##).is_none());
        assert!(held_string(r##"key = "#{name}_check""##).is_none());
    }

    /// The shapes `read_file` finds sent, by line.
    fn sent_lines(text: &str) -> Vec<(String, u32)> {
        let sendings = Built::default().read_file("lib/x.rb", text);
        sendings.into_iter().map(|s| (s.shape, s.at.1)).collect()
    }

    #[test]
    fn a_local_is_read_in_its_own_method_as_last_assigned() {
        // Assigned on the send's own line, as one statement before it.
        assert_eq!(
            sent_lines(
                "class W; def go(n, v); rule = \"handle_#{n}\"; public_send(rule, v); end; end\n"
            ),
            [("handle_*".to_string(), 1)]
        );
        // Another method's parameter of the same name is another local.
        assert!(
            sent_lines(
                "class W\n  def a(x)\n    m = \"handle_#{x}\"\n  end\n\n  def b(m)\n    send(m)\n  end\nend\n"
            )
            .is_empty()
        );
        // Replaced before the send: not the string any more.
        assert!(
            sent_lines(
                "class W\n  def a(x)\n    m = \"handle_#{x}\"\n    m = \"other\"\n    send(m)\n  end\nend\n"
            )
            .is_empty()
        );
        // Replaced only on a branch, or under a modifier: may still be.
        for replaced in [
            "    if x\n      m = \"other\"\n    end\n",
            "    m = \"other\" if x\n",
        ] {
            let text = format!(
                "class W\n  def a(x)\n    m = \"handle_#{{x}}\"\n{replaced}    send(m)\n  end\nend\n"
            );
            assert_eq!(sent_lines(&text).len(), 1, "{text}");
        }
    }

    #[test]
    fn a_line_splits_into_statements_outside_strings() {
        assert_eq!(
            statements(r#"a = "x;y"; b(1) # c; d"#),
            [r#"a = "x;y""#, " b(1) "]
        );
    }

    #[test]
    fn a_sent_strings_receiver_is_read_from_its_chain() {
        assert_eq!(receiver("    if "), Some(To::Own));
        assert_eq!(receiver("x = self."), Some(To::Own));
        assert_eq!(receiver("  new(result, opts)."), Some(To::Own));
        assert_eq!(
            receiver("@list = TopicQuery.new(user, f(opts))."),
            Some(To::Constant("TopicQuery".into()))
        );
        assert_eq!(
            receiver("::Mail::Admin.with(to: user.account)."),
            Some(To::Constant("Mail::Admin".into()))
        );
        assert_eq!(receiver("self.class."), Some(To::Own));
        assert_eq!(receiver("Widget.for(1)."), None);
        assert_eq!(receiver("self.helper."), None);
        assert_eq!(receiver("record."), None);
        assert_eq!(receiver("@q.new."), None);
        assert_eq!(receiver("x&."), None);
    }

    #[test]
    fn a_computed_name_sent_to_a_constant_names_the_constant() {
        assert_eq!(
            computed_sends("Mail::Notifier.public_send(type, user)"),
            ["Mail::Notifier"]
        );
        assert!(computed_sends("Notifier.public_send(:welcome, user)").is_empty());
        assert!(computed_sends("record.send(name)").is_empty());
        assert_eq!(computed_sends("Notifier.try(kind, user)"), ["Notifier"]);
        assert_eq!(computed_sends("Notifier.try!(kind)"), ["Notifier"]);
    }

    #[test]
    fn a_listing_of_a_constants_methods_names_the_constant() {
        assert_eq!(
            listings("Helpers.instance_methods.each do |name|"),
            [("Helpers".to_string(), "instance_methods".to_string())]
        );
        assert!(listings("klass.instance_methods(false)").is_empty());
        assert!(listings("# ―Tools.instance_methods").len() == 1);
        assert!(listings("Tools.instance_methods_of(x)").is_empty());
    }

    #[test]
    fn a_comment_builds_no_name() {
        let mut built = Built::default();
        let sendings = built.read_file(
            "app/x.rb",
            "# Mailer.public_send(type)\n  # :\"report_#{type}\"\n  # send(\"list_#{x}\")\n",
        );
        assert!(built.shapes.is_empty() && built.sent_to.is_empty() && sendings.is_empty());
    }
}
