//! What a definition says about itself: the comment written above it, and its
//! signature as written.
//!
//! Read from the definition's file when a hover asks, not stored (DEC-052).
//! Everything here is a pure function of that file's text and one line
//! number, so the caller decides which text is current and which line the
//! definition is on — and is the one who must refuse when it cannot be sure.

use crate::core::{Def, Kind, ParamKind};

/// A doc comment, cut down to what a hover has room for.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Doc {
    /// The first paragraph of prose, comment markers stripped, inline markup
    /// as written.
    pub(crate) summary: String,
    /// YARD's `@return`, as written after the tag: `[Type] description`.
    pub(crate) returns: Option<String>,
    /// YARD's `@deprecated`, as written after the tag — possibly empty.
    pub(crate) deprecated: Option<String>,
}

/// A hover is a glance, not the documentation. The rest is one click away, at
/// the definition the hover links to.
const SUMMARY_LINES: usize = 6;
const SUMMARY_CHARS: usize = 400;
const TAG_CHARS: usize = 160;
/// How far above an `end` to look for the `sig do` that opens it.
const SIG_LINES: usize = 40;

/// The doc comment for the definition on `line` (1-based): the contiguous
/// comment block directly above it, or above the Sorbet `sig` directly above
/// it. A blank line ends the block — a comment separated from a definition
/// is about something else.
pub(crate) fn doc_above(text: &str, line: u32) -> Option<Doc> {
    // Nothing below the definition is read, and a gem file can be long.
    let lines: Vec<&str> = text.lines().take(line as usize).collect();
    let at = (line as usize).checked_sub(1)?;
    if at >= lines.len() {
        return None;
    }
    let start = statement_start(&lines, at);
    summarize(&comment_block(&lines, start)?)
}

/// Step up over a `sig` written above a definition, which belongs to it.
fn statement_start(lines: &[&str], mut at: usize) -> usize {
    loop {
        let Some(above) = at.checked_sub(1) else {
            return at;
        };
        let written = lines[above].trim();
        if is_sig(written) && !written.ends_with(" do") && written != "sig" {
            at = above; // one line: `sig { … }`
            continue;
        }
        if written == "end" || written == "}" {
            let indent = indent_of(lines[above]);
            match opener(lines, above, indent) {
                Some(open) => {
                    at = open;
                    continue;
                }
                None => return at,
            }
        }
        return at;
    }
}

fn is_sig(written: &str) -> bool {
    written == "sig"
        || ["sig {", "sig{", "sig do", "sig("]
            .iter()
            .any(|p| written.starts_with(p))
}

