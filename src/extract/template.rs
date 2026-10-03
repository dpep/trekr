//! A view template's Ruby, in place (DEC-520).
//!
//! An ERB template is compiled by Erubi into one method whose body is the
//! insides of its tags, joined in order: `<% %>` a statement, `<%= %>` (and
//! `<%==`) an expression whose value is output, `<%# %>` nothing, `<%%` text.
//! Read here into a buffer the length of the file in which every byte outside
//! a tag's code is a space — a newline stays a newline — so a fact's line and
//! column, and its byte offset, are the template's own. A `;` sits where each
//! tag's `<` and `>` were, so two tags on one line are two statements, and a
//! block opened in one tag (`<% @posts.each do |post| %>`) closes in another
//! (`<% end %>`), as Erubi's output does.
//!
//! A pure function of the bytes, as every extraction is; which reader a file
//! gets is chosen by its path (`extract_file`), as a SQL dump's is (DEC-480).

/// The Ruby an ERB template runs, at the template's own offsets. `None` for
/// a file that is not UTF-8 or holds a NUL: a binary with a template's
/// extension, not a template (DEC-360).
pub(crate) fn erb_ruby(src: &[u8]) -> Option<Vec<u8>> {
    if std::str::from_utf8(src).is_err() || src.contains(&0) {
        return None;
    }
    let mut out: Vec<u8> = src
        .iter()
        .map(|&b| if b == b'\n' { b'\n' } else { b' ' })
        .collect();
    let mut at = 0;
    while let Some(open) = find(src, at, b"<%") {
        let mut start = open + 2;
        let literal_or_comment = matches!(src.get(start), Some(b'%' | b'#'));
        match src.get(start) {
            Some(b'=') => {
                start += 1;
                if src.get(start) == Some(&b'=') {
                    start += 1;
                }
            }
            Some(b'-') => start += 1,
            _ => {}
        }
        let Some(close) = find(src, start, b"%>") else {
            // Erubi raises on an unclosed tag; what follows it is still the
            // Ruby someone is writing.
            if !literal_or_comment {
                out[start..].copy_from_slice(&src[start..]);
                out[open] = b';';
            }
            break;
        };
        at = close + 2;
        if literal_or_comment {
            continue;
        }
        // `-%>` trims the newline after the tag, `=%>` is Erubi's too: the
        // delimiter's, not the code's.
        let end = match src[start..close].last() {
            Some(b'-' | b'=') => close - 1,
            _ => close,
        };
        out[start..end].copy_from_slice(&src[start..end]);
        out[open] = b';';
        out[close + 1] = b';';
    }
    Some(out)
}

fn find(haystack: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    haystack
        .get(from..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|at| from + at)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ruby(src: &str) -> String {
        String::from_utf8(erb_ruby(src.as_bytes()).unwrap()).unwrap()
    }

    fn parses(src: &str) {
        let out = erb_ruby(src.as_bytes()).unwrap();
        let parsed = ruby_prism::parse(&out);
        let errors: Vec<String> = parsed.errors().map(|e| e.message().to_string()).collect();
        assert!(
            errors.is_empty(),
            "{errors:?} in\n{}",
            String::from_utf8_lossy(&out)
        );
    }

    #[test]
    fn keeps_every_offset_and_line() {
        let src = "<p>é <%= link_to post.title, post %></p>\n<% if x %>\n  y\n<% end %>\n";
        let out = ruby(src);
        assert_eq!(out.len(), src.len());
        assert_eq!(out.lines().count(), src.lines().count());
        let at = src.find("link_to").unwrap();
        assert_eq!(&out[at..at + 7], "link_to");
        assert!(
            !out.contains('<') && !out.contains('y'),
            "markup is blanked: {out:?}"
        );
    }

    #[test]
    fn two_tags_on_a_line_are_two_statements() {
        let out = erb_ruby(b"<%= a %> <%= b %>").unwrap();
        let parsed = ruby_prism::parse(&out);
        let statements = parsed
            .node()
            .as_program_node()
            .unwrap()
            .statements()
            .body()
            .iter()
            .count();
        assert_eq!(statements, 2, "{:?}", String::from_utf8_lossy(&out));
    }

    #[test]
    fn a_block_opened_in_one_tag_closes_in_another() {
        parses("<%= form_with model: @post do |f| %>\n  <%= f.text_field :title %>\n<% end %>\n");
        parses("<% @posts.each do |post| %><li><%= post.title %></li><% end %>");
        parses("<% if a %>A<% elsif b %>B<% else %>C<% end %>");
        parses("<% case kind %>\n<% when :a %>A\n<% when :b %>B\n<% end %>");
        parses("<%= content_tag :div, class: 'x' do %>hi<% end %>");
    }

    #[test]
    fn comments_and_literals_are_not_ruby() {
        let out = ruby("<%# a comment's apostrophe %><%% not code %><%= ok %>");
        assert!(!out.contains("comment"));
        assert!(!out.contains("not code"));
        assert!(out.contains("ok"));
    }

    #[test]
    fn trimming_and_raw_delimiters_are_not_code() {
        let out = ruby("<%- x -%>\n<%== y %>\n<%= z =%>");
        assert_eq!(
            out.split_whitespace().collect::<Vec<_>>(),
            ["; x ;", "; y ;", "; z ;"]
                .iter()
                .flat_map(|s| s.split_whitespace())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_binary_is_no_template() {
        assert!(erb_ruby(b"\x89PNG\0\0").is_none());
    }
}
