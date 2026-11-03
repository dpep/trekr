//! The names a checkout's view templates write in their Ruby: what `--dead`
//! cannot see calls from, and says so per row (DEC-315).
//!
//! Not a reading of the templates. A name here is a word that appears where
//! a template runs Ruby — a Slim line, a Jbuilder file — with no receiver
//! typed and no scope known, which is evidence enough for a caveat and never
//! for a caller. ERB, Haml and RABL templates are not among them: the index
//! reads those, and their calls are call sites (DEC-520, DEC-658).

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

/// Template extensions whose Ruby a view runs, and the index does not read.
const TEMPLATES: [&str; 3] = ["slim", "jbuilder", "builder"];

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
    /// Every template git knows in the checkout at `root`. None when git
    /// cannot list them, which is also the answer for a checkout with none.
    pub(super) fn read(root: &Path) -> Views {
        let mut args = vec!["ls-files".to_string(), "-z".to_string(), "--".to_string()];
        args.extend(TEMPLATES.iter().map(|ext| format!("*.{ext}")));
        let Ok(out) = Command::new("git").arg("-C").arg(root).args(&args).output() else {
            return Views::default();
        };
        let mut views = Views::default();
        for path in out.stdout.split(|b| *b == 0).filter(|p| !p.is_empty()) {
            let path = String::from_utf8_lossy(path).into_owned();
            let Ok(bytes) = crate::scan::read_source(root.join(&path)) else {
                continue;
            };
            let Some(text) = template_text(&bytes) else {
                continue;
            };
            let ruby = if path.ends_with(".slim") {
                indented_ruby(text)
            } else {
                text.to_string()
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

/// A template's text: none for a file that is not UTF-8 or holds a NUL,
/// which is no template a view renders but a binary with its extension.
fn template_text(bytes: &[u8]) -> Option<&str> {
    std::str::from_utf8(bytes)
        .ok()
        .filter(|text| !text.contains('\0'))
}

/// The Ruby a Slim template runs: a line that starts with `-` or `=`, what
/// follows a tag's `=`, `{` or `(`, every `#{…}`, and the lines a Ruby line
/// ending in a comma continues onto. Not a `-#` comment (Haml's, from when
/// this read Haml too).
fn indented_ruby(text: &str) -> String {
    let mut ruby = String::new();
    let mut continued = false;
    for line in text.lines() {
        let line = line.trim_start();
        if std::mem::take(&mut continued) {
            ruby.push_str(line);
            ruby.push('\n');
            continued = line.trim_end().ends_with(',');
            continue;
        }
        if line.starts_with("-#") {
            continue;
        }
        let mut rest = line;
        while let Some(open) = rest.find("#{") {
            let inner = &rest[open + 2..];
            let close = inner.find('}').unwrap_or(inner.len());
            ruby.push_str(&inner[..close]);
            ruby.push('\n');
            rest = &inner[close..];
        }
        if line.starts_with(['-', '=', '~', '!', '&']) {
            ruby.push_str(line);
            ruby.push('\n');
            continued = line.trim_end().ends_with(',');
            continue;
        }
        let tag = line
            .find(|c: char| !(c.is_ascii_alphanumeric() || "%.#_-:".contains(c)))
            .unwrap_or(line.len());
        let after = line[tag..].trim_start();
        if tag > 0 && after.starts_with(['=', '{', '(', '[']) {
            ruby.push_str(after);
            ruby.push('\n');
            continued = after.trim_end().ends_with(',');
        }
    }
    ruby
}

/// The identifiers in some Ruby, with a trailing `?` or `!`. A string's
/// text is no name — `t(".edit")` is an i18n key — but a double-quoted
/// string's `#{…}` is Ruby.
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
        if !(c.is_ascii_alphabetic() || c == '_') {
            continue;
        }
        let mut end = start + c.len_utf8();
        while let Some(&(at, next)) = chars.peek() {
            if next.is_ascii_alphanumeric() || next == '_' {
                end = at + 1;
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
        assert!(template_text(b"<%= w.\xff\xfe %>").is_none());
        assert!(template_text(b"<%= w.title %>\0").is_none());
    }

    #[test]
    fn a_slim_template_names_what_its_ruby_lines_run() {
        let ruby = indented_ruby("h1= w.title\np lonely words\n- if w.shown?\na{ href: w.link }\n");
        let names = words(&ruby);
        for name in ["title", "shown?", "link"] {
            assert!(names.iter().any(|n| n == name), "{name}");
        }
        assert!(!names.iter().any(|n| n == "lonely"));
    }
}
