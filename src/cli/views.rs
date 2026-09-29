//! The names a checkout's view templates write in their Ruby: what `--dead`
//! cannot see calls from, and says so per row (DEC-315).
//!
//! Not a reading of the templates. A name here is a word that appears where
//! a template runs Ruby — an ERB tag, a Haml or Slim line, a Jbuilder file —
//! with no receiver typed and no scope known, which is evidence enough for a
//! caveat and never for a caller.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

/// Template extensions whose Ruby a view runs.
const TEMPLATES: [&str; 6] = ["erb", "haml", "slim", "jbuilder", "rabl", "builder"];

#[derive(Default)]
pub(super) struct Views {
    /// Each name, with the first template (relative to the checkout) that
    /// writes it.
    names: HashMap<String, String>,
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
            let Ok(bytes) = std::fs::read(root.join(&path)) else {
                continue;
            };
            let text = String::from_utf8_lossy(&bytes);
            let ruby = if path.ends_with(".erb") {
                erb_ruby(&text)
            } else if path.ends_with(".haml") || path.ends_with(".slim") {
                indented_ruby(&text)
            } else {
                text.into_owned()
            };
            for name in words(&ruby) {
                views.names.entry(name).or_insert_with(|| path.clone());
            }
        }
        views
    }

    /// The first template that writes `name`, if one does.
    pub(super) fn naming(&self, name: &str) -> Option<&str> {
        self.names.get(name).map(String::as_str)
    }
}

/// The Ruby an ERB template runs: the insides of its `<% %>` tags, less the
/// `<%#` comments.
fn erb_ruby(text: &str) -> String {
    let mut ruby = String::new();
    let mut rest = text;
    while let Some(open) = rest.find("<%") {
        let after = &rest[open + 2..];
        let Some(close) = after.find("%>") else {
            break;
        };
        if !after.starts_with('#') {
            ruby.push_str(&after[..close]);
            ruby.push('\n');
        }
        rest = &after[close + 2..];
    }
    ruby
}

/// The Ruby a Haml or Slim template runs: a line that starts with `-` or
/// `=`, what follows a tag's `=`, `{` or `(`, and every `#{…}`.
fn indented_ruby(text: &str) -> String {
    let mut ruby = String::new();
    for line in text.lines() {
        let line = line.trim_start();
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
            continue;
        }
        let tag = line
            .find(|c: char| !(c.is_ascii_alphanumeric() || "%.#_-:".contains(c)))
            .unwrap_or(line.len());
        let after = line[tag..].trim_start();
        if tag > 0 && after.starts_with(['=', '{', '(', '[']) {
            ruby.push_str(after);
            ruby.push('\n');
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
    fn an_erb_template_names_only_what_its_tags_run() {
        let ruby = erb_ruby("<p>quiet</p><%= @w.title %><%# lonely %><% if w.shown? %>");
        let names = words(&ruby);
        assert!(names.contains(&"title".to_string()));
        assert!(names.contains(&"shown?".to_string()));
        assert!(!names.iter().any(|n| n == "quiet" || n == "lonely"));
    }

    #[test]
    fn a_strings_text_is_no_name_but_its_interpolation_is() {
        let names = words(r#"t('.edit') + "by #{account.display_name}""#);
        assert!(names.iter().any(|n| n == "display_name"));
        assert!(!names.iter().any(|n| n == "edit" || n == "by"));
    }

    #[test]
    fn a_haml_template_names_what_its_ruby_lines_run() {
        let ruby =
            indented_ruby("%h1= w.title\n%p lonely words\n- if w.shown?\n%a{ href: w.link }\n");
        let names = words(&ruby);
        for name in ["title", "shown?", "link"] {
            assert!(names.iter().any(|n| n == name), "{name}");
        }
        assert!(!names.iter().any(|n| n == "lonely"));
    }
}
