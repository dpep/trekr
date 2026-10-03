//! What a hover shows from the app's schema (DEC-481): a model's table, and a
//! column attribute's facts. Read from the dump when asked, like a doc
//! comment (DEC-052), so null, default and indexes are never stored.

use super::convert::path_to_uri;
use super::state::Session;
use crate::schema::model::Model;
use crate::schema::{Column, Index, PrimaryKey, Table};
use std::path::Path;

/// How many columns a hover lists before it says how many it left out.
const ROWS: usize = 20;
/// And indexes, which a wide table has a dozen of.
const INDEXES: usize = 6;
/// A default is a glance: an expression longer than this is cut.
const DEFAULT_CHARS: usize = 40;

/// The table of the model `fqn`, `None` when it is not one.
pub(super) fn model_section(session: &mut Session, root: &Path, fqn: &str) -> Option<String> {
    let read = |path: &str| std::fs::read_to_string(path).ok();
    let (model, site) = {
        let tree = session.tree(root).ok()?;
        let model = crate::schema::model::of(tree, fqn, &read)?;
        let site = tree
            .sites(fqn)
            .into_iter()
            .map(|s| s.path)
            .find(|p| crate::core::paths::under(&root.to_string_lossy(), p));
        (model, site)
    };
    let (table, base) = match model {
        Model::Abstract => {
            return Some(format!(
                "_Abstract: `{fqn}` has no table of its own; the models that inherit it do._"
            ));
        }
        Model::Table { name, base } => (name, base),
    };
    let inherited = base
        .map(|base| format!(", shared with `{base}` (single-table inheritance)"))
        .unwrap_or_default();
    let dumps = crate::schema::dumps_near(root, site.as_deref());
    if dumps.is_empty() {
        return None;
    }
    for dump in &dumps {
        let absolute = root.join(dump);
        let Some(document) = session.document(&absolute) else {
            continue;
        };
        let tables = document.tables();
        if let Some(found) = tables.iter().find(|t| t.name == table) {
            let link = link(&absolute, dump, found.line);
            let kind = match found.view {
                Some(_) => "View",
                None => "Table",
            };
            return Some(format!(
                "**{kind} `{table}`**{inherited} · {link}\n\n{}",
                render(found)
            ));
        }
    }
    Some(format!(
        "_Table `{table}`{inherited} is not in {}._",
        dumps
            .iter()
            .map(|d| format!("`{d}`"))
            .collect::<Vec<_>>()
            .join(" or ")
    ))
}

/// The column a schema-declared attribute method was made for, from the dump
/// at `path` (absolute), on `line`: its facts in a line, linked.
pub(super) fn column_line(
    session: &mut Session,
    root: &Path,
    path: &str,
    line: u32,
    method: &str,
    linked: bool,
) -> Option<String> {
    let absolute = std::path::PathBuf::from(path);
    let tables = session.document(&absolute)?.tables();
    let (table, column) = tables.iter().find_map(|table| {
        // `t.timestamps` is two columns on one line: the method names which.
        table
            .columns
            .iter()
            .filter(|c| c.pos.line == line && method.starts_with(&c.name))
            .max_by_key(|c| c.name.len())
            .map(|c| (table, c))
    })?;
    // A view's column computed by an expression has no type to show.
    let mut facts: Vec<String> = (!column.sql_type.is_empty())
        .then(|| format!("`{}`", column.sql_type))
        .into_iter()
        .collect();
    if !column.null {
        facts.push("not null".to_string());
    }
    if let Some(default) = &column.default {
        facts.push(format!("default {}", code(&cap(default))));
    }
    if indexed(table, &column.name) {
        facts.push("indexed".to_string());
    }
    let mut text = format!(
        "Column `{}.{}`: {}",
        table.name,
        column.name,
        facts.join(", ")
    );
    if linked {
        let shown = path
            .strip_prefix(&*root.to_string_lossy())
            .map_or(path, |p| p.trim_start_matches('/'));
        text.push_str(&format!(" · {}", link(&absolute, shown, line)));
    }
    Some(text)
}

fn link(absolute: &Path, shown: &str, line: u32) -> String {
    format!("[`{shown}:{line}`]({}#L{line})", path_to_uri(absolute))
}

