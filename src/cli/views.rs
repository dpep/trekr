//! The names a checkout's view templates write in their Ruby: what `--dead`
//! cannot see calls from, and says so per row (DEC-315).
//!
//! Not a reading of the templates. A name here is a word that appears where
//! a template runs Ruby — a Slim line, a Jbuilder file — with no receiver
//! typed and no scope known, which is evidence enough for a caveat and never
//! for a caller. ERB, Haml and RABL templates are not among them: the index
//! reads those, and their calls are call sites (DEC-520, DEC-658).

// Scans Ruby as text: a str slice must say why its offset is a char boundary (DEC-835).
#![deny(clippy::string_slice)]

use std::collections::HashMap;
use std::path::Path;

use crate::core::text::is_ident;

/// Template extensions whose Ruby a view runs, and the index does not read.
const TEMPLATES: [&str; 3] = ["*.slim", "*.jbuilder", "*.builder"];

#[derive(Default)]
pub(super) struct Views {
    /// Each name, with the first template (relative to the checkout) that
    /// writes it.
    names: HashMap<String, String>,
    /// Each constant path a template writes (`Widget::LIMIT`), with the
    /// first template that writes it: resolved from the top level, where a
    /// template's constants are looked up, by `--dead`'s constants (DEC-420).
    constants: HashMap<String, String>,
}

impl Views {
    /// Every template in the checkout at `root`, as the index lists files.
    /// None when git cannot list them, which is also the answer for a
    /// checkout with none.
    pub(super) fn read(root: &Path) -> Views {
        let mut views = Views::default();
        for path in crate::scan::listed(root, &TEMPLATES) {
            let Ok(bytes) = crate::scan::read_source(root.join(&path)) else {
                continue;
            };
            let Some(text) = template_text(&bytes) else {
                continue;
            };
            let ruby = if path.ends_with(".slim") {
                crate::extract::slim::slim_ruby(&text)
            } else {
                text.into_owned()
            };
            // Line by line: a quote a line leaves open is a slip of the
            // reading, and must not swallow the template after it.
            for name in ruby.lines().flat_map(words) {
                views.names.entry(name).or_insert_with(|| path.clone());
            }
            for constant in ruby.lines().flat_map(constant_paths) {
                views
                    .constants
                    .entry(constant)
                    .or_insert_with(|| path.clone());
            }
        }
        views
    }

    /// The first template that writes `name`, if one does.
    pub(super) fn naming(&self, name: &str) -> Option<&str> {
        self.names.get(name).map(String::as_str)
    }

    /// Each constant path the templates write, with the first that does.
    pub(super) fn constants(&self) -> impl Iterator<Item = (&str, &str)> {
        self.constants
            .iter()
            .map(|(constant, path)| (constant.as_str(), path.as_str()))
    }
}

/// Whether a file with no extension is a Ruby script (`exe/tool`, `bin/setup`):
/// its first line a shebang that names ruby. The index reads none of them.
pub(super) fn ruby_script(path: &str, text: &str) -> bool {
    !path.rsplit('/').next().unwrap_or(path).contains('.')
        && text.starts_with("#!")
        && text
            .lines()
            .next()
            .is_some_and(|first| first.contains("ruby"))
}

/// The method names the checkout's Ruby scripts write, each with the first
/// script that does: as views' names are, evidence of a way in that no call
/// site records.
#[derive(Default)]
pub(super) struct Scripts {
    names: HashMap<String, String>,
}

impl Scripts {
    pub(super) fn read(texts: &super::built::Texts) -> Scripts {
        let mut scripts = Scripts::default();
        for (path, text) in texts
            .files
            .iter()
            .filter(|(path, text)| ruby_script(path, text))
        {
            for name in script_names(text) {
                scripts.names.entry(name).or_insert_with(|| path.clone());
            }
        }
        scripts
    }