/// The `sig` that an `end` or `}` at this indentation closes, if that is what
/// it closes. Anything else at the same indentation first means it is not.
fn opener(lines: &[&str], close: usize, indent: usize) -> Option<usize> {
    for i in (close.saturating_sub(SIG_LINES)..close).rev() {
        if lines[i].trim().is_empty() {
            continue;
        }
        match indent_of(lines[i]) {
            n if n > indent => continue,
            n if n == indent && is_sig(lines[i].trim()) => return Some(i),
            _ => return None,
        }
    }
    None
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// The comment lines ending directly above `start`, markers stripped, top to
/// bottom. An `=begin`/`=end` block counts when its `=end` is what is above.
fn comment_block(lines: &[&str], start: usize) -> Option<Vec<String>> {
    let last = start.checked_sub(1)?;
    if is_directive_line(lines[last], "=end") {
        let open = (0..last)
            .rev()
            .find(|&i| is_directive_line(lines[i], "=begin"))?;
        return Some(
            lines[open + 1..last]
                .iter()
                .map(|l| l.to_string())
                .collect(),
        );
    }
    let mut block: Vec<String> = lines[..=last]
        .iter()
        .rev()
        .map(|l| l.trim_start())
        .take_while(|l| l.starts_with('#'))
        .map(strip_marker)
        .collect();
    block.reverse();
    (!block.is_empty()).then_some(block)
}

/// `=begin` and `=end` only count at the start of a line, and may carry text.
fn is_directive_line(line: &str, word: &str) -> bool {
    line.strip_prefix(word)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
}

/// `# text` → `text`. YARD's `##` opener is a marker too; indentation past
/// the first space is kept, because it is what marks a code example or a
/// tag's continuation.
fn strip_marker(line: &str) -> String {
    let rest = line.trim_start_matches('#');
    // `#!` is a shebang, not prose; keep its `!` so the filter can see it.
    rest.strip_prefix(' ').unwrap_or(rest).to_string()
}

/// What the comment said, minus what was never meant as documentation.
fn summarize(raw: &[String]) -> Option<Doc> {
    let lines = prose_lines(raw)?;
    let mut doc = Doc::default();
    let mut prose: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = &lines[i];
        let written = line.trim_start();
        if let Some(tag) = written.strip_prefix('@') {
            // A tag runs on through the lines indented past it.
            let indent = indent_of(line);
            let mut body = vec![tag];
            i += 1;
            while i < lines.len() && !lines[i].trim().is_empty() && indent_of(&lines[i]) > indent {
                body.push(lines[i].trim());
                i += 1;
            }
            let (name, text) = tag.split_once(char::is_whitespace).unwrap_or((tag, ""));
            let rest: Vec<&str> = std::iter::once(text)
                .chain(body[1..].iter().copied())
                .collect();
            let text = collapse(&rest.join(" "));
            match name {
                "return" if doc.returns.is_none() => doc.returns = Some(cap(&text, TAG_CHARS)),
                "deprecated" if doc.deprecated.is_none() => {
                    doc.deprecated = Some(cap(&text, TAG_CHARS))
                }
                _ => {}
            }
            // A tag ends the prose before it: YARD's summary is what precedes
            // the first one.
            prose.push("");
            continue;
        }
        prose.push(line);
        i += 1;
    }
    doc.summary = first_paragraph(&prose);
    let empty = doc.summary.is_empty() && doc.returns.is_none() && doc.deprecated.is_none();
    (!empty).then_some(doc)
}

/// The comment with directives, magic comments and RDoc's hidden sections
/// removed — or `None` when the comment itself says there is no doc.
fn prose_lines(raw: &[String]) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut hidden = false;
    let mut skipping_call_seq = false;
    for line in raw {
        let written = line.trim();
        // RDoc: `#--` hides everything until `#++`.
        if written == "--" {
            hidden = true;
            continue;
        }
        if written == "++" {
            hidden = false;
            continue;
        }
        if hidden {
            continue;
        }
        if skipping_call_seq {
            if written.is_empty() {
                skipping_call_seq = false;
            } else {
                continue;
            }
        }
        if written.starts_with(":nodoc:") || written.starts_with(":stopdoc:") {
            return None;
        }
        if written.starts_with(":call-seq:") {
            // The signature is already shown, as written.
            skipping_call_seq = true;
            continue;
        }
        if is_directive(written) {
            continue;
        }
        if is_rule(written) {
            out.push(String::new());
            continue;
        }
        out.push(line.clone());
    }
    Some(out)
}

/// A comment addressed to a tool rather than a reader.
fn is_directive(written: &str) -> bool {
    let lower = written.to_ascii_lowercase();
    const MAGIC: [&str; 7] = [
        "frozen_string_literal:",
        "encoding:",
        "coding:",
        "warn_indent:",
        "warn_past_scope:",
        "shareable_constant_value:",
        "typed:",
    ];
    const TOOLS: [&str; 6] = [
        "rubocop:",
        "rubocop :",
        "standard:",
        "steep:",
        "reek:",
        "simplecov",
    ];
    MAGIC.iter().chain(TOOLS.iter()).any(|p| lower.starts_with(p))
        || lower.starts_with("-*-")
        // A shebang, kept as `!/…` by `strip_marker`.
        || lower.starts_with("!/")
        // RDoc directives (`:startdoc:`, `:yields:`) and rbs-inline's `#:`.
        // YARD's `@!directive` and rbs-inline's `@rbs` are tags, and dropped
        // with their continuation lines as unkept tags are.
        || lower.starts_with(':')
}

