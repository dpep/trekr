//! A Haml template's Ruby, in place (DEC-658).
//!
//! As `template::erb_ruby` reads ERB: a buffer in which every byte that is
//! not the template's Ruby is a space and every newline stays, so a fact's
//! line, column and byte offset are the template's own. The Ruby is a script
//! line's code (`- x`, `= x`, `~ x`, `!= x`, `&= x`, a tag's `= x`, and the
//! lines a trailing comma continues it onto), a tag's attribute hash `{…}`,
//! object reference `[…]` and `(name=value)` values, each `#{…}` in text, and
//! a `:ruby` filter's body. A `;` sits in the markup byte before each piece,
//! so pieces on one line are separate statements.
//!
//! Haml closes a block by indentation, and blanking leaves no room for an
//! `end`. So an `if`/`unless`/`case` chain is read flat — its keywords
//! blanked, its conditions and branches each a statement, which is all a
//! call site needs — and a `do` block, whose parameters must stay bound, is
//! rewritten to braces (`f.input :x do |y|` reads `f.input(:x){  |y|`), so its
//! close is one `}` written in the markup before the next Ruby. Where that
//! markup has too few bytes, the innermost blocks close there and the rest
//! wait for the next room outside any block opened since — never inside one,
//! where a `}` would close it instead; past the last Ruby, after the file.

use ruby_prism::Visit;

/// The Ruby a Haml template runs, at the template's own offsets — longer than
/// the template only by the closes of blocks open at its end. `None` for a
/// file that is not UTF-8 or holds a NUL (DEC-360).
pub(crate) fn haml_ruby(src: &[u8]) -> Option<Vec<u8>> {
    if std::str::from_utf8(src).is_err() || src.contains(&0) {
        return None;
    }
    let mut haml = Haml {
        src,
        out: src
            .iter()
            .map(|&b| if b == b'\n' { b'\n' } else { b' ' })
            .collect(),
        blocks: Vec::new(),
        code_end: 0,
    };
    haml.read();
    Some(haml.finish())
}

/// Keywords of a chain read flat: the line's Ruby less the keyword.
const FLAT: [&str; 11] = [
    "if", "elsif", "unless", "else", "case", "when", "while", "until", "begin", "rescue", "ensure",
];

/// What the lines indented under a line are.
#[derive(Clone, Copy)]
enum Body {
    /// A `-#` comment's: nothing.
    Comment,
    /// Any other filter's: text, read for its `#{…}`.
    Text,
}

/// A `do` block whose `{` is written and whose `}` is not.
struct Block {
    indent: usize,
    /// A line indented no deeper than the opener has ended it.
    ended: bool,
}

struct Haml<'a> {
    src: &'a [u8],
    out: Vec<u8>,
    /// Innermost last.
    blocks: Vec<Block>,
    /// Just past the last Ruby written.
    code_end: usize,
}

