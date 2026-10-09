//! Which of an answer's locations a definition request gets, and which a
//! declaration request gets (DEC-646). An `.rbi` is Sorbet's signature of a
//! method or class — a declaration, never the implementation — so it answers
//! Go to Definition only when nothing real defines the same thing, and Go to
//! Declaration first.

use crate::resolve::MethodSite;
use crate::tree::Site;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Asked {
    Definition,
    Declaration,
}

/// The locations among `items` that answer `asked`, in their order. `of`
/// says what each one locates — its method or constant — and where: a
/// signature stands in for its own definition only, never for another
/// candidate's, so a residue's `.rbi`-only candidate stays a definition.
pub(crate) fn answering<T, K: PartialEq>(
    asked: Asked,
    items: Vec<T>,
    of: impl Fn(&T) -> (K, &Site),
) -> Vec<T> {
    let tagged: Vec<(K, bool)> = items
        .iter()
        .map(|item| {
            let (key, site) = of(item);
            (key, site.is_rbi())
        })
        .collect();
    // Does what `key` locates have a location of the other sort than `rbi`?
    let has = |key: &K, rbi: bool| tagged.iter().any(|(k, r)| k == key && *r == rbi);
    items
        .into_iter()
        .zip(&tagged)
        .filter(|(_, (key, rbi))| match asked {
            Asked::Definition => !rbi || !has(key, false),
            Asked::Declaration => *rbi || !has(key, true),
        })
        .map(|(item, _)| item)
        .collect()
}

/// One method's locations: where it is, then the signatures that describe it
/// and are not already among them — a stub that is the answer is its own.
pub(crate) fn of_one_method(
    sites: Vec<MethodSite>,
    signatures: Vec<MethodSite>,
) -> Vec<MethodSite> {
    let listed = |s: &MethodSite| {
        sites
            .iter()
            .any(|at| at.site.path == s.site.path && at.site.line == s.site.line)
    };
    let signatures: Vec<MethodSite> = signatures.into_iter().filter(|s| !listed(s)).collect();
    sites.into_iter().chain(signatures).collect()
}

/// `sites` with real source first and `.rbi` signatures after: the order the
/// CLI lists a constant's sites in, which keeps every one visible.
pub(crate) fn real_first(sites: &mut [Site]) {
    sites.sort_by_key(Site::is_rbi);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(path: &str) -> Site {
        Site {
            path: path.to_string(),
            line: 1,
            col: 1,
            kind: "method".to_string(),
        }
    }

    fn asked(asked: Asked, items: &[(&str, &str)]) -> Vec<String> {
        let items: Vec<(&str, Site)> = items.iter().map(|(k, p)| (*k, site(p))).collect();
        answering(asked, items, |(key, site)| (*key, site))
            .into_iter()
            .map(|(_, site)| site.path)
            .collect()
    }

    #[test]
    fn a_signature_is_a_definition_only_when_it_is_all_there_is() {
        use Asked::*;
        // (asked, (what, path) in order, the paths answered)
        type Case<'a> = (Asked, &'a [(&'a str, &'a str)], &'a [&'a str]);
        let cases: &[Case] = &[
            // Real source and its signature: one each way.
            (Definition, &[("A", "a.rb"), ("A", "a.rbi")], &["a.rb"]),
            (Declaration, &[("A", "a.rb"), ("A", "a.rbi")], &["a.rbi"]),
            // A gem's method only its RBI describes.
            (Definition, &[("B", "b.rbi")], &["b.rbi"]),
            (Declaration, &[("B", "b.rbi")], &["b.rbi"]),
            // No signature: a declaration falls back to the definition.
            (Declaration, &[("A", "a.rb")], &["a.rb"]),
            // Another candidate's signature is not this one's: each keeps its
            // own, in order.
            (
                Definition,
                &[("B", "b.rbi"), ("A", "a.rb"), ("A", "a.rbi")],
                &["b.rbi", "a.rb"],
            ),
            (
                Declaration,
                &[("A", "a.rb"), ("B", "b.rb"), ("A", "a.rbi")],
                &["b.rb", "a.rbi"],
            ),
            (Definition, &[], &[]),
        ];
        for (ask, items, want) in cases {
            assert_eq!(asked(*ask, items), *want, "{ask:?} of {items:?}");
        }
    }

    #[test]
    fn real_source_is_listed_before_its_signatures() {
        let mut sites = vec![site("w.rbi"), site("w.rb"), site("x.rbi"), site("x.rb")];
        real_first(&mut sites);
        let paths: Vec<&str> = sites.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(paths, ["w.rb", "x.rb", "w.rbi", "x.rbi"]);
    }
}
