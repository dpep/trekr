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
    /// The class a reader returns, when the macro fixes it.
    pub(super) returns: Option<&'static str>,
}

impl Generated {
    fn reader(name: impl Into<String>) -> Generated {
        Generated {
            name: name.into(),
            singleton: false,
            writer: false,
            returns: None,
        }
    }

    fn writer(name: impl Into<String>) -> Generated {
        Generated {
            name: name.into(),
            singleton: false,
            writer: true,
            returns: None,
        }
    }

    fn class_method(name: impl Into<String>) -> Generated {
        Generated {
            name: name.into(),
            singleton: true,
            writer: false,
            returns: None,
        }
    }

    fn class_writer(name: impl Into<String>) -> Generated {
        Generated {
            name: name.into(),
            singleton: true,
            writer: true,
            returns: None,
        }
    }

    fn returning(mut self, class: &'static str) -> Generated {
        self.returns = Some(class);
        self
    }
}

/// An accessor pair, which most of these macros are.
fn accessor(name: &str) -> Vec<Generated> {
    vec![
        Generated::reader(name),
        Generated::writer(format!("{name}=")),
    ]
}

/// The dirty-tracking methods ActiveModel gives an attribute — the ones code
/// calls. The rest of the family (`x_change`, `restore_x!`, …) went
/// uncalled in discourse and mastodon (DEC-111).
pub(super) fn dirty(attribute: &str) -> Vec<Generated> {
    [
        format!("{attribute}_changed?"),
        format!("{attribute}_was"),
        format!("{attribute}_previously_changed?"),
        format!("{attribute}_before_last_save"),
        format!("saved_change_to_{attribute}?"),
        format!("will_save_change_to_{attribute}?"),
    ]
    .into_iter()
    .map(Generated::reader)
    .collect()
}

/// A store accessor's methods: the accessor pair and the dirty tracking
/// ActiveRecord::Store defines for it by hand.
pub(super) fn store_accessor(key: &str) -> Vec<Generated> {
    let mut out = accessor(key);
    out.extend(
        [
            format!("{key}_changed?"),
            format!("{key}_change"),
            format!("{key}_was"),
            format!("saved_change_to_{key}?"),
            format!("saved_change_to_{key}"),
            format!("{key}_before_last_save"),
        ]
        .into_iter()
        .map(Generated::reader),
    );
    out
}

/// Does an `instance_*: false` option drop this method? `class_attribute`
/// and the `mattr` family take `instance_accessor:`, `instance_reader:` and
/// `instance_writer:`; `class_attribute` also `instance_predicate:`, which
/// drops the class's predicate as well as the instance's.
pub(super) fn dropped_by(made: &Generated, off: impl Fn(&str) -> bool) -> bool {
    let predicate = made.name.ends_with('?');
    if predicate && off("instance_predicate") {
        return true;
    }
    if made.singleton {
        return false;
    }
    let accessor = off("instance_accessor");
    if made.writer {
        accessor || off("instance_writer")
    } else {
        accessor || off("instance_reader")
    }
}

