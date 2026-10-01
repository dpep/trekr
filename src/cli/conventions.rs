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

/// A library that calls a method by a name no call site writes: who, why,
/// and the line that names it, when one does.
pub(super) struct Convention {
    pub(super) by: &'static str,
    pub(super) reason: String,
    pub(super) at: Option<(String, u32)>,
}

const SERIALIZER: &str = "ActiveModel::Serializer";

/// Where `attributes :x` (or `has_one :x`, …) names the attribute that
/// ActiveModel::Serializers 0.8/0.9 calls `include_x?` for, when `owner` is a
/// serializer or a module one mixes in: the symbol, in the serializer's own
/// file or an ancestor's. None when the method is no such hook, or the
/// indexed gem builds no `include_` methods (0.10 does not).
pub(super) fn serializer_include(
    tree: &Tree,
    owner: &str,
    name: &str,
    symbols: &mut Symbols,
) -> Option<Convention> {
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
                    return Some(Convention {
                        by: "ActiveModel::Serializers",
                        reason: format!(
                            "named only by a symbol ActiveModel::Serializers calls it for, at {}:{line}",
                            site.path
                        ),
                        at: Some((site.path, line)),
                    });
                }
            }
        }
    }
    None
}

const ASSIGNMENT: &str = "ActiveModel::AttributeAssignment";

/// Whether `name` is a public writer that Active Model's `assign_attributes`
/// may call by the key it is handed — `new(mode: …)`, `update(…)`, a form's
/// params — on `owner` or the classes that mix it in (DEC-364).
pub(super) fn assigned_writer(tree: &Tree, owner: &str, name: &str, public: bool) -> bool {
    let Some(attribute) = name.strip_suffix('=') else {
        return false;
    };
    let writer = attribute.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
        && attribute
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_');
    public
        && writer
        && (tree.inherits(owner, ASSIGNMENT)
            || tree
                .includers_of(owner)
                .iter()
                .any(|class| tree.inherits(class, ASSIGNMENT)))
}

/// Whether Thor runs this public method by its name: a `Thor` subclass's
/// public methods are its commands (`desc "prune"`, then `cli prune`), and
/// a `Thor::Group`'s — every Rails generator's — are run in turn, as are a
/// module's that such a class mixes in (DEC-371).
pub(super) fn thor_command(tree: &Tree, owner: &str, public: bool) -> Option<Convention> {
    if !public {
        return None;
    }
    let mut classes = vec![owner.to_string()];
    if !tree.inherits(owner, "Thor") && !tree.inherits(owner, "Thor::Group") {
        classes = tree.includers_of(owner);
    }
    let group = classes.iter().any(|c| tree.inherits(c, "Thor::Group"));
    if !group && !classes.iter().any(|c| tree.inherits(c, "Thor")) {
        return None;
    }
    Some(Convention {
        by: "Thor",
        reason: if group {
            "a Thor::Group's public method, which Thor runs in turn".to_string()
        } else {
            "a Thor command, which Thor runs by its name".to_string()
        },
        at: None,
    })
}
