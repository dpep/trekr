//! Which table a model reads, worked out as Rails' `compute_table_name` does,
//! from what the tree knows (DEC-481).

use crate::tree::Tree;

#[derive(Debug, PartialEq)]
pub(crate) enum Model {
    /// The model's table, and the class it is inherited from when it shares
    /// that class's table (single-table inheritance).
    Table { name: String, base: Option<String> },
    /// A class for models to inherit, with no table of its own.
    Abstract,
}

/// The table of the class `fqn`, `None` when it is not an Active Record
/// model. `read` is a file's text, for the string a namespace's
/// `def self.table_name_prefix` returns.
pub(crate) fn of(tree: &Tree, fqn: &str, read: &dyn Fn(&str) -> Option<String>) -> Option<Model> {
    if !tree.inherits(fqn, "ActiveRecord::Base") {
        return None;
    }
    if is_abstract(tree, fqn) {
        return Some(Model::Abstract);
    }
    let chain = tree.superclass_chain(fqn);
    for (at, class) in chain.iter().enumerate() {
        let base = (at > 0).then(|| class.clone());
        if let Some(table) = tree.table_name_of(class) {
            return Some(Model::Table {
                name: table.to_string(),
                base,
            });
        }
        // A class whose parent has no table is where the table is named;
        // below it, every subclass shares it.
        let parent = chain.get(at + 1);
        if parent.is_none_or(|p| p == "ActiveRecord::Base" || is_abstract(tree, p)) {
            return Some(Model::Table {
                name: conventional(tree, class, read),
                base,
            });
        }
    }
    None
}

/// `ApplicationRecord` by convention, which every app generated since Rails 5
/// declares abstract.
fn is_abstract(tree: &Tree, fqn: &str) -> bool {
    fqn == "ApplicationRecord" || tree.is_abstract_model(fqn)
}

/// `Admin::UserReport` → `admin_user_reports` under a module whose
/// `table_name_prefix` is `admin_`; `Post::Comment` → `post_comments` when
/// `Post` is a model itself.
fn conventional(tree: &Tree, class: &str, read: &dyn Fn(&str) -> Option<String>) -> String {
    let (namespace, short) = match class.rsplit_once("::") {
        Some((namespace, short)) => (Some(namespace), short),
        None => (None, class),
    };
    let contained = namespace
        .filter(|n| tree.kind_of(n) == Some("class"))
        .and_then(|n| match of(tree, n, read) {
            Some(Model::Table { name, .. }) => {
                Some(format!("{}_", crate::inflect::singular(&name)))
            }
            _ => None,
        })
        .unwrap_or_default();
    let own = crate::inflect::plural(&crate::scan::near::underscore(short));
    let prefix = affix(tree, class, "table_name_prefix", read);
    let suffix = affix(tree, class, "table_name_suffix", read);
    format!("{prefix}{contained}{own}{suffix}")
}

/// What the nearest enclosing namespace that defines `def self.<name>`
/// returns, as Rails asks the first of `module_parents` that answers it.
fn affix(tree: &Tree, class: &str, name: &str, read: &dyn Fn(&str) -> Option<String>) -> String {
    let mut scope = class;
    while let Some((namespace, _)) = scope.rsplit_once("::") {
        if let Some(def) = tree
            .lookup(namespace, true, name)
            .filter(|def| def.owner == namespace)
        {
            return read(&def.site.path)
                .and_then(|text| returned_literal(&text, def.site.line))
                .unwrap_or_default();
        }
        scope = namespace;
    }
    String::new()
}

/// The string literal a short method starting on `line` returns:
/// `def self.table_name_prefix\n  "admin_"\nend`, or written on one line.
fn returned_literal(text: &str, line: u32) -> Option<String> {
    let body: Vec<&str> = text
        .lines()
        .skip(line.checked_sub(1)? as usize)
        .take(3)
        .collect();
    let body = body.join("\n");
    let after = &body[body.find("table_name_")?..];
    let open = after.find(['"', '\''])?;
    let quote = after[open..].chars().next()?;
    let rest = &after[open + 1..];
    let close = rest.find(quote)?;
    Some(rest[..close].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_literal_a_prefix_method_returns() {
        let text = "module Admin\n  def self.table_name_prefix\n    'admin_'\n  end\nend\n";
        assert_eq!(returned_literal(text, 2).as_deref(), Some("admin_"));
        let text = "module Shop\n  def self.table_name_prefix = \"shop_\"\nend\n";
        assert_eq!(returned_literal(text, 2).as_deref(), Some("shop_"));
        let text = "module Kit\n  def self.table_name_prefix\n    PREFIX\n  end\nend\n";
        assert_eq!(
            returned_literal(text, 2),
            None,
            "a computed prefix is not guessed"
        );
    }
}
