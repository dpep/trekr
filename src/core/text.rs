//! Runs of characters at a text's edge, for the readers that scan Ruby by
//! hand. Each returns a subslice, so the caller never computes a byte offset:
//! `rfind(..).map_or(0, |i| i + 1)` lands inside a multi-byte character
//! (DEC-835).

/// A character Ruby allows in an identifier: any non-ASCII one is.
pub(crate) fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || !c.is_ascii()
}

/// The longest suffix of `text` whose characters all satisfy `keep`.
pub(crate) fn trailing_run(text: &str, keep: impl Fn(char) -> bool) -> &str {
    let start = text
        .char_indices()
        .rev()
        .take_while(|&(_, c)| keep(c))
        .last()
        .map_or(text.len(), |(at, _)| at);
    text.get(start..).unwrap_or_default()
}

/// The longest prefix of `text` whose characters all satisfy `keep`.
pub(crate) fn leading_run(text: &str, keep: impl Fn(char) -> bool) -> &str {
    let end = text
        .char_indices()
        .find(|&(_, c)| !keep(c))
        .map_or(text.len(), |(at, _)| at);
    text.get(..end).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(c: char) -> bool {
        c.is_ascii_alphanumeric() || c == '_'
    }

    #[test]
    fn a_run_stops_at_a_multibyte_character_without_splitting_it() {
        assert_eq!(trailing_run("año", word), "o");
        assert_eq!(trailing_run("x.año", word), "o");
        assert_eq!(leading_run("año", word), "a");
        assert_eq!(trailing_run("日本", word), "");
        assert_eq!(leading_run("日本", word), "");
    }

    #[test]
    fn a_run_may_be_the_whole_text_or_none_of_it() {
        assert_eq!(
            trailing_run("Foo::Bar", |c| word(c) || c == ':'),
            "Foo::Bar"
        );
        assert_eq!(leading_run("save!", word), "save");
        assert_eq!(trailing_run("", word), "");
        assert_eq!(leading_run(" x", word), "");
        assert_eq!(trailing_run("x ", word), "");
    }

    #[test]
    fn a_run_of_multibyte_characters_is_kept_whole() {
        assert_eq!(trailing_run("x.año", is_ident), "año");
        assert_eq!(leading_run("日本.x", is_ident), "日本");
    }
}