/// The table as a markdown table: the primary key first, then each column as
/// declared, then the indexes on a line.
fn render(table: &Table) -> String {
    let mut rows = vec![
        "| column | type | null | default |".to_string(),
        "| --- | --- | --- | --- |".to_string(),
    ];
    let key = table.primary_key.names();
    // A `schema.rb` key is implicit: the dump names its type only when it is
    // not Rails' default, which it does not write down either.
    if let PrimaryKey::Columns(names, written) = &table.primary_key {
        for name in names.iter().filter(|n| table.column(n).is_none()) {
            let kind = written
                .as_deref()
                .map_or("primary key".to_string(), |t| format!("{t}, primary key"));
            rows.push(row(&[name.clone(), kind, "no".into(), String::new()]));
        }
    }
    let mut ordered: Vec<&Column> = table
        .columns
        .iter()
        .filter(|c| key.contains(&c.name))
        .collect();
    ordered.extend(table.columns.iter().filter(|c| !key.contains(&c.name)));
    let shown = ROWS.saturating_sub(rows.len() - 2);
    for column in ordered.iter().take(shown) {
        let kind = match key.contains(&column.name) {
            true => format!("{}, primary key", column.sql_type),
            false => column.sql_type.clone(),
        };
        rows.push(row(&[
            column.name.clone(),
            kind,
            if column.null { "yes" } else { "no" }.into(),
            column
                .default
                .as_deref()
                .map(|d| code(&cap(d)))
                .unwrap_or_default(),
        ]));
    }
    let mut out = rows.join("\n");
    if let Some(view) = table.view.filter(|v| v.unread > 0) {
        out.push_str(&format!(
            "\n\n_A view: {} of its select list's columns {} no name written down, and {} not read._",
            view.unread,
            if view.unread == 1 { "has" } else { "have" },
            if view.unread == 1 { "is" } else { "are" },
        ));
    }
    if ordered.len() > shown {
        out.push_str(&format!(
            "\n\n_and {} more {}_",
            ordered.len() - shown,
            if ordered.len() - shown == 1 {
                "column"
            } else {
                "columns"
            }
        ));
    }
    if !table.indexes.is_empty() {
        let mut listed: Vec<String> = table.indexes.iter().take(INDEXES).map(index).collect();
        if table.indexes.len() > INDEXES {
            listed.push(format!("{} more", table.indexes.len() - INDEXES));
        }
        out.push_str(&format!("\n\nIndexes: {}", listed.join(" · ")));
    }
    out
}

fn index(index: &Index) -> String {
    let columns = code(&cap(&index.columns.join(", ")));
    match index.unique {
        true => format!("unique {columns}"),
        false => columns,
    }
}

/// Is the column the first of some index, so a lookup by it is cheap?
fn indexed(table: &Table, column: &str) -> bool {
    table
        .primary_key
        .names()
        .first()
        .is_some_and(|k| k == column)
        || table
            .indexes
            .iter()
            .any(|i| i.columns.first().is_some_and(|c| c == column))
}

fn cap(text: &str) -> String {
    match text.char_indices().nth(DEFAULT_CHARS) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_string(),
    }
}

/// A value as code, unless a backtick in it would break the span.
fn code(text: &str) -> String {
    match text.contains('`') {
        true => text.to_string(),
        false => format!("`{text}`"),
    }
}

/// A markdown table row. A pipe in a cell would end it.
fn row(cells: &[String]) -> String {
    let cells: Vec<String> = cells.iter().map(|c| c.replace('|', "\\|")).collect();
    format!("| {} |", cells.join(" | "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wide_table_says_how_much_it_left_out() {
        let tables = crate::schema::sql::tables(
            format!(
                "CREATE TABLE public.wide (\n    id bigint NOT NULL,\n{}\n);\n\
                 ALTER TABLE ONLY public.wide ADD CONSTRAINT wide_pkey PRIMARY KEY (id);\n\
                 CREATE UNIQUE INDEX a ON public.wide USING btree (c1, c2);\n",
                (1..=30)
                    .map(|n| format!("    c{n} text DEFAULT 'x|y'::text"))
                    .collect::<Vec<_>>()
                    .join(",\n")
            )
            .as_bytes(),
        );
        let text = render(&tables[0]);
        assert!(
            text.contains("| id | bigint, primary key | no |  |"),
            "the key first: {text}"
        );
        assert!(text.contains("_and 11 more columns_"), "{text}");
        assert!(
            text.contains(r"`'x\|y'::text`"),
            "a pipe kept in its cell: {text}"
        );
        assert!(text.contains("Indexes: unique `c1, c2`"), "{text}");
    }

    #[test]
    fn an_implicit_key_is_listed_with_what_the_dump_says_of_it() {
        let tables = crate::schema::ruby::tables(
            b"create_table \"parts\", id: :uuid do |t|\n  t.string \"name\"\nend\n\
              create_table \"kinds\" do |t|\n  t.string \"name\"\nend\n",
        );
        assert!(render(&tables[0]).contains("| id | uuid, primary key | no |  |"));
        assert!(render(&tables[1]).contains("| id | primary key | no |  |"));
    }
}
