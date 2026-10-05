//! A database schema as the app dumps it: `db/schema.rb` or `db/structure.sql`,
//! read into one shape (DEC-480).
//!
//! Two readers answer from it. Extraction makes each column's attribute
//! methods, typed from its SQL type (DEC-022). A hover reads the file again
//! when asked and shows the table — type, null, default, indexes — the way a
//! doc comment is read rather than stored (DEC-052).
//!
//! Everything here is a pure function of a file's text.

pub(crate) mod model;
pub(crate) mod ruby;
pub(crate) mod sql;

use crate::core::Pos;
use std::path::Path;

/// One table, as the schema declares it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Table {
    pub(crate) name: String,
    /// Where `create_table` / `CREATE TABLE` starts.
    pub(crate) line: u32,
    /// The columns as declared, in order. A `schema.rb` primary key is
    /// implicit and not among them; `primary_key` names it.
    pub(crate) columns: Vec<Column>,
    pub(crate) primary_key: PrimaryKey,
    pub(crate) indexes: Vec<Index>,
    /// A `CREATE [MATERIALIZED] VIEW`: its columns are its select list's.
    pub(crate) view: Option<View>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct View {
    /// Select-list items whose column name is not written down: `*`, or an
    /// expression with no `AS`.
    pub(crate) unread: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Column {
    pub(crate) name: String,
    /// The type as the schema spells it: `string`, `character varying(60)`.
    pub(crate) sql_type: String,
    /// The class its reader returns, when one class always does.
    pub(crate) class: Option<&'static str>,
    pub(crate) null: bool,
    /// As written, casts and all; `None` when the schema states none.
    pub(crate) default: Option<String>,
    /// The column's name in the schema.
    pub(crate) pos: Pos,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) enum PrimaryKey {
    /// The columns named, with the type when the schema says it and the
    /// column is not itself declared (`create_table "x", id: :uuid`).
    Columns(Vec<String>, Option<String>),
    /// `id: false`, or a table no constraint gives one.
    #[default]
    None,
}

impl PrimaryKey {
    pub(crate) fn names(&self) -> &[String] {
        match self {
            PrimaryKey::Columns(names, _) => names,
            PrimaryKey::None => &[],
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Index {
    pub(crate) columns: Vec<String>,
    pub(crate) unique: bool,
}

impl Table {
    pub(crate) fn column(&self, name: &str) -> Option<&Column> {
        self.columns.iter().find(|c| c.name == name)
    }

    /// The columns Rails makes attribute methods for here. The primary key is
    /// left to `ActiveRecord::Base`, as it always has been for `schema.rb`,
    /// whose dump does not list it.
    pub(crate) fn attribute_columns(&self) -> impl Iterator<Item = &Column> {
        let key = self.primary_key.names();
        self.columns.iter().filter(move |c| !key.contains(&c.name))
    }
}

/// The class a column of this Rails type reads as.
///
/// Only where it is determinate and the class is one core knows. `boolean` is
/// deliberately absent: `true` and `false` are different classes and neither is
/// a useful receiver. `decimal` is BigDecimal, which core.rb declares.
pub(crate) fn column_class(rails_type: &str) -> Option<&'static str> {
    Some(match rails_type {
        "string" | "text" | "citext" | "binary" | "uuid" | "inet" | "cidr" => "String",
        "integer" | "bigint" | "serial" | "bigserial" | "primary_key" => "Integer",
        "float" => "Float",
        "decimal" | "numeric" | "money" => "BigDecimal",
        "datetime" | "timestamp" | "timestamptz" | "time" => "Time",
        "date" => "Date",
        "json" | "jsonb" | "hstore" => "Hash",
        _ => return None,
    })
}

/// Is `t.<name>` in a `create_table` block a column declaration?
///
/// `t.index`, `t.check_constraint` and friends declare something else.
pub(crate) fn is_column_type(name: &str) -> bool {
    column_class(name).is_some()
        || matches!(name, "boolean" | "virtual" | "column" | "interval" | "enum")
}

/// Every table a schema dump declares, read as the file's kind says.
pub(crate) fn tables_in(path: &str, src: &[u8]) -> Vec<Table> {
    use crate::scan::Reader;
    match Reader::of(path) {
        Reader::StructureSql => sql::tables(src),
        Reader::Ruby | Reader::Erb | Reader::Rabl => ruby::tables(src),
    }
}

/// The dumps of the app a file belongs to: the nearest directory above it
/// that has a `db/` with one, so an engine or a monorepo's app finds its own.
pub(crate) fn dumps_near(root: &Path, site: Option<&str>) -> Vec<String> {
    let root_text = root.to_string_lossy();
    let relative = site
        .and_then(|s| s.strip_prefix(&*root_text))
        .map(|s| s.trim_start_matches('/'))
        .unwrap_or("");
    let mut dir = relative.rsplit_once('/').map_or("", |(dir, _)| dir);
    loop {
        let app = if dir.is_empty() {
            String::new()
        } else {
            format!("{dir}/")
        };
        let dumps = crate::scan::schema_dumps(root, &app);
        if !dumps.is_empty() || dir.is_empty() {
            return dumps;
        }
        dir = dir.rsplit_once('/').map_or("", |(up, _)| up);
    }
}

/// Is this file a schema dump: `db/schema.rb`, `db/structure.sql`, or a
/// second database's `db/<name>_schema.rb` / `_structure.sql`?
pub(crate) fn is_dump(path: &str) -> bool {
    let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
    crate::scan::is_structure_sql(path)
        || (dir == "db" || dir.ends_with("/db"))
            && (name == "schema.rb" || name.ends_with("_schema.rb"))
}

/// Which of an app's schema files Rails reads, and so which one trekr does.
///
/// Both are often committed while an app moves from one format to the other,
/// and they then disagree. Rails loads the one `schema_format` names, which
/// defaults to `:ruby`; `config` is the text of `config/application.rb`.
pub(crate) fn sql_is_the_schema(config: Option<&str>) -> bool {
    config.is_some_and(|text| {
        text.lines()
            .map(|line| line.split('#').next().unwrap_or(""))
            .any(|code| {
                let code: String = code.split_whitespace().collect();
                code.contains("active_record.schema_format=:sql")
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_format_follows_the_apps_config() {
        assert!(sql_is_the_schema(Some(
            "module App\n  class Application\n    config.active_record.schema_format = :sql\n  end\nend\n"
        )));
        assert!(!sql_is_the_schema(Some(
            "# config.active_record.schema_format = :sql\n"
        )));
        assert!(!sql_is_the_schema(Some(
            "config.active_record.schema_format = :ruby\n"
        )));
        assert!(!sql_is_the_schema(None), "Rails' default is schema.rb");
    }
}
