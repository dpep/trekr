//! `db/schema.rb`: `create_table "posts" do |t| … end`, and the `add_index`
//! an older dump writes after it.

use super::{Column, Index, PrimaryKey, Table};
use crate::extract::LineIndex;
use ruby_prism::{CallNode, Node, Visit};

/// Every table the file creates, with the indexes it adds to them.
pub(crate) fn tables(src: &[u8]) -> Vec<Table> {
    struct Walk<'s> {
        src: &'s [u8],
        lines: LineIndex,
        tables: Vec<Table>,
        added: Vec<(String, Index)>,
    }
    impl<'pr> Visit<'pr> for Walk<'_> {
        fn visit_call_node(&mut self, call: &CallNode<'pr>) {
            if let Some(table) = table_of(call, self.src, &self.lines) {
                self.tables.push(table);
                return;
            }
            if name(call).as_deref() == Some("add_index") {
                let args = args(call);
                if let Some((table, index)) = args
                    .split_first()
                    .and_then(|(table, rest)| Some((literal(table)?, index_of(rest)?)))
                {
                    self.added.push((table, index));
                }
            }
            ruby_prism::visit_call_node(self, call);
        }
    }
    let parsed = ruby_prism::parse(src);
    let mut walk = Walk {
        src,
        lines: LineIndex::new(src),
        tables: Vec::new(),
        added: Vec::new(),
    };
    walk.visit(&parsed.node());
    let Walk {
        mut tables, added, ..
    } = walk;
    for (name, index) in added {
        if let Some(table) = tables.iter_mut().find(|t| t.name == name) {
            table.indexes.push(index);
        }
    }
    tables
}

/// One `create_table` call, when it is one with a block of columns.
pub(crate) fn table_of(call: &CallNode<'_>, src: &[u8], lines: &LineIndex) -> Option<Table> {
    if name(call).as_deref() != Some("create_table") {
        return None;
    }
    let options = args(call);
    let table = options.first().and_then(literal)?;
    let block = call.block()?.as_block_node()?;
    // The block parameter is what column declarations are called on.
    let builder = block
        .parameters()?
        .as_block_parameters_node()?
        .parameters()?
        .requireds()
        .iter()
        .next()?
        .as_required_parameter_node()
        .map(|p| p.name().as_slice().to_vec())?;
    let primary_key = match (keyword(&options, "id"), keyword(&options, "primary_key")) {
        (Some(id), _) if id.as_false_node().is_some() => PrimaryKey::None,
        (id, key) => {
            let names = key
                .map(|key| list(&key))
                .filter(|names| !names.is_empty())
                .unwrap_or_else(|| vec!["id".to_string()]);
            PrimaryKey::Columns(names, id.as_ref().and_then(literal))
        }
    };
    let mut out = Table {
        name: table,
        line: lines.pos(call.location().start_offset()).line,
        primary_key,
        ..Table::default()
    };
    let statements = block.body().and_then(|b| b.as_statements_node());
    for statement in statements.iter().flat_map(|s| s.body().iter()) {
        let Some(inner) = statement.as_call_node() else {
            continue;
        };
        // Only calls on the block parameter declare columns.
        let on_builder = inner
            .receiver()
            .and_then(|r| r.as_local_variable_read_node())
            .is_some_and(|l| l.name().as_slice() == builder.as_slice());
        let Some(kind) = name(&inner).filter(|_| on_builder) else {
            continue;
        };
        let args = args(&inner);
        let at = |node: &Node<'_>| lines.pos(node.location().start_offset());
        let message = inner
            .message_loc()
            .map_or(inner.location().start_offset(), |l| l.start_offset());
        let null = keyword(&args, "null").is_none_or(|n| n.as_false_node().is_none());
        let default = keyword(&args, "default").map(|d| source(src, &d));
        let column = |name: String, sql_type: &str, pos| {
            let array = keyword(&args, "array").is_some_and(|a| a.as_true_node().is_some());
            Column {
                name,
                sql_type: match array {
                    true => format!("{sql_type}[]"),
                    false => sql_type.to_string(),
                },
                class: match array {
                    true => Some("Array"),
                    false => super::column_class(sql_type),
                },
                null,
                default: default.clone(),
                pos,
            }
        };
        match kind.as_str() {
            // Two datetime columns spelled as one call, not null since Rails 5.
            "timestamps" => {
                let null = keyword(&args, "null").is_some_and(|n| n.as_true_node().is_some());
                for name in ["created_at", "updated_at"] {
                    out.columns.push(Column {
                        null,
                        ..column(name.to_string(), "datetime", lines.pos(message))
                    });
                }
            }
            // `t.references :author` is the `author_id` column. The `author`
            // reader is the model's `belongs_to`, not the table's.
            "references" | "belongs_to" => {
                for arg in &args {
                    if let Some(name) = literal(arg) {
                        out.columns
                            .push(column(format!("{name}_id"), "bigint", at(arg)));
                    }
                }
            }
            "index" => {
                if let Some(index) = index_of(&args) {
                    out.indexes.push(index);
                }
            }
            "column" => {
                if let (Some(name), Some(sql_type)) = (
                    args.first().and_then(literal),
                    args.get(1).and_then(literal),
                ) {
                    out.columns.push(column(name, &sql_type, at(&args[0])));
                }
            }
            _ if super::is_column_type(&kind) => {
                for arg in &args {
                    if let Some(name) = literal(arg) {
                        out.columns.push(column(name, &kind, at(arg)));
                    }
                }
            }
            _ => continue,
        }
    }
    Some(out)
}

