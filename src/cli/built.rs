//! The method names a checkout builds at runtime, which no call site writes
//! and no index records: what `--dead` cannot see a caller in, and says so
//! per row (DEC-363).
//!
//! Read from the checkout's Ruby as text, like the views (DEC-315): an
//! interpolated symbol `:"report_#{type}"` is a name of the shape `report_*`,
//! `Mailer.public_send(type, …)` sends a computed name to `Mailer`, and
//! `Tools.instance_methods` lists `Tools`' methods to call by name (DEC-370).
//! Evidence of a way in, never of a caller, so a row it touches keeps its
//! tier and is graded `lower`.

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
}

/// The calls that list a module's methods, for a caller to call by name.
const LISTS: [&str; 2] = ["public_instance_methods", "instance_methods"];

/// The calls that send the name they are handed.
const SENDS: [&str; 3] = ["public_send", "__send__", "send"];

impl Built {
    /// Every Ruby file git knows in the checkout at `root`, less its tests
    /// and migrations: a name a spec builds is a test's value, and one a
    /// migration builds is a column's, not a way into the app.
    pub(super) fn read(root: &Path) -> Built {
        let args = ["ls-files", "-z", "--", "*.rb", "*.rake"];
        let Ok(out) = Command::new("git").arg("-C").arg(root).args(args).output() else {
            return Built::default();
        };
        let mut built = Built::default();
        for path in out.stdout.split(|b| *b == 0).filter(|p| !p.is_empty()) {
            let path = String::from_utf8_lossy(path).into_owned();
            if is_test(&path) {
                continue;
            }
            let Ok(bytes) = std::fs::read(root.join(&path)) else {
                continue;
            };
            let text = String::from_utf8_lossy(&bytes);
            if !text.contains("#{")
                && !SENDS.iter().any(|s| text.contains(s))
                && !text.contains("instance_methods")
            {
                continue;
            }
            built.read_file(&path, &text);
        }
        built
    }

    fn read_file(&mut self, path: &str, text: &str) {
        for (at, line) in text.lines().enumerate() {
            let at = (path.to_string(), at as u32 + 1);
            for shape in interpolated_symbols(line) {
                if !self.shapes.iter().any(|(known, _)| *known == shape) {
                    self.shapes.push((shape, at.clone()));
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
    }

    /// Why a method of this name, on this owner, may be reached by a name
    /// built at runtime: the shape or the constant, and where.
    pub(super) fn reaching(&self, owner: &str, name: &str) -> Option<String> {
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

/// A spec, a test, or a database migration.
fn is_test(path: &str) -> bool {
    path.starts_with("db/")
        || path
            .split('/')
            .rev()
            .skip(1)
            .any(|dir| matches!(dir, "spec" | "test"))
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

/// The constants a line sends a computed name to: `Const.public_send(type`
/// and the like, whose first argument is no literal.
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
    fn a_computed_name_sent_to_a_constant_names_the_constant() {
        assert_eq!(
            computed_sends("Mail::Notifier.public_send(type, user)"),
            ["Mail::Notifier"]
        );
        assert!(computed_sends("Notifier.public_send(:welcome, user)").is_empty());
        assert!(computed_sends("record.send(name)").is_empty());
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
    fn a_spec_or_test_directory_is_a_test() {
        assert!(is_test("spec/models/widget_spec.rb"));
        assert!(is_test("plugins/chat/test/x.rb"));
        assert!(is_test("db/migrate/20240101_add_score.rb"));
        assert!(!is_test("app/models/spec.rb"));
    }
}
