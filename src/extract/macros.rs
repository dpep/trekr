//! What Rails' class macros bring into being.
//!
//! `delegate :where, to: :all` defines a real method that no `def` declares.
//! Session 6's audit found this is 82 % of every reference this engine rules
//! out — a method a DSL defines is absent from the index without being absent
//! from the program, so "nothing defines this name" was the weakest thing we
//! could say (DEC-021). Teaching the index what these macros define is what
//! makes that claim sound.
//!
//! The table below is the seam: a macro name and one literal argument in, the
//! methods it creates out. Nothing here is Rails-specific machinery — any DSL
//! family can be added by extending the match.
//!
//! **Literal names only.** `delegate(*methods, to: :x)` and
//! `has_many :"#{name}s"` compute their names at runtime; those refuse rather
//! than guess, and the call site stays visible as an ordinary call (rwr's
//! discipline).

/// One method a macro creates.
#[derive(Debug, PartialEq)]
pub(super) struct Generated {
    pub(super) name: String,
    /// A class method rather than an instance method — `scope` makes one.
    pub(super) singleton: bool,
    /// Takes exactly one argument, so arity checks can rule call sites out.
    pub(super) writer: bool,
}

impl Generated {
    fn reader(name: impl Into<String>) -> Generated {
        Generated {
            name: name.into(),
            singleton: false,
            writer: false,
        }
    }

    fn writer(name: impl Into<String>) -> Generated {
        Generated {
            name: name.into(),
            singleton: false,
            writer: true,
        }
    }

    fn class_method(name: impl Into<String>) -> Generated {
        Generated {
            name: name.into(),
            singleton: true,
            writer: false,
        }
    }
}

/// An accessor pair, which most of these macros are.
fn accessor(name: &str) -> Vec<Generated> {
    vec![
        Generated::reader(name),
        Generated::writer(format!("{name}=")),
    ]
}

/// The methods `macro_name :arg` defines.
///
/// Empty for a macro we do not model — the caller then treats the line as an
/// ordinary call, which is what it looked like before.
pub(super) fn generated(macro_name: &str, arg: &str) -> Vec<Generated> {
    match macro_name {
        // Each delegated name becomes a method in its own right. This is the
        // exact mechanism behind `Topic.where`.
        "delegate" => vec![Generated::reader(arg)],

        // A collection association. `widget_ids` is the one people forget.
        "has_many" | "has_and_belongs_to_many" => {
            let singular = singularize(arg);
            let mut out = accessor(arg);
            out.extend(accessor(&format!("{singular}_ids")));
            out
        }

        // A singular association brings the build/create family with it.
        "has_one" | "belongs_to" => {
            let mut out = accessor(arg);
            out.push(Generated::writer(format!("build_{arg}")));
            out.push(Generated::writer(format!("create_{arg}")));
            out.push(Generated::writer(format!("create_{arg}!")));
            out.push(Generated::reader(format!("reload_{arg}")));
            out
        }

        // A scope is a class method.
        "scope" => vec![Generated::class_method(arg)],

        // Readable and writable from both sides, plus the predicate.
        "class_attribute" => vec![
            Generated::reader(arg),
            Generated::writer(format!("{arg}=")),
            Generated::reader(format!("{arg}?")),
            Generated::class_method(arg),
            Generated {
                name: format!("{arg}="),
                singleton: true,
                writer: true,
            },
            Generated::class_method(format!("{arg}?")),
        ],
        "mattr_accessor" | "cattr_accessor" => {
            let mut out = accessor(arg);
            out.push(Generated::class_method(arg));
            out.push(Generated {
                name: format!("{arg}="),
                singleton: true,
                writer: true,
            });
            out
        }
        "mattr_reader" | "cattr_reader" => {
            vec![Generated::reader(arg), Generated::class_method(arg)]
        }
        "mattr_writer" | "cattr_writer" => vec![
            Generated::writer(format!("{arg}=")),
            Generated {
                name: format!("{arg}="),
                singleton: true,
                writer: true,
            },
        ],

        // An explicitly declared attribute, and an alias for one.
        "attribute" | "store_accessor" | "alias_attribute" => accessor(arg),

        _ => Vec::new(),
    }
}

/// Does this macro name a class in its argument, and which?
///
/// `belongs_to :user` gives `user` a determinate type, which makes it a
/// *receiver* source and not merely a method. `has_many` does not: its reader
/// returns a relation, not the associated class.
pub(super) fn associated_class(macro_name: &str, arg: &str) -> Option<String> {
    matches!(macro_name, "has_one" | "belongs_to").then(|| camelize(arg))
}