impl Haml<'_> {
    fn read(&mut self) {
        let len = self.src.len();
        let mut body: Option<(usize, Body)> = None;
        let mut at = 0;
        while at < len {
            let end = self.line_end(at);
            let indent = self.src[at..end]
                .iter()
                .take_while(|b| matches!(b, b' ' | b'\t'))
                .count();
            let p = at + indent;
            if p == end {
                at = end + 1;
                continue;
            }
            if let Some((depth, kind)) = body {
                if indent > depth {
                    match kind {
                        Body::Comment => {}
                        Body::Text => self.interpolations(p, end),
                    }
                    at = end + 1;
                    continue;
                }
                body = None;
            }
            // A line ends every block opened at its depth or deeper.
            for block in self.blocks.iter_mut().rev() {
                if !block.ended && block.indent < indent {
                    break;
                }
                block.ended = true;
            }
            at = self.line(p, end, indent, &mut body);
        }
    }

    /// Read the line whose text starts at `p`; where the next line starts.
    fn line(
        &mut self,
        p: usize,
        end: usize,
        indent: usize,
        body: &mut Option<(usize, Body)>,
    ) -> usize {
        let s = self.src;
        let next = |i: usize| s.get(p + i).copied();
        match s[p] {
            b'-' if next(1) == Some(b'#') => *body = Some((indent, Body::Comment)),
            b'-' => return self.script(p + 1, indent),
            b'=' | b'~' | b'!' | b'&' => return self.indicated(p, end, indent),
            b':' => {
                let ruby = s[p + 1..end].starts_with(b"ruby")
                    && !s.get(p + 5).is_some_and(|b| b.is_ascii_alphanumeric());
                if ruby {
                    return self.ruby_filter(end, indent);
                }
                *body = Some((indent, Body::Text));
            }
            b'\\' => self.interpolations(p + 1, end),
            b'%' | b'.' if next(1).is_some_and(name_byte) => return self.tag(p, indent),
            b'#' if next(1).is_some_and(name_byte) => return self.tag(p, indent),
            _ => self.interpolations(p, end),
        }
        end + 1
    }

    /// What follows a script indicator at `p` — `=`, `~`, `!=`, `&=` — or
    /// text: `==` and `!==` interpolate, `! x` and `& x` are text, `!!!` a
    /// doctype.
    fn indicated(&mut self, p: usize, end: usize, indent: usize) -> usize {
        let s = self.src;
        let at = |i: usize| s.get(p + i).copied();
        let code = match (s[p], at(1), at(2)) {
            (b'!', Some(b'!'), Some(b'!')) => return end + 1,
            (b'=' | b'~', Some(b'='), _) if s[p] == b'=' => None,
            (b'=' | b'~', ..) => Some(p + 1),
            (b'!' | b'&', Some(b'='), Some(b'=')) => None,
            (b'!' | b'&', Some(b'='), _) => Some(p + 2),
            _ => None,
        };
        match code {
            Some(start) => self.script(start, indent),
            None => {
                self.interpolations(p, end);
                end + 1
            }
        }
    }

    /// A script's Ruby from `start` to its line's end, and onto each line a
    /// trailing comma continues it to; where the next line starts.
    fn script(&mut self, start: usize, indent: usize) -> usize {
        let s = self.src;
        let mut end = self.line_end(start);
        while trim_end(&s[start..end]).ends_with(b",") && end < s.len() {
            end = self.line_end(end + 1);
        }
        let start = start
            + s[start..end]
                .iter()
                .take_while(|b| b.is_ascii_whitespace())
                .count();
        if start == end {
            return end + 1;
        }
        self.write(start, end);
        if let Some(keyword) = FLAT.iter().find(|k| starts_with_word(&s[start..end], k)) {
            self.out[start..start + keyword.len()].fill(b' ');
            let code = &s[start..self.code_end(start, end)];
            let rest = trim_start(&code[keyword.len()..]);
            match *keyword {
                // `while x do`: the loop's `do`, with nothing to close.
                "while" | "until" if rest.len() > 2 && ends_with_word(rest, "do") => {
                    let at = start + code.len() - 2;
                    self.out[at..at + 2].fill(b' ');
                }
                // `rescue => e` binds `e` to the error, as `$! => e` does.
                "rescue" if rest.starts_with(b"=>") => {
                    self.out[start..start + 2].copy_from_slice(b"$!");
                }
                _ => {}
            }
        } else if self.braces(start, end) {
            self.blocks.push(Block {
                indent,
                ended: false,
            });
        }
        end + 1
    }

    /// A `:ruby` filter's body, the lines indented under the filter's line
    /// ending at `from`: one run of Ruby, so a statement may span lines.
    /// Where the next line starts.
    fn ruby_filter(&mut self, from: usize, indent: usize) -> usize {
        let s = self.src;
        let mut run: Option<(usize, usize)> = None;
        let mut at = from + 1;
        while at < s.len() {
            let end = self.line_end(at);
            let depth = s[at..end]
                .iter()
                .take_while(|b| matches!(b, b' ' | b'\t'))
                .count();
            if at + depth < end {
                if depth <= indent {
                    break;
                }
                run = Some((run.map_or(at + depth, |(lo, _)| lo), end));
            }
            at = end + 1;
        }
        if let Some((lo, hi)) = run {
            self.write(lo, hi);
        }
        at
    }

    /// A tag at `p` — `%name`, `.class`, `#id` — its attributes, and what
    /// follows them: a script, or text.
    fn tag(&mut self, p: usize, indent: usize) -> usize {
        let s = self.src;
        let mut q = p;
        while q < s.len() && (name_byte(s[q]) || matches!(s[q], b'%' | b'.' | b'#')) {
            if s[q] == b'#' && s.get(q + 1) == Some(&b'{') {
                break;
            }
            q += 1;
        }
        loop {
            let limit = s.len();
            match s.get(q) {
                Some(b'{' | b'[') => match self.balanced(q, limit) {
                    Some(close) => {
                        self.write(q, close + 1);
                        // `%html{ html_attributes }`: braces around a call
                        // that returns the hash.
                        if s[q] == b'{'
                            && ruby_prism::parse(&s[q..=close]).errors().next().is_some()
                        {
                            self.out[q] = b'(';
                            self.out[close] = b')';
                        }
                        q = close + 1;
                    }
                    None => {
                        let end = self.line_end(q);
                        self.write(q, end);
                        return end + 1;
                    }
                },
                Some(b'(') => match self.balanced(q, limit) {
                    Some(close) => {
                        self.attributes(q + 1, close);
                        q = close + 1;
                    }
                    None => return self.line_end(q) + 1,
                },
                _ => break,
            }
        }
        while matches!(s.get(q), Some(b'<' | b'>')) {
            q += 1;
        }
        let end = self.line_end(q);
        match (s.get(q), s.get(q + 1)) {
            (Some(b'=' | b'~' | b'!' | b'&'), _) => self.indicated(q, end, indent),
            (Some(b'/'), _) => end + 1,
            _ => {
                self.interpolations(q, end);
                end + 1
            }
        }
    }

    /// HTML-style attributes between `(` and `)`: each `name=value`'s value
    /// is Ruby, a quoted one a string read for its `#{…}`.
    fn attributes(&mut self, lo: usize, hi: usize) {
        let s = self.src;
        let mut q = lo;
        while q < hi {
            if s[q] != b'=' {
                q += 1;
                continue;
            }
            q += 1;
            while q < hi && s[q].is_ascii_whitespace() {
                q += 1;
            }
            if q >= hi {
                break;
            }
            if matches!(s[q], b'"' | b'\'') {
                let close = self.string_end(q, hi).unwrap_or(hi);
                self.interpolations(q, close);
                q = close + 1;
            } else {
                let end = q + s[q..hi]
                    .iter()
                    .take_while(|b| !b.is_ascii_whitespace())
                    .count();
                self.write(q, end);
                q = end;
            }
        }
    }

    /// Each `#{…}` in text from `lo` to `hi`: its Ruby, after a `;` in its
    /// `{`, so that its `#` is room for a close.
    fn interpolations(&mut self, lo: usize, hi: usize) {
        let s = self.src;
        let mut q = lo;
        while q + 1 < hi {
            if s[q] == b'\\' {
                q += 2;
                continue;
            }
            if s[q] != b'#' || s[q + 1] != b'{' {
                q += 1;
                continue;
            }
            let Some(close) = self.balanced(q + 1, hi) else {
                return;
            };
            self.write(q + 2, close);
            q = close + 1;
        }
    }

    /// Rewrite a script that opens a `do` block to braces, so its close is
    /// one byte: `do` becomes `{`, and a command's arguments are wrapped in
    /// the spaces around them, so the brace binds to the call `do` did.
    fn braces(&mut self, start: usize, end: usize) -> bool {
        let code = &self.src[start..self.code_end(start, end)];
        if !(code.ends_with(b"do") || code.ends_with(b"|")) {
            return false;
        }
        // The block is the one an `end` after the script closes.
        let mut probe = self.src[start..end].to_vec();
        probe.extend_from_slice(b"\nend");
        let parsed = ruby_prism::parse(&probe);
        let mut opener = Opener {
            close: end - start + 1,
            found: None,
        };
        opener.visit(&parsed.node());
        let Some(Brace { at, args }) = opener.found else {
            return false;
        };
        let space = |i: usize| self.src[start + i].is_ascii_whitespace();
        if args.is_some_and(|(open, close)| !space(open) || !space(close)) {
            return false;
        }
        self.out[start + at] = b'{';
        self.out[start + at + 1] = b' ';
        if let Some((open, close)) = args {
            self.out[start + open] = b'(';
            self.out[start + close] = b')';
        }
        true
    }

    /// Copy `src[lo..hi]`, Ruby, to the buffer: the closes of blocks ended
    /// since the last Ruby first, in the markup before it, then a `;` in the
    /// markup byte just before it.
    fn write(&mut self, lo: usize, hi: usize) {
        let separated = lo > 0 && self.out[lo - 1] == b' ';
        let room = if separated { lo - 1 } else { lo };
        self.close_blocks(room);
        if separated {
            self.out[lo - 1] = b';';
        }
        self.out[lo..hi].copy_from_slice(&self.src[lo..hi]);
        self.code_end = self.code_end.max(hi);
    }

    /// Write the `}` of each ended block, innermost first, in a blank byte
    /// between the last Ruby and `hi`. One under a block still open waits
    /// for that to end: a `}` written now would close the open one.
    fn close_blocks(&mut self, hi: usize) {
        let mut p = self.code_end;
        while p < hi && self.blocks.last().is_some_and(|b| b.ended) {
            if self.out[p] == b' ' {
                self.out[p] = b'}';
                self.blocks.pop();
            }
            p += 1;
        }
    }

    /// The buffer, every block closed: those open at the end in the blank
    /// bytes after the last Ruby, and any left over on a line after the file.
    fn finish(mut self) -> Vec<u8> {
        self.blocks.iter_mut().for_each(|b| b.ended = true);
        self.close_blocks(self.out.len());
        if !self.blocks.is_empty() {
            if self.out.last() != Some(&b'\n') {
                self.out.push(b'\n');
            }
            self.out
                .extend(std::iter::repeat_n(b'}', self.blocks.len()));
        }
        self.out
    }

    /// Where a script's Ruby from `start` to `end` ends, less a comment
    /// after it and the spaces before that.
    fn code_end(&self, start: usize, end: usize) -> usize {
        let code = &self.src[start..end];
        let mut cut = code.len();
        if code.contains(&b'#') {
            let parsed = ruby_prism::parse(code);
            let trailing = parsed
                .comments()
                .map(|c| c.location())
                .find(|l| l.end_offset() >= trim_end(code).len());
            if let Some(comment) = trailing {
                cut = comment.start_offset();
            }
        }
        start + trim_end(&code[..cut]).len()
    }

    fn line_end(&self, from: usize) -> usize {
        self.src[from..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(self.src.len(), |at| from + at)
    }

    /// The bracket that closes the one at `open`, before `limit`, past
    /// strings and their `#{…}`.
    fn balanced(&self, open: usize, limit: usize) -> Option<usize> {
        let s = self.src;
        let mut depth = 0usize;
        let mut q = open;
        while q < limit {
            match s[q] {
                b'{' | b'(' | b'[' => depth += 1,
                b'}' | b')' | b']' => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        return Some(q);
                    }
                }
                b'"' | b'\'' => q = self.string_end(q, limit)?,
                _ => {}
            }
            q += 1;
        }
        None
    }

    /// The quote that closes the string opening at `open`.
    fn string_end(&self, open: usize, limit: usize) -> Option<usize> {
        let s = self.src;
        let quote = s[open];
        let mut q = open + 1;
        while q < limit {
            match s[q] {
                b'\\' => q += 1,
                b'#' if quote == b'"' && s.get(q + 1) == Some(&b'{') => {
                    q = self.balanced(q + 1, limit)?
                }
                b if b == quote => return Some(q),
                _ => {}
            }
            q += 1;
        }
        None
    }
}

