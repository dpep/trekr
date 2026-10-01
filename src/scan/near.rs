//! Which of a checkout's files a few files most likely need (DEC-322).
//!
//! A first index reads the files someone is looking at before the rest, and
//! with them the files their constants most likely live in. "Most likely" is
//! the autoloader's own convention — `Admin::UserReport` in
//! `admin/user_report.rb`, under whichever root — tried for each scope the
//! reference is written in, as Ruby's lookup would. Only an order: a wrong
//! guess costs a file read early, never an answer.

use super::Files;
use crate::core::Facts;
use std::collections::{HashMap, HashSet};

/// At most this many files for one constant: a name every copy of a gem
/// defines would otherwise pull them all forward.
const PER_NAME: usize = 4;

/// Files `facts` (each read from the path beside it) most likely name,
/// nearest first, at most `cap` of them, none of `from` itself.
pub(crate) fn nearby(files: &Files, from: &[(String, Facts)], cap: usize) -> Vec<String> {
    let mut by_name: HashMap<&str, Vec<&str>> = HashMap::new();
    for path in files.keys() {
        let name = path.rsplit('/').next().unwrap_or(path);
        by_name.entry(name).or_default().push(path);
    }
    let mut seen: HashSet<&str> = from.iter().map(|(path, _)| path.as_str()).collect();
    let mut out = Vec::new();
    for (_, facts) in from {
        for (name, nesting) in written(facts) {
            for candidate in candidates(name, nesting) {
                let file = candidate.rsplit('/').next().unwrap_or(&candidate);
                let Some(paths) = by_name.get(file) else {
                    continue;
                };
                let found = paths
                    .iter()
                    .filter(|path| {
                        **path == candidate
                            || path
                                .strip_suffix(candidate.as_str())
                                .is_some_and(|dir| dir.ends_with('/'))
                    })
                    .take(PER_NAME);
                for path in found {
                    if seen.insert(path) {
                        out.push(path.to_string());
                        if out.len() >= cap {
                            return out;
                        }
                    }
                }
            }
        }
    }
    out
}

/// Which of `gems` (each with its listed `lib/`) hold a file `from`'s
/// constants most likely live in, by the same convention: the gem whose
/// `lib/` has `active_support/concern.rb` for `ActiveSupport::Concern`.
pub(crate) fn gems_named(
    gems: &[(std::path::PathBuf, Vec<String>)],
    from: &[(String, Facts)],
) -> HashSet<usize> {
    let mut holding: HashMap<&str, Vec<usize>> = HashMap::new();
    for (at, (_, paths)) in gems.iter().enumerate() {
        for path in paths {
            holding.entry(path.as_str()).or_default().push(at);
        }
    }
    let mut named = HashSet::new();
    for (name, nesting) in from.iter().flat_map(|(_, facts)| written(facts)) {
        for candidate in candidates(name, nesting) {
            if let Some(gems) = holding.get(format!("lib/{candidate}").as_str()) {
                named.extend(gems);
            }
        }
    }
    named
}

/// The constants a file's code names, each with the scopes it is written in:
/// its references, and the targets of its `include`s, superclasses and kin.
fn written(facts: &Facts) -> impl Iterator<Item = (&str, &[String])> {
    facts
        .const_refs
        .iter()
        .map(|r| (r.name.as_str(), r.nesting.as_slice()))
        .chain(
            facts
                .ancestry
                .iter()
                .filter(|edge| edge.target != "self")
                .map(|edge| (edge.target.as_str(), edge.owner.get(1..).unwrap_or(&[]))),
        )
}

/// Where the autoloader would look for `name` written inside `nesting`
/// (innermost first): each enclosing scope's file for it, innermost first,
/// then the top level's. `::Foo` is only the top level's.
fn candidates(name: &str, nesting: &[String]) -> Vec<String> {
    if let Some(top) = name.strip_prefix("::") {
        return vec![file_of(top)];
    }
    let mut out: Vec<String> = (0..nesting.len())
        .map(|at| {
            let scope: Vec<&str> = nesting[at..].iter().rev().map(String::as_str).collect();
            file_of(&format!("{}::{name}", scope.join("::")))
        })
        .collect();
    out.push(file_of(name));
    out
}

/// `Admin::UserReport` → `admin/user_report.rb`, as ActiveSupport's
/// `underscore` spells it: `HTMLParser` → `html_parser`.
fn file_of(constant: &str) -> String {
    let parts: Vec<String> = constant.split("::").map(underscore).collect();
    format!("{}.rb", parts.join("/"))
}

fn underscore(word: &str) -> String {
    let chars: Vec<char> = word.chars().collect();
    let mut out = String::with_capacity(word.len() + 4);
    for (at, &c) in chars.iter().enumerate() {
        if c.is_ascii_uppercase() && at > 0 {
            let before = chars[at - 1];
            let after_lower = chars.get(at + 1).is_some_and(char::is_ascii_lowercase);
            if before.is_ascii_lowercase()
                || before.is_ascii_digit()
                || (before.is_ascii_uppercase() && after_lower)
            {
                out.push('_');
            }
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Oid;

    fn files(paths: &[&str]) -> Files {
        paths
            .iter()
            .map(|p| (p.to_string(), Oid(String::new())))
            .collect()
    }

    #[test]
    fn spells_a_constant_as_the_autoloader_does() {
        assert_eq!(file_of("Admin::UserReport"), "admin/user_report.rb");
        assert_eq!(file_of("HTMLParser"), "html_parser.rb");
        assert_eq!(file_of("OAuth2Token"), "o_auth2_token.rb");
        assert_eq!(file_of("Widget"), "widget.rb");
    }

    #[test]
    fn finds_a_constants_file_under_any_root_innermost_scope_first() {
        let source =
            b"class Shop\n  def go\n    Report.new\n    Widget\n    ::Gadget\n  end\nend\n";
        let facts = crate::extract::extract(source);
        let tree = files(&[
            "app/models/shop.rb",
            "app/models/shop/report.rb",
            "app/models/report.rb",
            "lib/widget.rb",
            "lib/gadget.rb",
            "lib/not_widget.rb",
            "app/models/unrelated.rb",
        ]);
        let near = nearby(&tree, &[("app/models/shop.rb".into(), facts)], 10);
        assert_eq!(
            near,
            [
                "app/models/shop/report.rb",
                "app/models/report.rb",
                "lib/widget.rb",
                "lib/gadget.rb"
            ]
        );
    }

    #[test]
    fn names_the_gem_whose_lib_holds_a_constants_file() {
        let facts = crate::extract::extract(b"class Widget\n  include Kit::Sorting\nend\n");
        let listed = |paths: &[&str]| paths.iter().map(|p| p.to_string()).collect();
        let gems = vec![
            (
                "/gems/other".into(),
                listed(&["lib/other.rb", "lib/sorting.rb"]),
            ),
            (
                "/gems/kit".into(),
                listed(&["lib/kit.rb", "lib/kit/sorting.rb"]),
            ),
        ];
        let named = gems_named(&gems, &[("widget.rb".into(), facts)]);
        assert_eq!(named, HashSet::from([1]));
    }

    #[test]
    fn stops_at_its_cap_and_never_lists_a_file_it_was_given() {
        let facts = crate::extract::extract(b"A\nB\nC\nSelf\n");
        let tree = files(&["a.rb", "b.rb", "c.rb", "self.rb"]);
        let near = nearby(&tree, &[("self.rb".into(), facts)], 2);
        assert_eq!(near, ["a.rb", "b.rb"]);
    }
}
