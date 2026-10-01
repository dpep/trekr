//! Library conventions that call a method by a name no call site writes, each
//! keyed on the library's own code being in the tree, so a version that does
//! not have the convention does not claim it (DEC-362).

use std::collections::{HashMap, HashSet};

use crate::tree::Tree;

/// The symbols each file writes, read once per `--dead` run.
#[derive(Default)]
pub(super) struct Symbols {
    by_path: HashMap<String, Vec<(String, u32)>>,
}

impl Symbols {
    /// The first line of `path` with the symbol `:name`.
    fn line_of(&mut self, path: &str, name: &str) -> Option<u32> {
        let symbols = self.by_path.entry(path.to_string()).or_insert_with(|| {
            std::fs::read(path)
                .map(|source| {
                    crate::extract::symbol_literals(&source)
                        .into_iter()
                        .map(|(name, pos, _)| (name, pos.line))
                        .collect()
                })
                .unwrap_or_default()
        });
        symbols
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, line)| *line)
    }
}

const SERIALIZER: &str = "ActiveModel::Serializer";

/// Where `attributes :x` (or `has_one :x`, …) names the attribute that
/// ActiveModel::Serializers 0.8/0.9 calls `include_x?` for, when `owner` is a
/// serializer or a module one mixes in: `(path, line)` of the symbol, in the
/// serializer's own file or an ancestor's. None when the method is no such
/// hook, or the indexed gem builds no `include_` methods (0.10 does not).
pub(super) fn serializer_include(
    tree: &Tree,
    owner: &str,
    name: &str,
    symbols: &mut Symbols,
) -> Option<(String, u32)> {
    let attribute = name.strip_prefix("include_")?.strip_suffix('?')?;
    tree.lookup(SERIALIZER, true, "define_include_method")?;
    // A serializer, or a mixin whose includers are: the hook is called on
    // the serializer, which is where its attributes are declared.
    let serializers: Vec<String> = if tree.inherits(owner, SERIALIZER) {
        vec![owner.to_string()]
    } else {
        tree.includers_of(owner)
            .into_iter()
            .filter(|class| tree.inherits(class, SERIALIZER))
            .collect()
    };
    let mut seen = HashSet::new();
    for serializer in &serializers {
        for ancestor in &tree.ancestors(serializer).chain {
            if ancestor == SERIALIZER || !seen.insert(ancestor.clone()) {
                continue;
            }
            for site in tree.sites(ancestor) {
                let path = tree.site_path(&site.path);
                if let Some(line) = symbols.line_of(&path, attribute) {
                    return Some((site.path, line));
                }
            }
        }
    }
    None
}