/// `["a", "b"], unique: true` — what `t.index` and `add_index` are handed
/// after any table name.
fn index_of(args: &[Node<'_>]) -> Option<Index> {
    let columns = list(args.first()?);
    if columns.is_empty() {
        return None;
    }
    let unique = keyword(args, "unique").is_some_and(|u| u.as_true_node().is_some());
    Some(Index { columns, unique })
}

/// A literal name, or an array of them.
fn list(node: &Node<'_>) -> Vec<String> {
    match node.as_array_node() {
        Some(array) => array
            .elements()
            .iter()
            .filter_map(|e| literal(&e))
            .collect(),
        None => literal(node).into_iter().collect(),
    }
}

fn name(call: &CallNode<'_>) -> Option<String> {
    String::from_utf8(call.name().as_slice().to_vec()).ok()
}

fn args<'pr>(call: &CallNode<'pr>) -> Vec<Node<'pr>> {
    call.arguments()
        .map(|a| a.arguments().iter().collect())
        .unwrap_or_default()
}

fn literal(node: &Node<'_>) -> Option<String> {
    if let Some(symbol) = node.as_symbol_node() {
        return String::from_utf8(symbol.unescaped().to_vec()).ok();
    }
    String::from_utf8(node.as_string_node()?.unescaped().to_vec()).ok()
}

fn keyword<'pr>(args: &[Node<'pr>], key: &str) -> Option<Node<'pr>> {
    args.iter()
        .filter_map(|arg| arg.as_keyword_hash_node())
        .flat_map(|hash| hash.elements().iter())
        .filter_map(|element| element.as_assoc_node())
        .find(|assoc| {
            assoc
                .key()
                .as_symbol_node()
                .is_some_and(|s| s.unescaped() == key.as_bytes())
        })
        .map(|assoc| assoc.value())
}

/// A value as written, whitespace collapsed.
fn source(src: &[u8], node: &Node<'_>) -> String {
    let loc = node.location();
    String::from_utf8_lossy(&src[loc.start_offset()..loc.end_offset()])
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = r#"ActiveRecord::Schema[7.1].define(version: 1) do
  create_table "widgets", force: :cascade do |t|
    t.string "name", limit: 60, null: false
    t.integer "count", default: 0
    t.bigint "tag_ids", default: [], array: true
    t.boolean "active", default: true, null: false
    t.datetime "seen_at", default: -> { "CURRENT_TIMESTAMP" }
    t.index ["name"], name: "index_widgets_on_name", unique: true
  end

  create_table "parts", id: :uuid do |t|
    t.references :widget
    t.timestamps
  end

  create_table "joins", id: false do |t|
    t.column "kind", :string
  end

  add_index "parts", ["widget_id"], name: "index_parts_on_widget_id"
end
"#;

    #[test]
    fn reads_columns_with_their_facts() {
        let tables = tables(SCHEMA.as_bytes());
        let names: Vec<&str> = tables.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["widgets", "parts", "joins"]);
        let widgets = &tables[0];
        assert_eq!(widgets.line, 2);
        let facts: Vec<String> = widgets
            .columns
            .iter()
            .map(|c| {
                format!(
                    "{} {} {:?} null={} default={:?} line {}",
                    c.name, c.sql_type, c.class, c.null, c.default, c.pos.line
                )
            })
            .collect();
        assert_eq!(
            facts,
            [
                r#"name string Some("String") null=false default=None line 3"#,
                r#"count integer Some("Integer") null=true default=Some("0") line 4"#,
                r#"tag_ids bigint[] Some("Array") null=true default=Some("[]") line 5"#,
                r#"active boolean None null=false default=Some("true") line 6"#,
                r#"seen_at datetime Some("Time") null=true default=Some("-> { \"CURRENT_TIMESTAMP\" }") line 7"#,
            ]
        );
        assert_eq!(widgets.primary_key.names(), ["id"]);
        assert_eq!(
            widgets.indexes,
            [Index {
                columns: vec!["name".into()],
                unique: true
            }]
        );
    }

    #[test]
    fn reads_the_primary_key_and_the_indexes_added_after() {
        let tables = tables(SCHEMA.as_bytes());
        let parts = &tables[1];
        assert_eq!(
            parts.primary_key,
            PrimaryKey::Columns(vec!["id".into()], Some("uuid".into()))
        );
        let names: Vec<&str> = parts.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["widget_id", "created_at", "updated_at"]);
        assert!(!parts.columns[1].null, "timestamps are not null");
        assert_eq!(parts.indexes.len(), 1, "add_index joins its table");
        let joins = &tables[2];
        assert_eq!(joins.primary_key, PrimaryKey::None);
        assert_eq!(joins.columns[0].class, Some("String"));
    }
}
