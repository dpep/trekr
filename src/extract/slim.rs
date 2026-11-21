//! A Slim view's Ruby, line for line (DEC-808, DEC-815).
//!
//! Unlike `template::erb_ruby` and `haml::haml_ruby`, nothing parses the
//! result: its readers take its words, so it keeps each line's Ruby on that
//! line rather than every byte at its offset.

// Scans Ruby as text: a str slice must say why its offset is a char boundary (DEC-835).
#![deny(clippy::string_slice)]

/// What a Slim view's line holds below it: Ruby (`ruby:`), text whose
/// `#{}` is Ruby (`|`, `javascript:`), or a comment (`/`).
#[derive(Clone, Copy, PartialEq)]
enum SlimBlock {
    Ruby,
    Text,
    Comment,
}

/// A Slim view's Ruby, line for line: what follows `-`, `=` or `==`, a
/// tag's `=` output, an attribute's unquoted value, a `ruby:` block, and
/// each `#{…}`. Text, a `/` comment and a quoted attribute are blank: prose
/// naming `Diffy`, or a URL holding `Tomlrb`, is no use of a gem.
pub(crate) fn slim_ruby(text: &str) -> String {
    let mut out = Vec::new();
    let mut block: Option<(usize, SlimBlock)> = None;
    let mut continued = false;
    for line in text.lines() {
        let indent = line.len() - line.trim_start().len();
        let body = line.trim();
        if let Some((at, kind)) = block {
            if body.is_empty() || indent > at {
                out.push(match kind {
                    SlimBlock::Ruby => body.to_string(),
                    SlimBlock::Text => interpolations(body),
                    SlimBlock::Comment => String::new(),
                });
                continue;
            }
            block = None;
        }
        let ruby = if continued {
            body.to_string()
        } else {
            let (ruby, opens) = slim_line(body);
            block = opens.map(|kind| (indent, kind));
            ruby
        };
        // A line of code ending in `,` or `\` goes on below.
        continued = ruby.ends_with(',') || ruby.ends_with('\\');
        out.push(ruby);
    }
    out.join("\n")
}

/// One Slim line's Ruby, and the block it opens, split as Slim 5's parser
/// splits it (`parse_line_indicators`).
#[expect(
    clippy::string_slice,
    reason = "past a first character matched as ASCII"
)]
fn slim_line(body: &str) -> (String, Option<SlimBlock>) {
    // An embedded engine, its first line written on its own: `ruby: x = 1`.
    if let Some((engine, first)) = body.split_once(':')
        && SLIM_ENGINES.contains(&engine)
    {
        let first = first.trim();
        return match engine {
            "ruby" => (first.to_string(), Some(SlimBlock::Ruby)),
            _ => (interpolations(first), Some(SlimBlock::Text)),
        };
    }
    match body.chars().next() {
        None => (String::new(), None),
        // A conditional comment's children are Slim.
        Some('/') if body.starts_with("/[") && body.ends_with(']') => (String::new(), None),
        Some('/') if body.starts_with("/!") => (interpolations(body), Some(SlimBlock::Text)),
        Some('/') => (String::new(), Some(SlimBlock::Comment)),
        Some('|' | '\'') => (interpolations(&body[1..]), Some(SlimBlock::Text)),
        Some('-' | '=') => (
            body.trim_start_matches(['-', '=', '~', '\'', '<', '>'])
                .to_string(),
            None,
        ),
        Some('<') => (interpolations(body), None),
        _ if body.starts_with("doctype") => (String::new(), None),
        _ => (slim_tag(body), None),
    }
}

/// The engines Slim 5 embeds (`Slim::Embedded.register`).
const SLIM_ENGINES: [&str; 10] = [
    "ruby",
    "javascript",
    "css",
    "markdown",
    "textile",
    "rdoc",
    "coffee",
    "less",
    "sass",
    "scss",
];

/// The length of what starts `text` as Slim's tag name does: word
/// characters, with `:` and `-` only between them (`svg:path`, `my-tag`).
fn slim_tag_name(text: &str) -> usize {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let mut end = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        let joins = matches!(c, ':' | '-') && chars.peek().is_some_and(|(_, next)| word(*next));
        if !word(c) && !(joins && end > 0) {
            break;
        }
        end = at + c.len_utf8();
    }
    end
}

/// The length of an attribute's name at the start of `text`, as Slim reads
/// one: anything up to a space, a quote, `<`, `>`, `/`, `=` or a bracket.
fn slim_attr_name(text: &str) -> usize {
    text.find(|c: char| c.is_whitespace() || "\"'<>/=()[]{}".contains(c))
        .unwrap_or(text.len())
}