/// The argument a macro takes when none is written: `has_secure_password`
/// is `has_secure_password :password`.
pub(super) fn default_argument(macro_name: &str) -> Option<&'static str> {
    match macro_name {
        "has_secure_password" => Some("password"),
        "has_secure_token" => Some("token"),
        _ => None,
    }
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
        // Its reader is a relation of the associated records (DEC-444).
        "has_many" | "has_and_belongs_to_many" => {
            let singular = crate::inflect::singular(arg);
            let mut out = vec![
                Generated::reader(arg).returning("::ActiveRecord::Associations::CollectionProxy"),
                Generated::writer(format!("{arg}=")),
            ];
            out.extend(accessor(&format!("{singular}_ids")));
            out
        }

        // A singular association brings the build/create family with it, and
        // a `belongs_to` its own change tracking.
        "has_one" | "belongs_to" => {
            let mut out = accessor(arg);
            out.push(Generated::writer(format!("build_{arg}")));
            out.push(Generated::writer(format!("create_{arg}")));
            out.push(Generated::writer(format!("create_{arg}!")));
            out.push(Generated::reader(format!("reload_{arg}")));
            out.push(Generated::reader(format!("reset_{arg}")));
            if macro_name == "belongs_to" {
                out.push(Generated::reader(format!("{arg}_changed?")));
                out.push(Generated::reader(format!("{arg}_previously_changed?")));
            }
            out
        }

        // Active Storage: a proxy reader, the association to the attachment
        // records and through them to the blobs, and a preloading scope.
        "has_one_attached" => vec![
            Generated::reader(arg).returning("::ActiveStorage::Attached::One"),
            Generated::writer(format!("{arg}=")),
            Generated::reader(format!("{arg}_attachment")).returning("::ActiveStorage::Attachment"),
            Generated::writer(format!("{arg}_attachment=")),
            Generated::reader(format!("{arg}_blob")).returning("::ActiveStorage::Blob"),
            Generated::writer(format!("{arg}_blob=")),
            Generated::class_method(format!("with_attached_{arg}")),
        ],
        "has_many_attached" => vec![
            Generated::reader(arg).returning("::ActiveStorage::Attached::Many"),
            Generated::writer(format!("{arg}=")),
            Generated::reader(format!("{arg}_attachments")),
            Generated::writer(format!("{arg}_attachments=")),
            Generated::reader(format!("{arg}_blobs")),
            Generated::writer(format!("{arg}_blobs=")),
            Generated::class_method(format!("with_attached_{arg}")),
        ],

        "accepts_nested_attributes_for" => vec![Generated::writer(format!("{arg}_attributes="))],

        // ActiveModel::SecurePassword, for `has_secure_password :arg`. The
        // reset token's three methods are ActiveRecord's, which is where the
        // macro is nearly always called.
        "has_secure_password" => {
            let mut out = vec![
                Generated::reader(arg),
                Generated::writer(format!("{arg}=")),
                Generated::reader(format!("authenticate_{arg}")),
                Generated::reader(format!("{arg}_salt")),
                Generated::reader(format!("{arg}_reset_token")),
                Generated::reader(format!("{arg}_reset_token_expires_in")),
                Generated::class_method(format!("find_by_{arg}_reset_token")),
                Generated::class_method(format!("find_by_{arg}_reset_token!")),
            ];
            out.extend(accessor(&format!("{arg}_confirmation")));
            out.extend(accessor(&format!("{arg}_challenge")));
            if arg == "password" {
                out.push(Generated::reader("authenticate"));
            }
            out
        }
        "has_secure_token" => vec![Generated::reader(format!("regenerate_{arg}"))],

        // A scope is a class method, and returns a relation (DEC-444).
        "scope" => vec![Generated::class_method(arg).returning("::ActiveRecord::Relation")],

        // `define_model_callbacks :save` is where `before_save`, `around_save`
        // and `after_save` come from. ActiveModel writes them with
        // `klass.define_singleton_method("before_#{callback}")` inside a `def`,
        // so nothing in the *defining* file states the names — the call does.
        // `only:` narrows the set and is applied by the caller.
        "define_model_callbacks" => vec![
            Generated::class_method(format!("before_{arg}")),
            Generated::class_method(format!("around_{arg}")),
            Generated::class_method(format!("after_{arg}")),
        ],

        // Readable and writable from both sides, plus the predicate.
        "class_attribute" => vec![
            Generated::reader(arg),
            Generated::writer(format!("{arg}=")),
            Generated::reader(format!("{arg}?")),
            Generated::class_method(arg),
            Generated::class_writer(format!("{arg}=")),
            Generated::class_method(format!("{arg}?")),
        ],
        "mattr_accessor" | "cattr_accessor" | "thread_mattr_accessor" | "thread_cattr_accessor" => {
            let mut out = accessor(arg);
            out.push(Generated::class_method(arg));
            out.push(Generated::class_writer(format!("{arg}=")));
            out
        }
        "mattr_reader" | "cattr_reader" | "thread_mattr_reader" | "thread_cattr_reader" => {
            vec![Generated::reader(arg), Generated::class_method(arg)]
        }
        "mattr_writer" | "cattr_writer" | "thread_mattr_writer" | "thread_cattr_writer" => vec![
            Generated::writer(format!("{arg}=")),
            Generated::class_writer(format!("{arg}=")),
        ],

        // An explicitly declared attribute, and an alias for one: an
        // attribute like any column, with its query method and dirty
        // tracking. An alias names only its first argument (the caller's
        // job); the second is the attribute it reads.
        "attribute" | "alias_attribute" => {
            let mut out = accessor(arg);
            out.push(Generated::reader(format!("{arg}?")));
            out.extend(dirty(arg));
            out
        }

        _ => Vec::new(),
    }
}

