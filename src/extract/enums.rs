//! What `enum` declares, read from its arguments.
//!
//! Rails spells an enum two ways, and both are read. Rails 7's `enum :status,
//! {…}, prefix: true` names the attribute first, members second (or as the
//! remaining keywords), options as keywords. Rails 6's `enum status: {…},
//! _prefix: true` makes every keyword but the underscored options an enum of
//! its own. The method names each member gets are ActiveRecord::Enum's rule,
//! `"#{prefix}#{label}#{suffix}"`, not a guess.

use ruby_prism::Node;

use super::literal_name;

/// One enum attribute and what it generates.
#[derive(Debug, PartialEq)]
pub(super) struct Enum {
    pub(super) attribute: String,
    /// Where the attribute is written, as a byte offset.
    pub(super) at: usize,
    /// Each member's label and where it is written.
    pub(super) members: Vec<(String, usize)>,
    pub(super) prefix: Affix,
    pub(super) suffix: Affix,
    pub(super) scopes: bool,
    pub(super) instance_methods: bool,
}

/// A `prefix:` or `suffix:` option.
#[derive(Debug, PartialEq, Clone)]
pub(super) enum Affix {
    None,
    /// `true`: the attribute's own name.
    Attribute,
    Named(String),
    /// Computed: the member names cannot be spelled.
    Unknown,
}

/// Rails 7's option keys. The Rails 6 spelling writes each with a leading `_`.
const OPTIONS: [&str; 6] = [
    "prefix",
    "suffix",
    "scopes",
    "default",
    "instance_methods",
    "validate",
];