/// A tag line's Ruby: `p = x` outputs `x`; `a href=url_for(x)` computes an
/// attribute; `.card*x.attrs` splats one; `a href="…#{x}"` interpolates;
/// text after the tag does not.
#[expect(
    clippy::string_slice,
    reason = "lengths are counted by char_indices or found by find"
)]
fn slim_tag(line: &str) -> String {
    let mut ruby = Vec::new();
    let mut rest = &line[slim_tag_name(line)..];
    // `.class` and `#id` shortcuts: `.md:flex`, `.w-1/2`.
    while let Some(after) = rest.strip_prefix(['.', '#']) {
        let mut end = 0;
        let mut chars = after.char_indices().peekable();
        while let Some((at, c)) = chars.next() {
            let word = |c: char| c.is_alphanumeric() || c == '_' || c == '-';
            let joins = match c {
                ':' => chars.peek().is_some_and(|(_, next)| word(*next)),
                '/' => chars.peek().is_some_and(|(_, next)| next.is_ascii_digit()),
                _ => word(c),
            };
            if !joins {
                break;
            }
            end = at + c.len_utf8();
        }
        rest = &after[end..];
    }
    // Whitespace modifiers: `span> = x`, `a<> href=x`.
    rest = rest.trim_start_matches(['<', '>', '\'']);
    let mut wrapped = false;
    loop {
        rest = rest.trim_start();
        if let Some(inline) = rest.strip_prefix(": ") {
            ruby.push(slim_tag(inline.trim_start()));
            break;
        }
        if rest.starts_with('=') {
            ruby.push(rest.trim_start_matches(['=', '\'', '<', '>']).to_string());
            break;
        }
        if let Some(after) = rest.strip_prefix(['(', '[', '{']) {
            wrapped = true;
            rest = after;
            continue;
        }
        if let Some(after) = rest.strip_prefix([')', ']', '}']).filter(|_| wrapped) {
            wrapped = false;
            rest = after;
            continue;
        }
        // A splat: `*attrs_for(x)`.
        if let Some(splat) = rest
            .strip_prefix('*')
            .filter(|s| s.starts_with(|c: char| !c.is_whitespace()))
        {
            let (written, after) = splat.split_at(value_end(splat));
            ruby.push(written.to_string());
            rest = after;
            continue;
        }
        let name_len = slim_attr_name(rest);
        if name_len == 0 {
            ruby.push(interpolations(rest));
            break;
        }
        let after_name = &rest[name_len..];
        // Slim allows spaces around an attribute's `=`: `a href = x`.
        let Some(value) = after_name.trim_start().strip_prefix('=') else {
            if wrapped {
                rest = after_name;
                continue;
            }
            // Text after the tag.
            ruby.push(interpolations(rest));
            break;
        };
        let value = value.strip_prefix('=').unwrap_or(value).trim_start();
        let end = value_end(value);
        let (written, after) = value.split_at(end);
        match written.chars().next() {
            Some(q @ ('"' | '\'')) => ruby.push(interpolations(written.trim_matches(q))),
            _ => ruby.push(written.to_string()),
        }
        rest = after;
    }
    ruby.retain(|r| !r.is_empty());
    ruby.join(" ")
}

/// Where an attribute's value ends: past its closing quote, or at the first
/// space outside brackets.
fn value_end(value: &str) -> usize {
    let mut chars = value.char_indices();
    if let Some((_, q @ ('"' | '\''))) = chars.next() {
        let mut escaped = false;
        for (at, c) in chars {
            match c {
                '\\' if !escaped => escaped = true,
                c if c == q && !escaped => return at + 1,
                _ => escaped = false,
            }
        }
        return value.len();
    }
    let mut depth = 0usize;
    for (at, c) in value.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' if depth == 0 => return at,
            ')' | ']' | '}' => depth -= 1,
            c if c.is_whitespace() && depth == 0 => return at,
            _ => {}
        }
    }
    value.len()
}

/// Each `#{…}` in text, its Ruby joined by spaces.
#[expect(
    clippy::string_slice,
    reason = "offsets are a found token's end, or from char_indices"
)]
fn interpolations(text: &str) -> String {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("#{") {
        let inner = &rest[start + 2..];
        let mut depth = 1usize;
        let end = inner
            .char_indices()
            .find(|(_, c)| {
                match c {
                    '{' => depth += 1,
                    '}' => depth -= 1,
                    _ => {}
                }
                depth == 0
            })
            .map_or(inner.len(), |(at, _)| at);
        found.push(&inner[..end]);
        rest = &inner[end.min(inner.len())..];
    }
    found.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slim_views_ruby_is_its_code_not_its_text() {
        let view = "/ we used to render with Diffy here\n\
                    h1 Welcome\n\
                    p Our deploys are Retriable and safe.\n\
                    a href=\"https://example.com/Tomlrb-docs\" docs\n\
                    p = Widget.render\n\
                    - if Pager.on?\n\
                    a href=url_for(Linker.path) title=\"#{Titler.name}\" go\n\
                    li: a = Inline.name\n\
                    | text #{Piped.word} more\n\
                    ruby:\n  Embedded.run\n\
                    p done\n";
        let ruby = slim_ruby(view);
        assert_eq!(ruby.split('\n').count(), view.lines().count());
        for used in [
            "Widget", "Pager", "Linker", "Titler", "Inline", "Piped", "Embedded",
        ] {
            assert!(ruby.contains(used), "{used}: {ruby}");
        }
        for prose in ["Diffy", "Retriable", "Tomlrb", "Welcome", "done"] {
            assert!(!ruby.contains(prose), "{prose}: {ruby}");
        }
    }

    #[test]
    fn a_slim_line_is_split_as_slim_5_splits_it() {
        let view = "span> = Modifier.out\n\
                    a<> href=Both.path go\n\
                    .card*Splat.attrs\n\
                    /[if lt IE 9]\n  = Conditional.run\n\
                    svg:path d=Spaced.path\n\
                    a href = Around.path Text\n\
                    a(title = Wrapped.name) Dur\n\
                    .md:flex.w-1/2 = Tailwind.name\n\
                    ruby: x = OneLine.run\n\
                    javascript: var x = \"#{Engine.name}\";\n\
                    /! html comment #{Shown.name}\n\
                    p Prose Text here\n";
        let ruby = slim_ruby(view);
        assert_eq!(ruby.split('\n').count(), view.lines().count());
        for used in [
            "Modifier",
            "Both",
            "Splat",
            "Conditional",
            "Spaced",
            "Around",
            "Wrapped",
            "Tailwind",
            "OneLine",
            "Engine",
            "Shown",
        ] {
            assert!(ruby.contains(used), "{used}: {ruby}");
        }
        for prose in ["Text", "Dur", "Prose", "html"] {
            assert!(!ruby.contains(prose), "{prose}: {ruby}");
        }
    }
}