    /// The first script that writes `name`, if one does.
    pub(super) fn naming(&self, name: &str) -> Option<&str> {
        self.names.get(name).map(String::as_str)
    }
}

/// The names a script's code writes, less those it defines (`def run`,
/// `def self.run`): its own methods, not calls of the checkout's.
fn script_names(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in script_code(text) {
        let words = words(line);
        for (at, name) in words.iter().enumerate() {
            let before: Vec<&str> = words[at.saturating_sub(2)..at]
                .iter()
                .map(String::as_str)
                .collect();
            let defined = before.last() == Some(&"def") || before == ["def", "self"];
            if !defined {
                names.push(name.clone());
            }
        }
    }
    names
}

/// A script's lines of code: no comment, and no heredoc's text, which is
/// prose (a `--help` message) far more often than Ruby.
#[expect(
    clippy::string_slice,
    reason = "offsets are where an ASCII token was found, or just past it"
)]
fn script_code(text: &str) -> Vec<&str> {
    let mut code = Vec::new();
    let mut heredoc: Option<String> = None;
    for line in text.lines() {
        if let Some(end) = &heredoc {
            if line.trim() == end {
                heredoc = None;
            }
            continue;
        }
        let line = uncommented(line);
        heredoc = line.find("<<").and_then(|at| {
            let tag = line[at + 2..].trim_start_matches(['~', '-']);
            let tag = tag.trim_start_matches(['\'', '"']);
            let end = tag
                .find(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
                .unwrap_or(tag.len());
            (end > 0).then(|| tag[..end].to_string())
        });
        code.push(line);
    }
    code
}

/// A line up to a `#` comment outside its strings (`#{…}` is no comment).
#[expect(
    clippy::string_slice,
    reason = "a byte scan stops only at ASCII bytes, which are char boundaries"
)]
fn uncommented(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut quote = None;
    let mut at = 0;
    while at < bytes.len() {
        match (quote, bytes[at]) {
            (Some(_), b'\\') => at += 1,
            (Some(q), b) if b == q => quote = None,
            (None, b @ (b'"' | b'\'')) => quote = Some(b),
            (None, b'#') if bytes.get(at + 1) != Some(&b'{') => return &line[..at],
            _ => {}
        }
        at += 1;
    }
    line
}

/// The constant paths in a line of a template's Ruby, outside its strings:
/// `Widget`, `Admin::Widget::LIMIT`, `::Widget`.
#[expect(
    clippy::string_slice,
    reason = "a byte scan stops only at ASCII bytes, which are char boundaries"
)]
pub(super) fn constant_paths(ruby: &str) -> Vec<String> {
    let mut found = Vec::new();
    let bytes = ruby.as_bytes();
    let mut quote = None;
    let mut at = 0;
    while at < bytes.len() {
        let b = bytes[at];
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) if b == b'\\' => at += 1,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => quote = Some(b),
            None if b.is_ascii_uppercase()
                && (at == 0
                    || !(bytes[at - 1].is_ascii_alphanumeric()
                        || bytes[at - 1] == b'_'
                        || bytes[at - 1] == b'.'
                        || bytes[at - 1] == b'@')) =>
            {
                // `x::Widget` is read on a value, not from the top level.
                let rooted = ruby[..at].ends_with("::");
                if rooted
                    && at > 2
                    && (bytes[at - 3].is_ascii_alphanumeric() || b"_)]".contains(&bytes[at - 3]))
                {
                    at += 1;
                    continue;
                }
                let start = if rooted { at - 2 } else { at };
                let mut end = at;
                loop {
                    while end < bytes.len()
                        && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_')
                    {
                        end += 1;
                    }
                    if ruby[end..].starts_with("::")
                        && bytes.get(end + 2).is_some_and(u8::is_ascii_uppercase)
                    {
                        end += 2;
                        continue;
                    }
                    break;
                }
                found.push(ruby[start..end].to_string());
                at = end;
                continue;
            }
            None => {}
        }
        at += 1;
    }
    found
}