/// Where a `do` block's rewrite goes, relative to the script: the `do`, and
/// the spaces before and after a command's unparenthesized arguments.
struct Brace {
    at: usize,
    args: Option<(usize, usize)>,
}

/// Finds the call (or lambda) whose `do` block the probe's `end` closes.
struct Opener {
    close: usize,
    found: Option<Brace>,
}

impl<'pr> Visit<'pr> for Opener {
    fn visit_call_node(&mut self, call: &ruby_prism::CallNode<'pr>) {
        if let Some(block) = call.block().and_then(|b| b.as_block_node())
            && block.closing_loc().start_offset() == self.close
            && block.opening_loc().as_slice() == b"do"
        {
            let args = match (call.opening_loc(), call.arguments(), call.message_loc()) {
                (None, Some(args), Some(message)) => {
                    Some((message.end_offset(), args.location().end_offset()))
                }
                _ => None,
            };
            self.found = Some(Brace {
                at: block.opening_loc().start_offset(),
                args,
            });
        }
        ruby_prism::visit_call_node(self, call);
    }

    fn visit_lambda_node(&mut self, lambda: &ruby_prism::LambdaNode<'pr>) {
        if lambda.closing_loc().start_offset() == self.close
            && lambda.opening_loc().as_slice() == b"do"
        {
            self.found = Some(Brace {
                at: lambda.opening_loc().start_offset(),
                args: None,
            });
        }
        ruby_prism::visit_lambda_node(self, lambda);
    }
}