/// Every enum the call declares. Empty when it names no attribute literally.
pub(super) fn declared(args: &[Node<'_>]) -> Vec<Enum> {
    match args.first() {
        Some(first) if literal_name(first).is_some() => rails7(args).into_iter().collect(),
        Some(first) if first.as_keyword_hash_node().is_some() => rails6(first),
        _ => Vec::new(),
    }
}

fn rails7(args: &[Node<'_>]) -> Option<Enum> {
    let first = args.first()?;
    let mut decl = Enum::new(literal_name(first)?, first.location().start_offset());
    let mut keyword_members = Vec::new();
    for arg in &args[1..] {
        if let Some(hash) = arg.as_keyword_hash_node() {
            for (key, at, value) in pairs(&hash.elements()) {
                if OPTIONS.contains(&key.as_str()) {
                    decl.option(&key, &value);
                } else {
                    keyword_members.push((key, at));
                }
            }
        } else {
            decl.members.extend(members(arg));
        }
    }
    // `enum :status, active: 0, archived: 1` — the members are the keywords
    // that are not options, when no positional values were given.
    if decl.members.is_empty() {
        decl.members = keyword_members;
    }
    Some(decl)
}

fn rails6(hash: &Node<'_>) -> Vec<Enum> {
    let Some(hash) = hash.as_keyword_hash_node() else {
        return Vec::new();
    };
    let mut options = Vec::new();
    let mut decls = Vec::new();
    for (key, at, value) in pairs(&hash.elements()) {
        // Rails reads only the underscored spelling here; the bare one is
        // read too, since an enum named `prefix` is not what anyone meant.
        let option = key.strip_prefix('_').unwrap_or(&key);
        match option {
            _ if OPTIONS.contains(&option) => options.push((option.to_string(), value)),
            _ => {
                let mut decl = Enum::new(key, at);
                decl.members = members(&value);
                decls.push(decl);
            }
        }
    }
    for decl in &mut decls {
        for (key, value) in &options {
            decl.option(key, value);
        }
    }
    decls
}

/// A hash's literal keys, where each is written, and its value.
fn pairs<'pr>(elements: &ruby_prism::NodeList<'pr>) -> Vec<(String, usize, Node<'pr>)> {
    elements
        .iter()
        .filter_map(|element| {
            let assoc = element.as_assoc_node()?;
            let key = assoc.key();
            let name = literal_name(&key)?;
            Some((name, key.location().start_offset(), assoc.value()))
        })
        .collect()
}

/// A literal hash's keys or a literal array's elements; nothing when computed.
fn members(node: &Node<'_>) -> Vec<(String, usize)> {
    if let Some(hash) = node.as_hash_node() {
        return pairs(&hash.elements())
            .into_iter()
            .map(|(name, at, _)| (name, at))
            .collect();
    }
    if let Some(array) = node.as_array_node() {
        return array
            .elements()
            .iter()
            .filter_map(|e| Some((literal_name(&e)?, e.location().start_offset())))
            .collect();
    }
    Vec::new()
}

impl Enum {
    fn new(attribute: String, at: usize) -> Enum {
        Enum {
            attribute,
            at,
            members: Vec::new(),
            prefix: Affix::None,
            suffix: Affix::None,
            scopes: true,
            instance_methods: true,
        }
    }

    fn option(&mut self, key: &str, value: &Node<'_>) {
        let affix = || {
            if value.as_true_node().is_some() {
                Affix::Attribute
            } else if value.as_false_node().is_some() || value.as_nil_node().is_some() {
                Affix::None
            } else {
                literal_name(value).map_or(Affix::Unknown, Affix::Named)
            }
        };
        match key {
            "prefix" => self.prefix = affix(),
            "suffix" => self.suffix = affix(),
            "scopes" => self.scopes = value.as_false_node().is_none(),
            "instance_methods" => self.instance_methods = value.as_false_node().is_none(),
            _ => {}
        }
    }

    /// The name a member's methods are built on, or `None` when a computed
    /// affix makes it unknowable.
    pub(super) fn method_name(&self, label: &str) -> Option<String> {
        let affix = |affix: &Affix| match affix {
            Affix::None => Some(None),
            Affix::Attribute => Some(Some(self.attribute.clone())),
            Affix::Named(name) => Some(Some(name.clone())),
            Affix::Unknown => None,
        };
        let prefix = affix(&self.prefix)?
            .map(|p| format!("{p}_"))
            .unwrap_or_default();
        let suffix = affix(&self.suffix)?
            .map(|s| format!("_{s}"))
            .unwrap_or_default();
        Some(format!("{prefix}{}{suffix}", friendly(label)))
    }
}

/// ActiveRecord::Enum's `label.to_s.gsub(/[\W&&[:ascii:]]+/, "_")`: a run of
/// ASCII punctuation or space becomes one `_`.
fn friendly(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut in_run = false;
    for c in label.chars() {
        if c.is_ascii() && !(c.is_ascii_alphanumeric() || c == '_') {
            if !in_run {
                out.push('_');
            }
            in_run = true;
        } else {
            out.push(c);
            in_run = false;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enums(src: &str) -> Vec<Enum> {
        let parsed = ruby_prism::parse(src.as_bytes());
        let node = parsed.node();
        let program = node.as_program_node().unwrap();
        let call = program
            .statements()
            .body()
            .iter()
            .next()
            .unwrap()
            .as_call_node()
            .unwrap();
        let args: Vec<Node<'_>> = call.arguments().unwrap().arguments().iter().collect();
        declared(&args)
    }

    fn names(decl: &Enum) -> Vec<String> {
        decl.members
            .iter()
            .filter_map(|(label, _)| decl.method_name(label))
            .collect()
    }

    #[test]
    fn affixes_follow_rails_rule() {
        let [decl] = &enums("enum :plan, [:free, :paid], prefix: true, suffix: :tier")[..] else {
            panic!("one enum");
        };
        assert_eq!(names(decl), ["plan_free_tier", "plan_paid_tier"]);
    }

    #[test]
    fn a_rails6_call_declares_each_keyword_with_the_shared_options() {
        let decls =
            enums("enum status: { active: 0 }, kind: [:basic], _suffix: true, _scopes: false");
        let got: Vec<(&str, Vec<String>, bool)> = decls
            .iter()
            .map(|d| (d.attribute.as_str(), names(d), d.scopes))
            .collect();
        assert_eq!(
            got,
            [
                ("status", vec!["active_status".to_string()], false),
                ("kind", vec!["basic_kind".to_string()], false),
            ]
        );
    }

    #[test]
    fn keyword_members_count_only_without_positional_values() {
        let [decl] = &enums("enum :status, active: 0, archived: 1, default: :active")[..] else {
            panic!("one enum");
        };
        assert_eq!(names(decl), ["active", "archived"]);
    }

    #[test]
    fn a_computed_affix_spells_nothing() {
        let [decl] = &enums("enum :status, { active: 0 }, prefix: name")[..] else {
            panic!("one enum");
        };
        assert!(names(decl).is_empty());
    }

    #[test]
    fn a_label_that_is_not_a_word_is_made_one() {
        assert_eq!(friendly("on hold"), "on_hold");
        assert_eq!(friendly("in-review / done"), "in_review_done");
        assert_eq!(friendly("café"), "café");
    }
}