/// A template's text, decoded as `--dead Gemfile` decodes it: a byte that is
/// not UTF-8 reads as U+FFFD rather than leave the view unread. None for a
/// file holding a NUL, which is no template a view renders but a binary with
/// its extension.
fn template_text(bytes: &[u8]) -> Option<std::borrow::Cow<'_, str>> {
    (!bytes.contains(&0)).then(|| String::from_utf8_lossy(bytes))
}

/// The identifiers in some Ruby, with a trailing `?` or `!`. A string's
/// text is no name — `t(".edit")` is an i18n key — but a double-quoted
/// string's `#{…}` is Ruby.
#[expect(clippy::string_slice, reason = "offsets come from char_indices")]
fn words(ruby: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut chars = ruby.char_indices().peekable();
    while let Some((start, c)) = chars.next() {
        if c == '\'' || c == '"' {
            let mut code = String::new();
            let mut depth = 0;
            while let Some((_, next)) = chars.next() {
                match next {
                    '\\' => {
                        chars.next();
                    }
                    '{' if depth > 0 => depth += 1,
                    '}' if depth > 0 => {
                        depth -= 1;
                        if depth == 0 {
                            code.push('\n');
                        }
                    }
                    '#' if c == '"'
                        && depth == 0
                        && chars.peek().is_some_and(|(_, n)| *n == '{') =>
                    {
                        chars.next();
                        depth = 1;
                        continue;
                    }
                    quote if quote == c && depth == 0 => break,
                    _ => {}
                }
                if depth > 0 {
                    code.push(next);
                }
            }
            found.extend(words(&code));
            continue;
        }
        if !is_ident(c) || c.is_ascii_digit() {
            continue;
        }
        let mut end = start + c.len_utf8();
        while let Some(&(at, next)) = chars.peek() {
            if is_ident(next) {
                end = at + next.len_utf8();
                chars.next();
            } else {
                if next == '?' || next == '!' {
                    end = at + 1;
                    chars.next();
                }
                break;
            }
        }
        found.push(ruby[start..end].to_string());
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_strings_text_is_no_name_but_its_interpolation_is() {
        let names = words(r#"t('.edit') + "by #{account.display_name}""#);
        assert!(names.iter().any(|n| n == "display_name"));
        assert!(!names.iter().any(|n| n == "edit" || n == "by"));
    }

    #[test]
    fn a_template_writes_constant_paths_outside_its_strings() {
        let found =
            constant_paths("Widget::LIMIT.each { |x| t('Gadget') } + ::Alpha + w.Beta + w::Gamma");
        assert_eq!(found, ["Widget::LIMIT", "::Alpha"]);
    }

    #[test]
    fn a_scripts_comments_and_heredocs_name_nothing() {
        let text = "#!/usr/bin/env ruby\n# prints usage\nwarn <<~TEXT\n  usage here\nTEXT\nputs cli.banner # then usage\nlist << \"#{cli.run}\"\ndef self.setup; end\ndef initialize; end\n";
        assert!(ruby_script("exe/tool", text));
        assert!(!ruby_script("exe/tool.sh", text));
        assert!(!ruby_script("bin/setup", "#!/bin/sh\necho\n"));
        let names = script_names(text);
        for name in ["banner", "run", "warn", "list"] {
            assert!(names.iter().any(|n| n == name), "{name}");
        }
        for name in ["usage", "prints", "here", "env", "setup", "initialize"] {
            assert!(!names.iter().any(|n| n == name), "{name}");
        }
    }

    #[test]
    fn a_binary_file_is_no_template() {
        assert!(template_text(b"<%= w.title %>").is_some());
        assert_eq!(
            template_text(b"= w.caf\xe9 + w.title").as_deref(),
            Some("= w.caf\u{fffd} + w.title"),
            "a Latin-1 byte leaves the rest of the view read"
        );
        assert!(template_text(b"<%= w.title %>\0").is_none());
    }
}