/// Does this macro name a class in its argument, and which?
///
/// `belongs_to :user` gives `user` a determinate type, which makes it a
/// *receiver* source and not merely a method. `has_many` does not: its reader
/// returns a relation, not the associated class.
///
/// Only a singular association: `has_many :clients, class_name: "Client"`
/// reads a collection, and naming its element does not make it one.
pub(super) fn associated_class(
    macro_name: &str,
    arg: &str,
    class_name: Option<&str>,
) -> Option<String> {
    matches!(macro_name, "has_one" | "belongs_to")
        .then(|| class_name.map_or_else(|| camelize(arg), str::to_string))
}

/// The model a collection association's records are, which its relation
/// hands a name it lacks to (DEC-444): `class_name:`, else the singular of
/// its name. `None` when a `source:` names another association's class.
pub(super) fn collection_class(
    macro_name: &str,
    arg: &str,
    class_name: Option<&str>,
    source: bool,
) -> Option<String> {
    if !matches!(macro_name, "has_many" | "has_and_belongs_to_many") {
        return None;
    }
    match class_name {
        Some(class) => Some(class.to_string()),
        None if source => None,
        None => Some(camelize(&crate::inflect::singular(arg))),
    }
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
    camelize(&crate::inflect::singular(table))
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
                "#reload_user",
                "#reset_user",
                "#user_changed?",
                "#user_previously_changed?"
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
            associated_class("belongs_to", "blog_post", None).as_deref(),
            Some("BlogPost")
        );
        assert_eq!(
            associated_class("belongs_to", "author", Some("Person")).as_deref(),
            Some("Person")
        );
        assert_eq!(
            associated_class("has_many", "widgets", None),
            None,
            "a collection reader returns a relation, not the associated class"
        );
        assert_eq!(
            associated_class("has_many", "clients_of_firm", Some("Client")),
            None,
            "nor does naming its element's class"
        );
    }

    #[test]
    fn has_secure_password_names_its_attribute_throughout() {
        let made = names("has_secure_password", "pin");
        for name in [
            "#pin=",
            "#authenticate_pin",
            "#pin_confirmation=",
            ".find_by_pin_reset_token",
        ] {
            assert!(made.iter().any(|m| m == name), "{name} in {made:?}");
        }
        assert!(
            !made.iter().any(|m| m == "#authenticate"),
            "`authenticate` is only the password's alias"
        );
    }

    #[test]
    fn an_instance_option_drops_the_instance_side() {
        let kept = |off: &[&str]| -> Vec<String> {
            generated("class_attribute", "logger")
                .into_iter()
                .filter(|made| !dropped_by(made, |key| off.contains(&key)))
                .map(|g| format!("{}{}", if g.singleton { "." } else { "#" }, g.name))
                .collect()
        };
        assert_eq!(
            kept(&["instance_writer"]),
            ["#logger", "#logger?", ".logger", ".logger=", ".logger?"]
        );
        assert_eq!(
            kept(&["instance_accessor", "instance_predicate"]),
            [".logger", ".logger="]
        );
    }

    #[test]
    fn a_macro_we_do_not_model_generates_nothing() {
        assert!(generated("validates", "name").is_empty());
    }
}
