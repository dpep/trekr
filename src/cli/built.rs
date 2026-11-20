//! The method names a checkout builds at runtime, which no call site writes
//! and no index records: what `--dead` cannot see a caller in, and says so
//! per row (DEC-363).
//!
//! An interpolated symbol `:"report_#{type}"`, read from the checkout's Ruby
//! as text like the views (DEC-315), is a name of the shape `report_*`; a
//! string sent by name, `Q.new.public_send("report_#{type}")`, is one for
//! `Q`'s methods only (DEC-655); `Mailer.public_send(type, …)` sends a
//! computed name to `Mailer` — the sends as extraction reads them
//! (`Facts::sends`); and `Tools.instance_methods` lists `Tools`' methods to
//! call by name (DEC-370).
//! Evidence of a way in, never of a caller, so a row it touches keeps its
//! tier and is graded `lower`.

use super::dead_consts::{in_db, in_tests};
use crate::core::ruby::BY_NAME;
use crate::core::{SentName, SentTo};
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
    /// Each interpolated string or symbol sent as a name
    /// (`public_send("list_#{x}")`, `__send__(:"#{k}_attrs")`): its shape,
    /// the class it is sent to, and where.
    sent: Vec<(String, String, At)>,
    /// Each literal name sent that the index records no symbol for (a
    /// string, an operator): the name, as written, the class it is sent
    /// to if placed, and where, per file.
    named: Vec<(String, String, Option<String>, At)>,
}

/// An interpolated name sent, before its receiver is placed.
struct Sending {
    shape: String,
    /// A literal name, as written, that the index reads no call of: the
    /// shape is then the name itself.
    written: Option<String>,
    to: SentTo,
    /// The nesting of the send, as the parse records it.
    nesting: Vec<String>,
    at: At,
}

