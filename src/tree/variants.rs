//! A name declared with two different superclasses (DEC-072).
//!
//! Ruby raises "superclass mismatch" when both declarations load, so a
//! checkout that holds both is holding two programs: rails' test fakes
//! (`Post = Struct.new`) beside the models (`class Post < ActiveRecord::Base`),
//! a benchmark script's `User = Data.define` beside the app's `User`. Merging
//! them let whichever sorted first win the superclass and gave the other's
//! mixins and methods to both.
//!
//! So the name is split. Each superclass becomes a **variant** — its own entry
//! under `Name@n`, holding that superclass and whatever was written nearest its
//! declarations — and the name itself keeps its sites and namespace but no
//! ancestry. A query picks the variant nearest the file asking; equally near
//! two, it gets the name, whose ancestry says it could not be settled.

use super::{Entry, EntryRef, Target, Tree};
use std::collections::HashMap;

/// An ancestry edge placed in the namespace, before it is attached.
pub(super) struct PlacedEdge {
    pub(super) scope: String,
    pub(super) relation: String,
    pub(super) target: Target,
    pub(super) path: String,
}

/// One of a split name's classes, while the namespace is assembled.
pub(super) struct Variant {
    pub(super) key: String,
    /// What its superclass resolves to: the edges that agree are one variant.
    pub(super) group: String,
    /// The files declaring that superclass — what "nearest" is measured from.
    pub(super) anchors: Vec<String>,
}

/// Marks a variant's key. Never part of a Ruby constant, so a written name can
/// never collide with one.
const MARK: char = '@';

/// The name a person wrote, for a key that may be a variant's.
pub(crate) fn public_name(fqn: &str) -> &str {
    fqn.split_once(MARK).map_or(fqn, |(name, _)| name)
}

impl Tree {
    /// Split every name whose superclass edges disagree, declaring an entry
    /// per variant. Keyed by the name split.
    pub(super) fn split_conflicts(
        &mut self,
        edges: &[PlacedEdge],
    ) -> HashMap<String, Vec<Variant>> {
        let mut groups: HashMap<&str, Vec<Variant>> = HashMap::new();
        // An `.rbi` describes a class some program defines; it is not a program
        // of its own. Tapioca writes the superclass it saw at runtime, which
        // differs from the source's whenever that is computed.
        let declaring = |e: &&PlacedEdge| e.relation == "superclass" && !e.path.ends_with(".rbi");
        for edge in edges.iter().filter(declaring) {
            let group = self.superclass_group(&edge.target);
            let variants = groups.entry(&edge.scope).or_default();
            match variants.iter_mut().find(|v| v.group == group) {
                Some(variant) => variant.anchors.push(edge.path.clone()),
                None => variants.push(Variant {
                    key: format!("{}{MARK}{}", edge.scope, variants.len() + 1),
                    group,
                    anchors: vec![edge.path.clone()],
                }),
            }
        }
        let split: HashMap<String, Vec<Variant>> = groups
            .into_iter()
            .filter(|(_, variants)| variants.len() > 1)
            .map(|(scope, variants)| (scope.to_string(), variants))
            .collect();
        let names = self.names.building();
        for variants in split.values() {
            for variant in variants {
                names.insert(
                    variant.key.clone(),
                    Entry {
                        kind: "class".to_string(),
                        ..Entry::default()
                    },
                );
            }
        }
        split
    }

    /// What a superclass edge names, for telling two declarations apart:
    /// `Base` and `ActiveRecord::Base` written in different scopes may be one
    /// class, and one `Base` written in two scopes may be two. A target nothing
    /// indexed resolves is compared as written.
    pub(super) fn superclass_group(&self, target: &Target) -> String {
        match self.resolve_lexical(&target.name, &target.nesting) {
            Some(fqn) => self.namespace_of(&fqn),
            None => format!("?{}", target.name),
        }
    }

    /// A reference to a split name, pointed at the variant nearest where it is
    /// written — `class SpecialPost < Post` beside the model is the model's
    /// subclass. Equally near two, it stays on the name.
    pub(super) fn aim(
        &self,
        target: Target,
        path: &str,
        split: &HashMap<String, Vec<Variant>>,
    ) -> Target {
        if split.is_empty() {
            return target;
        }
        let Some(variants) = self
            .resolve_lexical(&target.name, &target.nesting)
            .map(|fqn| self.namespace_of(&fqn))
            .and_then(|fqn| split.get(&fqn))
        else {
            return target;
        };
        let name = &variants[0].key;
        match nearest(name, variants.iter().map(|v| (&v.anchors, v)), path)[..] {
            [only] => Target {
                name: format!("::{}", only.key),
                nesting: Vec::new(),
            },
            _ => target,
        }
    }

    /// The variants a split name was divided into, in declaration order.
    /// Empty for every name that was not split.
    pub(crate) fn variants_of(&self, fqn: &str) -> Vec<String> {
        (1..)
            .map(|n| format!("{fqn}{MARK}{n}"))
            .take_while(|key| self.names.contains(key))
            .collect()
    }

    /// The class `fqn` means in the file at `path`: the nearest variant of a
    /// split name, or the name itself when it was not split or two variants
    /// are equally near.
    pub(crate) fn variant_at(&self, fqn: &str, path: &str) -> String {
        match &self.nearest_variants(fqn, path)[..] {
            [only] => only.clone(),
            _ => fqn.to_string(),
        }
    }