/// `# ----------` and `# ==========`: a visual separator, read as a break.
fn is_rule(written: &str) -> bool {
    written.len() >= 3 && written.chars().all(|c| "-=*#_~".contains(c))
}

/// The first paragraph: headings skipped, dedented, capped.
fn first_paragraph(prose: &[&str]) -> String {
    let body: Vec<&str> = prose
        .iter()
        .copied()
        .skip_while(|l| l.trim().is_empty() || is_heading(l.trim()))
        .take_while(|l| !l.trim().is_empty())
        .collect();
    let indent = body.iter().map(|l| indent_of(l)).min().unwrap_or(0);
    let lines: Vec<&str> = body.iter().map(|l| &l[indent..]).collect();
    let cut = lines.len() > SUMMARY_LINES;
    let joined = lines[..lines.len().min(SUMMARY_LINES)].join("\n");
    let capped = cap(joined.trim_end(), SUMMARY_CHARS);
    if cut && !capped.ends_with('…') {
        format!("{capped} …")
    } else {
        capped
    }
}

/// RDoc's `= Title` and Markdown's `# Title` (whose `#` the marker strip
/// already took one of).
fn is_heading(written: &str) -> bool {
    written.starts_with("= ") || written.starts_with("==") || written.starts_with('#')
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// At most `max` bytes, cut at a word boundary, marked when cut.
fn cap(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let head = &text[..end];
    let head = head.rfind(char::is_whitespace).map_or(head, |i| &head[..i]);
    format!("{} …", head.trim_end())
}

impl Doc {
    /// As Markdown: deprecation first, because it changes what to do; then
    /// the summary; then what it returns, which a signature cannot say.
    pub(crate) fn markdown(&self) -> String {
        let mut parts = Vec::new();
        if let Some(why) = &self.deprecated {
            parts.push(match why.is_empty() {
                true => "**Deprecated.**".to_string(),
                false => format!("**Deprecated.** {}", inline(why)),
            });
        }
        if !self.summary.is_empty() {
            parts.push(inline(&self.summary));
        }
        if let Some(returns) = &self.returns
            && let Some(line) = returns_line(returns)
        {
            parts.push(line);
        }
        parts.join("\n\n")
    }
}

/// `[String, nil] the name` → **Returns** `String, nil` — the name. `void`
/// says nothing a reader wants, so it is not said.
fn returns_line(written: &str) -> Option<String> {
    let (types, rest) = match written.strip_prefix('[') {
        Some(after) => match after.split_once(']') {
            Some((types, rest)) => (Some(types.trim()), rest.trim()),
            None => (None, written),
        },
        None => (None, written),
    };
    if types == Some("void") {
        return None;
    }
    Some(match (types, rest.is_empty()) {
        (Some(t), true) => format!("**Returns** `{t}`"),
        (Some(t), false) => format!("**Returns** `{t}` — {}", inline(rest)),
        (None, false) => format!("**Returns** {}", inline(rest)),
        (None, true) => return None,
    })
}

/// RDoc and YARD inline markup as Markdown: `+x+`, `<tt>x</tt>` and `{X#y}`
/// become code; `<b>`/`<i>` become emphasis. A stray `<` is escaped outside
/// code, or `Array<String>` renders as an unknown HTML tag and vanishes.
fn inline(text: &str) -> String {
    let mut text = text.to_string();
    for (open, close, md) in [
        ("<tt>", "</tt>", "`"),
        ("<code>", "</code>", "`"),
        ("<b>", "</b>", "**"),
        ("<strong>", "</strong>", "**"),
        ("<i>", "</i>", "*"),
        ("<em>", "</em>", "*"),
    ] {
        text = text.replace(open, md).replace(close, md);
    }
    let mut out = String::with_capacity(text.len());
    let mut in_code = false;
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '`' {
            in_code = !in_code;
            out.push(c);
            i += 1;
            continue;
        }
        if !in_code {
            if let Some((code, next)) = delimited(&chars, i, '+', '+', is_rdoc_code)
                .or_else(|| delimited(&chars, i, '{', '}', is_yard_ref))
            {
                out.push('`');
                out.push_str(&code);
                out.push('`');
                i = next;
                continue;
            }
            if c == '<' {
                out.push_str("\\<");
                i += 1;
                continue;
            }
            // RDoc's `\Rails` stops a word being auto-linked; Markdown would
            // show the backslash.
            if c == '\\' && chars.get(i + 1).is_some_and(|n| n.is_alphanumeric()) {
                i += 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// `open…close` starting at `i`, when it is word-delimited and its content
/// passes `ok`: the content (first word only, for YARD's `{ref label}`) and
/// the index after `close`.
fn delimited(
    chars: &[char],
    i: usize,
    open: char,
    close: char,
    ok: fn(&str) -> bool,
) -> Option<(String, usize)> {
    if chars[i] != open || (i > 0 && chars[i - 1].is_alphanumeric()) {
        return None;
    }
    let end = (i + 1..chars.len()).find(|&j| chars[j] == close || chars[j] == '\n')?;
    if chars[end] != close || chars.get(end + 1).is_some_and(|c| c.is_alphanumeric()) {
        return None;
    }
    let content: String = chars[i + 1..end].iter().collect();
    let first = content.split_whitespace().next()?.to_string();
    ok(&content).then_some((first, end + 1))
}

/// RDoc's `+code+` holds one word: `+nil+`, `+find_by+`, `+Foo::Bar+`.
fn is_rdoc_code(content: &str) -> bool {
    !content.is_empty()
        && content
            .chars()
            .all(|c| c.is_alphanumeric() || "_:#.?!=[]@$".contains(c))
}

/// YARD's `{Foo#bar}`, `{#bar}`, `{Foo::Bar label}` — a reference, not a hash.
fn is_yard_ref(content: &str) -> bool {
    let first = content.split_whitespace().next().unwrap_or("");
    first.starts_with(|c: char| c.is_ascii_uppercase() || c == '#')
        && first
            .chars()
            .all(|c| c.is_alphanumeric() || "_:#.?!=".contains(c))
}

/// How a method is named in prose: `Owner#name`, `Owner.name`, or bare.
pub(crate) fn method_name(owner: Option<&str>, singleton: bool, name: &str) -> String {
    match owner.filter(|o| !o.is_empty()) {
        Some(owner) => format!("{owner}{}{name}", if singleton { "." } else { "#" }),
        None => name.to_string(),
    }
}

/// The definition's first line as a reader wants it: the qualified name the
/// caller settled on — after `class`/`module`, but a method bare, as Ruby docs
/// name it — then what was written after the name: a method's parameters, a
/// class's superclass, a constant's value.
pub(crate) fn signature(def: &Def, qualified: &str, text: &str) -> String {
    let after = after_name(def, text);
    match def.kind {
        Kind::Method => {
            let written = after.filter(|_| def.via.is_none()).and_then(written_params);
            let params = written.unwrap_or_else(|| params_of(def));
            format!("{qualified}{params}")
        }
        Kind::Class => {
            let parent = after
                .map(|rest| code_part(first_line(rest)).trim())
                .filter(|rest| rest.starts_with('<'))
                .map(|rest| format!(" {}", cap(rest.trim_end_matches(';').trim(), 100)))
                .unwrap_or_default();
            format!("class {qualified}{parent}")
        }
        Kind::Module => format!("module {qualified}"),
        Kind::Constant => {
            let value = after
                .map(|rest| code_part(first_line(rest)).trim())
                .and_then(|rest| rest.strip_prefix('='))
                .map(str::trim)
                .unwrap_or("");
            format!("{qualified} = {}", constant_value(value))
        }
    }
}

/// The source following the definition's name, when the name is where the
/// definition says it is.
fn after_name<'t>(def: &Def, text: &'t str) -> Option<&'t str> {
    let start = line_offset(text, def.pos.line)? + (def.pos.col as usize).checked_sub(1)?;
    let rest = text.get(start..)?;
    rest.strip_prefix(def.name.as_str())
}

fn line_offset(text: &str, line: u32) -> Option<usize> {
    if line == 0 {
        return None;
    }
    if line == 1 {
        return Some(0);
    }
    text.match_indices('\n')
        .nth(line as usize - 2)
        .map(|(i, _)| i + 1)
}

fn first_line(text: &str) -> &str {
    text.split('\n').next().unwrap_or("")
}

/// `(a, b = 1,\n  key: nil)` → `(a, b = 1, key: nil)`: the parenthesized
/// list, comments dropped and whitespace collapsed, capped.
fn written_params(after: &str) -> Option<String> {
    if !after.starts_with('(') {
        return None;
    }
    let mut joined = String::new();
    let mut depth = 0i32;
    for line in after.split('\n').take(12) {
        let line = code_part(line);
        for (i, c) in line.char_indices() {
            match c {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                _ => {}
            }
            if depth == 0 {
                joined.push_str(&line[..=i]);
                let tidy = collapse(&joined).replace("( ", "(").replace(" )", ")");
                return Some(match tidy.len() > 120 {
                    true => format!("{})", cap(tidy.trim_end_matches(')'), 116)),
                    false => tidy,
                });
            }
        }
        joined.push_str(line);
        joined.push(' ');
    }
    None
}

/// Parameters from the extracted facts, for a definition whose list cannot be
/// read as written: a macro's, or one written without parentheses. Defaults
/// are not facts, so an optional one shows as `…`.
fn params_of(def: &Def) -> String {
    if def.params.is_empty() {
        return String::new();
    }
    let each: Vec<String> = def
        .params
        .iter()
        .map(|p| {
            let n = &p.name;
            match p.kind {
                ParamKind::Req | ParamKind::Post => n.clone(),
                ParamKind::Opt => format!("{n} = …"),
                ParamKind::Rest => format!("*{n}"),
                ParamKind::Keyreq => format!("{n}:"),
                ParamKind::Key => format!("{n}: …"),
                ParamKind::Keyrest => format!("**{n}"),
                ParamKind::Block => format!("&{n}"),
                ParamKind::Nokey => "**nil".to_string(),
            }
        })
        .collect();
    format!("({})", each.join(", "))
}

/// A constant's value on its first line, marked `…` when it goes on.
fn constant_value(value: &str) -> String {
    if value.is_empty() {
        return "…".to_string();
    }
    let depth: i32 = value
        .chars()
        .map(|c| match c {
            '(' | '[' | '{' => 1,
            ')' | ']' | '}' => -1,
            _ => 0,
        })
        .sum();
    let continues = depth > 0
        || value.contains("<<")
        || value.ends_with(" do")
        || value.ends_with(['|', ',', '\\', '+', '-', '*', '.', '&', '=']);
    let shown = cap(value, 80);
    match continues && !shown.ends_with('…') {
        true => format!("{shown} …"),
        false => shown,
    }
}

/// A line without its trailing comment: the text before a `#` that is not
/// inside a string. Deliberately simple, as `complete::in_comment_or_string`
/// is — a miss shows a comment in a signature, nothing worse.
fn code_part(line: &str) -> &str {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in line.char_indices() {
        match quote {
            Some(q) => {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == q {
                    quote = None;
                }
            }
            None => match c {
                '"' | '\'' => quote = Some(c),
                '#' => return line[..i].trim_end(),
                _ => {}
            },
        }
    }
    line.trim_end()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The doc for the definition on the last line of `source`.
    fn doc(source: &str) -> Option<Doc> {
        let line = source.trim_end().lines().count() as u32;
        doc_above(source, line)
    }

    fn summary(source: &str) -> String {
        doc(source).map(|d| d.summary).unwrap_or_default()
    }

    #[test]
    fn reads_the_comment_block_directly_above() {
        let source = "# Resizes the widget.\n# Keeps the aspect ratio.\ndef resize\n";
        assert_eq!(
            summary(source),
            "Resizes the widget.\nKeeps the aspect ratio."
        );
    }

    #[test]
    fn a_blank_line_separates_a_comment_from_the_definition() {
        let source = "# About the section below.\n\ndef resize\n";
        assert_eq!(doc(source), None);
    }

    #[test]
    fn keeps_only_the_first_paragraph() {
        let source = "# Saves it.\n#\n# Then does a great deal more,\n# at length.\ndef save\n";
        assert_eq!(summary(source), "Saves it.");
    }

    #[test]
    fn magic_comments_and_tool_directives_are_not_documentation() {
        let source = "# frozen_string_literal: true\n# typed: strict\n# rubocop:disable Metrics/AbcSize\nclass Widget\n";
        assert_eq!(doc(source), None);
        let source = "# rubocop:disable Style/Foo\n# Builds a widget.\n# rubocop:enable Style/Foo\ndef build\n";
        assert_eq!(summary(source), "Builds a widget.");
        let source = "#!/usr/bin/env ruby\n# -*- coding: utf-8 -*-\nmodule Tool\n";
        assert_eq!(doc(source), None);
    }

    #[test]
    fn nodoc_means_no_doc() {
        let source = "# Internal plumbing.\n# :nodoc:\ndef plumb\n";
        assert_eq!(doc(source), None);
    }

    #[test]
    fn rdoc_hidden_sections_and_call_seq_are_skipped() {
        let source = "# :call-seq:\n#   find(id)\n#   find(*ids)\n#\n# Finds records by id.\n#--\n# Internal note.\n#++\ndef find(*args)\n";
        assert_eq!(summary(source), "Finds records by id.");
    }

    #[test]
    fn a_heading_is_not_the_summary() {
        let source = "# = Active Widget\n#\n# Widgets that know how to save.\nclass Widget\n";
        assert_eq!(summary(source), "Widgets that know how to save.");
    }

    #[test]
    fn yard_tags_end_the_summary_and_keep_return_and_deprecated() {
        let source = concat!(
            "# Looks up a widget.\n",
            "# @param id [Integer] the id\n",
            "# @return [Widget, nil] the widget, or nil\n",
            "#   when there is none\n",
            "# @deprecated Use {Widget.fetch} instead.\n",
            "# @example\n",
            "#   find(1)\n",
            "def find(id)\n",
        );
        let doc = doc(source).unwrap();
        assert_eq!(doc.summary, "Looks up a widget.");
        assert_eq!(
            doc.returns.as_deref(),
            Some("[Widget, nil] the widget, or nil when there is none")
        );
        assert_eq!(
            doc.deprecated.as_deref(),
            Some("Use {Widget.fetch} instead.")
        );
        assert_eq!(
            doc.markdown(),
            "**Deprecated.** Use `Widget.fetch` instead.\n\nLooks up a widget.\n\n**Returns** `Widget, nil` — the widget, or nil when there is none"
        );
    }

    #[test]
    fn a_tag_only_comment_still_says_what_it_returns() {
        let doc = doc("# @return [String]\ndef name\n").unwrap();
        assert_eq!(doc.markdown(), "**Returns** `String`");
        assert_eq!(
            doc_above("# @return [void]\ndef go\n", 2)
                .unwrap()
                .markdown(),
            ""
        );
    }

    #[test]
    fn a_sorbet_sig_between_comment_and_def_belongs_to_the_def() {
        let one_line = "# The name.\nsig { returns(String) }\ndef name\n";
        assert_eq!(summary(one_line), "The name.");
        let block = "  # The name.\n  sig do\n    returns(String)\n  end\n  def name\n";
        assert_eq!(summary(block), "The name.");
        // An `end` that closes something else is not skipped over.
        let other = "  # About run.\n  def run\n  end\n  def name\n";
        assert_eq!(doc(other), None);
    }

    #[test]
    fn a_comment_above_a_visibility_line_is_about_the_section() {
        let source = "  # Helpers below.\n  private\n  def helper\n";
        assert_eq!(doc(source), None);
        let inline = "  # A helper.\n  private def helper\n";
        assert_eq!(summary(inline), "A helper.");
    }

    #[test]
    fn a_heredoc_body_is_not_a_comment() {
        // The `#` line is heredoc content; the terminator stands between it
        // and the definition, so nothing attaches.
        let source = "SQL = <<~SQL\n  # not a comment\nSQL\ndef query\n";
        assert_eq!(doc(source), None);
    }

    #[test]
    fn an_embedded_document_block_is_read() {
        let source = "=begin\nThe widget.\n\nMore.\n=end\nclass Widget\n";
        assert_eq!(summary(source), "The widget.");
    }

    #[test]
    fn a_long_summary_is_capped_and_says_so() {
        let long = format!("# {}\ndef go\n", "word ".repeat(200));
        let s = summary(&long);
        assert!(s.len() <= SUMMARY_CHARS + 4, "{}", s.len());
        assert!(s.ends_with('…'));
        let lines = format!("{}def go\n", "# line\n".repeat(10));
        assert_eq!(summary(&lines).lines().count(), SUMMARY_LINES);
        assert!(summary(&lines).ends_with('…'));
    }

    #[test]
    fn rdoc_markup_becomes_markdown() {
        assert_eq!(
            inline("Returns +nil+ or a <tt>Widget</tt>, see {Widget#save}."),
            "Returns `nil` or a `Widget`, see `Widget#save`."
        );
        assert_eq!(inline("a + b + c"), "a + b + c", "arithmetic is not code");
        assert_eq!(inline("an Array<String>"), "an Array\\<String>");
        assert_eq!(inline("`Array<String>`"), "`Array<String>`");
        assert_eq!(inline("a hash {a: 1}"), "a hash {a: 1}");
        assert_eq!(inline("the \\Rails validators"), "the Rails validators");
    }

    fn def_at(source: &str, name: &str) -> Def {
        crate::extract::extract(source.as_bytes())
            .defs
            .into_iter()
            .find(|d| d.name == name)
            .unwrap()
    }

    #[test]
    fn signatures_read_what_was_written_after_the_name() {
        let source = concat!(
            "class Shop::Widget < Base # :nodoc:\n",
            "  LIMIT = 10 # per page\n",
            "  NAMES = %w[\n",
            "    a b\n",
            "  ].freeze\n",
            "  def resize(width,\n",
            "             height = nil, # optional\n",
            "             **opts)\n",
            "  end\n",
            "  def self.build = new\n",
            "  def bare a, b\n",
            "  end\n",
            "  attr_reader :size\n",
            "end\n",
        );
        let sig = |name: &str, qualified: &str| signature(&def_at(source, name), qualified, source);
        assert_eq!(
            sig("Shop::Widget", "Shop::Widget"),
            "class Shop::Widget < Base"
        );
        assert_eq!(
            sig("LIMIT", "Shop::Widget::LIMIT"),
            "Shop::Widget::LIMIT = 10"
        );
        assert_eq!(sig("NAMES", "NAMES"), "NAMES = %w[ …");
        assert_eq!(
            sig("resize", "Shop::Widget#resize"),
            "Shop::Widget#resize(width, height = nil, **opts)"
        );
        assert_eq!(sig("build", "Shop::Widget.build"), "Shop::Widget.build");
        assert_eq!(sig("bare", "bare"), "bare(a, b)");
        assert_eq!(sig("size", "Shop::Widget#size"), "Shop::Widget#size");
    }

    #[test]
    fn names_a_method_the_way_ruby_docs_do() {
        assert_eq!(method_name(Some("Widget"), false, "save"), "Widget#save");
        assert_eq!(method_name(Some("Widget"), true, "build"), "Widget.build");
        assert_eq!(method_name(None, false, "helper"), "helper");
    }
}