/// The class a `db/schema.rb` column type produces.
///
/// Only where it is determinate and the class is one core knows. `boolean` is
/// deliberately absent: `true` and `false` are different classes and neither is
/// a useful receiver. `decimal` is BigDecimal, which core.rb declares.
pub(super) fn column_class(sql_type: &str) -> Option<&'static str> {
    Some(match sql_type {
        "string" | "text" | "citext" | "binary" | "uuid" | "inet" | "cidr" => "String",
        "integer" | "bigint" | "serial" | "bigserial" | "primary_key" => "Integer",
        "float" => "Float",
        "decimal" | "numeric" | "money" => "BigDecimal",
        "datetime" | "timestamp" | "timestamptz" | "time" | "date" => "Time",
        "json" | "jsonb" | "hstore" => "Hash",
        _ => return None,
    })
}

/// Is this `t.<name>` call a column declaration, and does it name the column
/// in its arguments?
///
/// `t.index`, `t.check_constraint` and friends declare something else.
pub(super) fn is_column_type(name: &str) -> bool {
    column_class(name).is_some() || matches!(name, "boolean" | "virtual" | "column" | "interval")
}

/// `posts` → `Post`. Rails' table-to-model convention, which is how a schema
/// attaches to a class without anything linking them.
pub(crate) fn table_to_class(table: &str) -> String {
    // A namespaced table is `admin_users` for `Admin::User` only when an
    // `Admin` module exists, which the extractor cannot know. The flat reading
    // is the common one and the one that is right without cross-file evidence.
    camelize(&singularize(table))
}

/// `blog_post` → `BlogPost`. Rails' own inflection, minus the irregulars: an
/// acronym table would be guessing at a project's `inflections.rb`.
pub(crate) fn camelize(name: &str) -> String {
    name.split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

/// `widgets` → `widget`, for the `_ids` readers. Deliberately crude: the only
/// consumer is a method name, and a wrong guess costs one unfound name rather
/// than a wrong answer.
fn singularize(name: &str) -> String {
    if let Some(stem) = name.strip_suffix("ies") {
        return format!("{stem}y");
    }
    for suffix in ["ses", "xes", "zes", "ches", "shes"] {
        if let Some(stem) = name.strip_suffix(suffix) {
            return format!("{stem}{}", &suffix[..suffix.len() - 2]);
        }
    }
    name.strip_suffix('s').unwrap_or(name).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(macro_name: &str, arg: &str) -> Vec<String> {
        generated(macro_name, arg)
            .into_iter()
            .map(|g| format!("{}{}", if g.singleton { "." } else { "#" }, g.name))
            .collect()
    }

    #[test]
    fn delegate_defines_the_name_it_forwards() {
        assert_eq!(names("delegate", "where"), ["#where"]);
    }

    #[test]
    fn a_collection_association_brings_the_ids_accessors() {
        assert_eq!(
            names("has_many", "widgets"),
            ["#widgets", "#widgets=", "#widget_ids", "#widget_ids="]
        );
    }

    #[test]
    fn a_singular_association_brings_the_build_and_create_family() {
        assert_eq!(
            names("belongs_to", "user"),
            [
                "#user",
                "#user=",
                "#build_user",
                "#create_user",
                "#create_user!",
                "#reload_user"
            ]
        );
    }

    #[test]
    fn a_scope_is_a_class_method() {
        assert_eq!(names("scope", "active"), [".active"]);
    }

    #[test]
    fn class_attribute_is_readable_from_both_sides() {
        assert_eq!(
            names("class_attribute", "logger"),
            [
                "#logger", "#logger=", "#logger?", ".logger", ".logger=", ".logger?"
            ]
        );
    }

    #[test]
    fn only_a_singular_association_names_a_class() {
        assert_eq!(
            associated_class("belongs_to", "blog_post").as_deref(),
            Some("BlogPost")
        );
        assert_eq!(
            associated_class("has_many", "widgets"),
            None,
            "a collection reader returns a relation, not the associated class"
        );
    }

    #[test]
    fn a_macro_we_do_not_model_generates_nothing() {
        assert!(generated("validates", "name").is_empty());
    }

    #[test]
    fn singularizes_well_enough_for_a_method_name() {
        for (plural, singular) in [
            ("widgets", "widget"),
            ("categories", "category"),
            ("boxes", "box"),
            ("addresses", "address"),
            ("person", "person"),
        ] {
            assert_eq!(singularize(plural), singular, "{plural}");
        }
    }
}