impl Sending {
    /// The class it is sent to, if the tree can place it.
    fn class(&self, tree: &Tree) -> Option<String> {
        let nesting = &self.nesting;
        match &self.to {
            SentTo::Own => tree.scope_fqn(nesting),
            SentTo::Constant(written) | SentTo::Made(written) => tree
                .resolve(written, nesting)
                .fqn
                .or_else(|| Some(written.clone())),
            SentTo::Value => None,
        }
    }
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
            .filter(|(path, _)| {
                [".rb", ".rake", ".erb", ".haml"]
                    .iter()
                    .any(|ext| path.ends_with(ext))
            })
            .filter_map(|(path, text)| {
                // A template's Ruby, at the template's own lines.
                let source = crate::extract::ruby_source(path, text.as_bytes());
                let text = &*String::from_utf8_lossy(&source);
                if !text.contains("#{")
                    && !text.contains("instance_methods")
                    && crate::extract::sends_in_text(text.as_bytes())
                        .next()
                        .is_none()
                {
                    return None;
                }
                let mut part = Built::default();
                // A template's sends only: its interpolated symbols are
                // mostly translation keys (`t(:"label_#{k}")`).
                let template = !path.ends_with(".rb") && !path.ends_with(".rake");
                let sendings = part.read_file(path, text, template);
                Some((part, sendings))
            })
            .collect();
        let mut built = Built::default();
        for (part, sendings) in parts {
            built.absorb(part);
            for sending in sendings {
                let class = sending.class(tree);
                if let Some(written) = sending.written {
                    let known = |(n, _, c, (path, _)): &(String, String, Option<String>, At)| {
                        *n == sending.shape && *c == class && *path == sending.at.0
                    };
                    if !built.named.iter().any(known) {
                        built
                            .named
                            .push((sending.shape, written, class, sending.at));
                    }
                    continue;
                }
                let Some(class) = class else {
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

    /// A file's finds, and its sent names, whose receiver is placed once
    /// the files are merged.
    fn read_file(&mut self, path: &str, text: &str, template: bool) -> Vec<Sending> {
        for (at, line) in text.lines().enumerate().filter(|_| !template) {
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
            for (constant, call) in listings(line) {
                if !self.listed.iter().any(|(known, _, _)| *known == constant) {
                    self.listed.push((constant, call, at.clone()));
                }
            }
        }
        // The sends, as extraction reads them; few files have one.
        if !BY_NAME.iter().any(|name| text.contains(name)) {
            return Vec::new();
        }
        let facts = crate::extract::extract(text.as_bytes());
        let mut sendings = Vec::new();
        for send in facts.sends {
            let at = (path.to_string(), send.pos.line);
            // A name computed sent to a constant or what it makes
            // (`Mailer.public_send(type)`), or a local sent to the constant
            // itself, whose last write the parse may misjudge
            // (`k = "#{k}="; Settings.public_send(k, v)`).
            let constant = match (&send.name, &send.to) {
                (SentName::Computed, SentTo::Constant(c) | SentTo::Made(c)) => Some(c),
                (SentName::Shape(_), SentTo::Constant(c)) if send.held => Some(c),
                _ => None,
            };
            // A `method(n)` handle sends only where it is run.
            if send.run
                && let Some(constant) = constant
                && !self.sent_to.iter().any(|(known, _)| known == constant)
            {
                self.sent_to.push((constant.clone(), at.clone()));
            }
            if let Some(written) = unrecorded(&send.name)
                && send.run
                && let SentName::Symbol(name) | SentName::String(name) = &send.name
            {
                sendings.push(Sending {
                    shape: name.clone(),
                    written: Some(written),
                    to: send.to.clone(),
                    nesting: send.nesting.clone(),
                    at: at.clone(),
                });
            }
            match send.name {
                // A string sent to a record (`row.public_send("total_#{kind}")`)
                // is an attribute's name far more often than a method's.
                SentName::Shape(_) if send.to == SentTo::Value => {}
                // One too short to cite anywhere (`"#{k}?"`) still fits the
                // methods of the class it is sent to.
                SentName::Shape(shape)
                    if sent_shape(&shape)
                        || matches!(send.to, SentTo::Constant(_) | SentTo::Made(_)) =>
                {
                    sendings.push(Sending {
                        shape,
                        written: None,
                        to: send.to,
                        nesting: send.nesting,
                        at,
                    })
                }
                _ => {}
            }
        }
        sendings
    }

    /// Why a method of this name, on this owner, may be reached by a name
    /// built at runtime: the shape or the constant, and where. `runs_as`
    /// is its name first, then the aliases that run its body.
    pub(super) fn reaching(
        &self,
        tree: &Tree,
        owner: &str,
        runs_as: &[(&str, bool)],
        file: &str,
    ) -> Option<String> {
        let name = runs_as.first().map_or("", |(name, _)| *name);
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
        // A string sent to a class reaches its chain, and its subclasses'
        // whole chains: the receiver may be any of them, and so reach a
        // module only a subclass includes, or a class a module's includer
        // inherits.
        let related = |class: &str| {
            names(class)
                || tree.inherits(class, owner)
                || tree.inherits(owner, class)
                || tree
                    .includers_of(class)
                    .iter()
                    .any(|sub| tree.inherits(sub, owner))
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
        // Its own file's says so already (DEC-662).
        if let Some((_, written, _, (path, line))) =
            self.named.iter().find(|(sent, _, class, (path, _))| {
                runs_as.iter().any(|(name, _)| sent == name)
                    && path != file
                    && class.as_deref().is_none_or(related)
            })
        {
            return Some(format!(
                "`{written}` is sent by name at {path}:{line}, which is not read as a call"
            ));
        }
        let (constant, call, (path, line)) = self.listed.iter().find(|(c, _, _)| names(c))?;
        Some(format!(
            "its module's methods are listed at runtime ({constant}.{call} at {path}:{line})"
        ))
    }
}

/// A literal name a send hands that extraction records no symbol for, so
/// nothing reads it as a call (DEC-664): a string, or a symbol that is no
/// plain method name (`:[]`, `:+`). As written.
fn unrecorded(name: &SentName) -> Option<String> {
    match name {
        SentName::String(name) => Some(format!("\"{name}\"")),
        SentName::Symbol(name)
            if !name.starts_with(|c: char| c.is_ascii_lowercase() || c == '_') =>
        {
            Some(format!(":{name}"))
        }
        _ => None,
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
        if let Some(shape) = shape.filter(|shape| named_shape(shape)) {
            shapes.push(shape);
        }
    }
    shapes
}

/// A shape that spells three name characters or more before its first
/// interpolation, as DEC-363 counts a symbol's.
fn named_shape(shape: &str) -> bool {
    shape.find('*').is_some_and(|prefix| prefix >= 3)
}

/// A shape sent as a method's name: a named one, or one that spells four
/// characters or more anywhere (`*_attrs`). Sent, a leading interpolation is
/// a method's name, not the attribute's DEC-363 guards against; `*?` and
/// `*=` would make nearly every predicate and writer a candidate.
fn sent_shape(shape: &str) -> bool {
    named_shape(shape) || (shape.contains('*') && shape.len() - shape.matches('*').count() >= 4)
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

    /// The shapes `read_file` finds sent, by line, and the constants it
    /// finds a computed name sent to.
    fn read(text: &str) -> (Vec<(String, u32)>, Vec<String>) {
        let mut built = Built::default();
        let sendings = built.read_file("lib/x.rb", text, false);
        let shapes = sendings.into_iter().map(|s| (s.shape, s.at.1)).collect();
        (shapes, built.sent_to.into_iter().map(|(c, _)| c).collect())
    }

    #[test]
    fn a_shape_sent_by_name_is_one_a_method_may_have() {
        let shapes =
            |text: &str| -> Vec<String> { read(text).0.into_iter().map(|s| s.0).collect() };
        assert_eq!(
            shapes(r##"Q.new.public_send("list_#{filter}", user)"##),
            ["list_*"]
        );
        assert_eq!(shapes(r##"send("#{name}_handler")"##), ["*_handler"]);
        assert!(shapes(r##"try("to#{kind}")"##).is_empty());
        assert!(shapes(r##"public_send("#{key}=", value)"##).is_empty());
        assert!(shapes(r##"send(:"#{name}?")"##).is_empty());
        assert!(shapes(r##"define_method("#{name}_attrs") {}"##).is_empty());
        // Sent to a value: an attribute's name far more often.
        assert!(shapes(r##"row.public_send("total_#{kind}")"##).is_empty());
        // Written over lines, by the parse.
        assert_eq!(
            read("Q.new\n  .public_send(\n    \"list_#{f}\")\n").0,
            [("list_*".to_string(), 3)]
        );
    }

    #[test]
    fn a_computed_name_sent_to_a_constant_names_the_constant() {
        assert_eq!(
            read("Mail::Notifier.public_send(type, user)").1,
            ["Mail::Notifier"]
        );
        assert!(read("Notifier.public_send(:welcome, user)").1.is_empty());
        assert!(read("record.send(name)").1.is_empty());
        assert_eq!(read("Notifier.try(kind, user)").1, ["Notifier"]);
        assert_eq!(read("Notifier.try!(kind)").1, ["Notifier"]);
        assert_eq!(
            read("Notifier.with(to: x)\n  .public_send(type)").1,
            ["Notifier"]
        );
        assert_eq!(read("View.new.send(name, *args)").1, ["View"]);
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
            false,
        );
        assert!(built.shapes.is_empty() && built.sent_to.is_empty() && sendings.is_empty());
    }
}