fn name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b':')
}

fn trim_end(bytes: &[u8]) -> &[u8] {
    let kept = bytes.len()
        - bytes
            .iter()
            .rev()
            .take_while(|b| b.is_ascii_whitespace())
            .count();
    &bytes[..kept]
}

fn trim_start(bytes: &[u8]) -> &[u8] {
    &bytes[bytes.iter().take_while(|b| b.is_ascii_whitespace()).count()..]
}

/// `code` ends with `word`, after a space.
fn ends_with_word(code: &[u8], word: &str) -> bool {
    code.ends_with(word.as_bytes())
        && code
            .len()
            .checked_sub(word.len() + 1)
            .is_some_and(|at| code[at].is_ascii_whitespace())
}

fn starts_with_word(code: &[u8], word: &str) -> bool {
    code.starts_with(word.as_bytes())
        && !code
            .get(word.len())
            .is_some_and(|&b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'?' | b'!' | b':'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ruby(src: &str) -> String {
        String::from_utf8(haml_ruby(src.as_bytes()).unwrap()).unwrap()
    }

    fn parses(src: &str) -> String {
        let out = ruby(src);
        let errors: Vec<String> = ruby_prism::parse(out.as_bytes())
            .errors()
            .map(|e| e.message().to_string())
            .collect();
        assert!(errors.is_empty(), "{errors:?} in\n{out}");
        out
    }

    /// Each `needle` in `src` is at the same offset in its Ruby.
    fn kept(src: &str, needles: &[&str]) {
        let out = ruby(src);
        for needle in needles {
            let at = src.find(needle).unwrap();
            assert_eq!(&out[at..at + needle.len()], *needle, "{needle} in\n{out}");
        }
    }

    #[test]
    fn script_lines_are_ruby_at_their_offsets_and_markup_is_not() {
        let src = "%p é\n= link_to post.title, post\n- x = 1\n%h1= x\n.a#b hello\n";
        let out = parses(src);
        assert_eq!(out.len(), src.len());
        assert_eq!(out.lines().count(), src.lines().count());
        kept(src, &["link_to post.title, post", "x = 1", "x\n"]);
        assert!(!out.contains("hello") && !out.contains('%'), "{out}");
    }

    #[test]
    fn a_do_block_closes_where_its_indentation_does() {
        let src = "- content_for :title do\n  = t('x')\n\n- content_for :heading do\n  = t('y')\n";
        let out = parses(src);
        assert!(out.starts_with(" ;content_for(:title){ "), "{out}");
        let parsed = ruby_prism::parse(out.as_bytes());
        let program = parsed.node();
        let statements = program.as_program_node().unwrap().statements().body();
        assert_eq!(statements.iter().count(), 2, "{out}");
    }

    #[test]
    fn a_blocks_parameters_are_bound_in_its_body_only() {
        let src = "%ul\n  - @posts.each do |post|\n    %li= post.title\n%p= post\n";
        let out = parses(src);
        // The `}` sits before `%p`'s Ruby, so the last `post` is a call.
        let close = out.find('}').unwrap();
        assert!(close < src.rfind("post").unwrap(), "{out}");
        assert!(close > src.find("post.title").unwrap(), "{out}");
    }

    #[test]
    fn a_close_without_room_never_closes_the_next_block() {
        // Two blocks end at the left margin, which has a byte for one `}`:
        // the inner closes there, and the outer after the new block, which
        // keeps its body.
        let src = "= form_for a do |f|\n  = f.fields_for :b do |ff|\n    = ff.text_field :c\n= form_for d do |g|\n  = g.text_field :e\n";
        let out = parses(src);
        assert_eq!(out.find('}'), src.find("= form_for d"), "{out}");
        assert!(out.ends_with("\n}}"), "{out}");
    }

    #[test]
    fn an_interpolation_at_the_margin_closes_the_block_before_it() {
        let src = "- items.each do |g|\n  = g\n#{g} items\n";
        let out = parses(src);
        assert_eq!(out.find('}'), src.find("#{"), "{out}");
        assert!(out.contains("};g "), "{out}");
    }

    #[test]
    fn a_form_builder_block_keeps_its_call_and_continuations() {
        parses(
            "= simple_form_for current_user, url: x_path, html: { id: :edit } do |f|\n  .row\n    = f.input :locale,\n              collection: ui_languages,\n              label_method: ->(l) { native_name(l) }\n  = f.button :submit\n",
        );
        let out = parses(
            "= f.simple_fields_for :settings, current_user.settings do |ff|\n  = ff.input :theme\n",
        );
        assert!(
            out.contains("f.simple_fields_for(:settings, current_user.settings){  |ff|"),
            "{out}"
        );
    }

    #[test]
    fn a_conditional_chain_is_read_flat() {
        let src = "- if a?\n  = b\n- elsif c\n  = d\n- else\n  = e\n- unless f\n  = g\n";
        let out = parses(src);
        assert_eq!(
            out.split_whitespace().collect::<Vec<_>>().join(" "),
            "; a? ;b ; c ;d ; ;e ; f ;g",
        );
    }

    #[test]
    fn a_do_with_a_comment_after_it_opens_a_block() {
        let src = "- @items.each do |item| # each row\n  = item\n%p= item\n";
        let out = parses(src);
        assert!(out.find('}').unwrap() < src.rfind("item").unwrap(), "{out}");
    }

    #[test]
    fn a_while_do_and_a_rescue_into_a_name_are_read_flat() {
        let out = parses("- while waiting? do\n  = in_while\n- until x.undo\n  = y\n");
        assert!(!out.contains(" do") && out.contains("x.undo"), "{out}");
        let out = parses("- begin\n  = risky\n- rescue => e\n  = e.message\n- rescue Oops => f\n");
        assert!(
            out.contains("$!     => e") && out.contains("Oops => f"),
            "{out}"
        );
    }

    #[test]
    fn nested_blocks_and_conditionals_balance() {
        parses(
            "- @a.each do |x|\n  - if x\n    - x.each do |y|\n      = y\n  - else\n    = z\n= after\n",
        );
        parses(".box\n  - @a.each_with_index do |x, i|\n    - x.b.each do |y|\n      = y\n- c\n");
        parses("- @a.each do |x|\n  = x\n");
        parses("- form = lambda do |x|\n  = x\n");
    }

    #[test]
    fn attributes_and_interpolation_are_ruby() {
        let src = "%a{ href: url_for(x), class: \"c #{y ? 'a' : 'b'}\" }= label\n%img(src=image_path alt=\"#{alt_text}\")\n%p Hi #{name}, #{greeting}!\n%div[@widget]\n";
        parses(src);
        kept(
            src,
            &[
                "href: url_for(x)",
                "image_path",
                "alt_text",
                "name",
                "greeting",
                "@widget",
                "label",
            ],
        );
    }

    #[test]
    fn a_multiline_attribute_hash_is_one_hash() {
        let src = "%a{ href: one,\n    title: two }\n  text\n";
        parses(src);
        kept(src, &["one", "two"]);
        let out = parses("%html{ html_attributes }\n");
        assert!(out.contains("( html_attributes )"), "{out}");
    }

    #[test]
    fn comments_text_and_filters() {
        let src = "-# it's a comment with helper_x\n  still_comment\n:javascript\n  var a = #{js_value};\n:ruby\n  x = compute\n  y = x\n\\= not_code\n/ html comment\n!!! 5\n";
        let out = parses(src);
        assert!(
            !out.contains("helper_x") && !out.contains("still_comment"),
            "{out}"
        );
        assert!(!out.contains("var a") && !out.contains("not_code"), "{out}");
        kept(src, &["js_value", "x = compute", "y = x"]);
    }

    #[test]
    fn a_ruby_filter_is_one_run_of_ruby() {
        let src = ":ruby\n  opts = {\n    a: one,\n\n    b: two\n  }\n  list = x\n    .order(three)\n%p= opts\n";
        let out = parses(src);
        assert_eq!(out.matches(';').count(), 2, "{out}");
        kept(src, &["a: one,\n\n    b: two\n  }", "x\n    .order(three)"]);
    }

    #[test]
    fn escaped_and_unescaped_scripts() {
        let src = "!= raw_html\n&= escaped\n~ preserved\n%p!= tag_raw\n== text #{interp}\n";
        parses(src);
        kept(
            src,
            &["raw_html", "escaped", "preserved", "tag_raw", "interp"],
        );
    }

    #[test]
    fn a_binary_is_no_template() {
        assert!(haml_ruby(b"\x89PNG\0\0").is_none());
    }
}