    /// Every variant of a split name tied for nearest `path` — empty for a name
    /// that was not split.
    pub(super) fn nearest_variants(&self, fqn: &str, path: &str) -> Vec<String> {
        let variants = self.variants_of(fqn);
        if variants.is_empty() {
            return variants;
        }
        let path = if path.starts_with('/') || self.root.is_empty() {
            path.to_string()
        } else {
            format!("{}/{path}", self.root)
        };
        let declared: Vec<(Vec<String>, &String)> = variants
            .iter()
            .map(|key| {
                let paths = self
                    .names
                    .get(key)
                    .map(EntryRef::sites)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|site| site.path)
                    .collect();
                (paths, key)
            })
            .collect();
        nearest(
            fqn,
            declared.iter().map(|(paths, key)| (paths, *key)),
            &path,
        )
        .into_iter()
        .cloned()
        .collect()
    }

    /// What the variants of a split name were declared to inherit, as
    /// written — the ancestry a query that could not pick one is missing.
    pub(super) fn conflicting_superclasses(&self, fqn: &str) -> Vec<String> {
        self.variants_of(fqn)
            .iter()
            .filter_map(|key| self.names.get(key).and_then(EntryRef::superclass))
            .map(|target| public_name(target.name.trim_start_matches("::")).to_string())
            .collect()
    }
}

/// The declarations of `name` nearest `path`: the most leading directories in
/// common with one of their files, and the file itself nearest of all.
///
/// Equally near several, the one declared in the file named for it wins —
/// `user.rb` for `User` is how an autoloader or a `require "models/user"`
/// reaches a class from anywhere, where a class in `fake_models.rb` or a
/// benchmark script is reached only by what sits beside it. Short of that,
/// every candidate tied for nearest is returned.
pub(super) fn nearest<'a, T>(
    name: &str,
    candidates: impl Iterator<Item = (&'a Vec<String>, T)>,
    path: &str,
) -> Vec<T> {
    let mut best: Option<usize> = None;
    let mut found: Vec<(&Vec<String>, T)> = Vec::new();
    for (files, candidate) in candidates {
        let score = files.iter().map(|file| closeness(file, path)).max();
        let Some(score) = score else { continue };
        match best {
            Some(top) if score < top => {}
            Some(top) if score == top => found.push((files, candidate)),
            _ => {
                best = Some(score);
                found = vec![(files, candidate)];
            }
        }
    }
    if found.len() > 1 {
        let named = |files: &Vec<String>| files.iter().any(|file| named_for(file, name));
        if found.iter().filter(|(files, _)| named(files)).count() == 1 {
            found.retain(|(files, _)| named(files));
        }
    }
    found.into_iter().map(|(_, candidate)| candidate).collect()
}

/// Is this the file a constant's name says it lives in? Compared without case
/// or underscores, so `http_client.rb` names `HTTPClient`.
fn named_for(file: &str, fqn: &str) -> bool {
    let stem = file
        .rsplit('/')
        .next()
        .and_then(|base| base.strip_suffix(".rb"))
        .unwrap_or_default();
    let last = public_name(fqn).rsplit("::").next().unwrap_or_default();
    !stem.is_empty() && stem.replace('_', "").eq_ignore_ascii_case(last)
}

/// Leading directories two paths share, and one more for the same file.
fn closeness(one: &str, other: &str) -> usize {
    if one == other {
        return usize::MAX;
    }
    let dirs = |path: &str| -> Vec<String> {
        let mut parts: Vec<String> = path.split('/').map(str::to_string).collect();
        parts.pop();
        parts
    };
    dirs(one)
        .iter()
        .zip(dirs(other).iter())
        .take_while(|(a, b)| a == b)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_prefers_the_deepest_shared_directory() {
        let model = vec!["/r/record/test/models/post.rb".to_string()];
        let fake = vec!["/r/pack/test/fake_models.rb".to_string()];
        let from = "/r/record/test/cases/batches_test.rb";
        let found = nearest(
            "Post",
            [(&model, "model"), (&fake, "fake")].into_iter(),
            from,
        );
        assert_eq!(found, ["model"]);
    }

    #[test]
    fn equally_near_the_file_named_for_the_class_wins() {
        let model = vec!["/r/record/models/post.rb".to_string()];
        let fake = vec!["/r/pack/fake_models.rb".to_string()];
        let from = "/r/guides/elsewhere.rb";
        let found = nearest(
            "Post",
            [(&model, "model"), (&fake, "fake")].into_iter(),
            from,
        );
        assert_eq!(found, ["model"]);
    }

    #[test]
    fn equally_near_and_equally_named_is_a_tie() {
        let model = vec!["/r/record/models/post.rb".to_string()];
        let fake = vec!["/r/pack/post.rb".to_string()];
        let from = "/r/guides/elsewhere.rb";
        let found = nearest(
            "Post",
            [(&model, "model"), (&fake, "fake")].into_iter(),
            from,
        );
        assert_eq!(found, ["model", "fake"]);
    }

    #[test]
    fn a_variant_key_reads_as_the_name_it_splits() {
        assert_eq!(public_name("A::Post@2"), "A::Post");
        assert_eq!(public_name("A::Post"), "A::Post");
    }
}
