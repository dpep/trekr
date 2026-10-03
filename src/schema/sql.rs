//! `db/structure.sql`: the schema of an app whose database does more than the
//! Ruby DSL can say, as `pg_dump` or `mysqldump` writes it.
//!
//! Not a SQL parser. A dump is a list of statements, and three of them matter:
//! `CREATE TABLE` with one column or constraint per element, `CREATE [UNIQUE]
//! INDEX … ON table (…)`, and `ALTER TABLE … ADD … PRIMARY KEY (…)`, which is
//! where Postgres states a primary key. Everything else — functions, views,
//! sequences, triggers — is stepped over whole, which means the lexer must
//! know where a statement ends: quoted strings, quoted identifiers, comments
//! and Postgres' dollar-quoted function bodies all hide semicolons.

use super::{Column, Index, PrimaryKey, Table};
use crate::extract::LineIndex;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    /// A bare word: keyword, identifier or number.
    Word,
    /// `"name"` or `` `name` ``.
    Quoted,
    /// `'text'` or `$tag$ … $tag$`.
    Str,
    Punct,
}

#[derive(Clone, Copy, Debug)]
struct Token {
    kind: Kind,
    start: usize,
    end: usize,
}

/// Every table the dump creates, with its primary key and indexes.
pub(crate) fn tables(src: &[u8]) -> Vec<Table> {
    let text = String::from_utf8_lossy(src);
    let text = text.as_ref();
    let lines = LineIndex::new(src);
    let tokens = lex(text);
    let mut tables: Vec<Table> = Vec::new();
    let mut keys: Vec<(String, Vec<String>)> = Vec::new();
    let mut indexes: Vec<(String, Index)> = Vec::new();
    for statement in tokens.split(|t| t.kind == Kind::Punct && &text[t.start..t.end] == ";") {
        let s = Statement {
            text,
            tokens: statement,
        };
        if let Some(table) = s.create_table(&lines) {
            tables.push(table);
        } else if let Some((table, index)) = s.create_index() {
            indexes.push((table, index));
        } else if let Some(found) = s.alter_table() {
            match found {
                Altered::Key(table, columns) => keys.push((table, columns)),
                Altered::Unique(table, index) => indexes.push((table, index)),
            }
        }
    }
    let at: HashMap<String, usize> = tables
        .iter()
        .enumerate()
        .map(|(i, t)| (t.name.clone(), i))
        .collect();
    for (table, columns) in keys {
        if let Some(&i) = at.get(&table) {
            tables[i].primary_key = PrimaryKey::Columns(columns, None);
        }
    }
    for (table, index) in indexes {
        if let Some(&i) = at.get(&table) {
            tables[i].indexes.push(index);
        }
    }
    tables
}

fn lex(text: &str) -> Vec<Token> {
    let bytes = text.as_bytes();
    // mysqldump escapes with a backslash; pg_dump doubles the quote and
    // writes `E'…'` when it means escapes. Backticks say which dump this is.
    let backslash = bytes.contains(&b'`');
    let mut tokens = Vec::new();
    let mut i = 0;
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80;
    while i < bytes.len() {
        let b = bytes[i];
        let start = i;
        let kind = match b {
            _ if b.is_ascii_whitespace() => {
                i += 1;
                continue;
            }
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                i = memchr(bytes, i, b'\n');
                continue;
            }
            b'#' if backslash => {
                i = memchr(bytes, i, b'\n');
                continue;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i = find(bytes, i + 2, b"*/").map_or(bytes.len(), |end| end + 2);
                continue;
            }
            b'\'' => {
                i = close_quote(bytes, i + 1, b'\'', backslash);
                Kind::Str
            }
            b'"' | b'`' => {
                i = close_quote(bytes, i + 1, b, false);
                Kind::Quoted
            }
            b'$' => match dollar_tag(bytes, i) {
                Some(tag) => {
                    let body = i + tag;
                    i = find(bytes, body, &bytes[start..body]).map_or(bytes.len(), |end| end + tag);
                    Kind::Str
                }
                None => {
                    i += 1;
                    Kind::Punct
                }
            },
            b':' if bytes.get(i + 1) == Some(&b':') => {
                i += 2;
                Kind::Punct
            }
            _ if word(b) => {
                while i < bytes.len() && word(bytes[i]) {
                    i += 1;
                }
                Kind::Word
            }
            _ => {
                i += 1;
                Kind::Punct
            }
        };
        tokens.push(Token {
            kind,
            start,
            end: i.min(bytes.len()),
        });
    }
    tokens
}

fn memchr(bytes: &[u8], from: usize, needle: u8) -> usize {
    bytes[from..]
        .iter()
        .position(|b| *b == needle)
        .map_or(bytes.len(), |at| from + at)
}

fn find(bytes: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if from > bytes.len() {
        return None;
    }
    bytes[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|at| from + at)
}

/// Past the quote closing one opened just before `from`; a doubled quote is
/// an escaped one.
fn close_quote(bytes: &[u8], mut i: usize, quote: u8, backslash: bool) -> usize {
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if backslash => i += 2,
            b if b == quote => {
                if bytes.get(i + 1) == Some(&quote) {
                    i += 2;
                } else {
                    return i + 1;
                }
            }
            _ => i += 1,
        }
    }
    bytes.len()
}

/// `$$` or `$body$` opening a dollar-quoted string: its length.
fn dollar_tag(bytes: &[u8], at: usize) -> Option<usize> {
    let mut i = at + 1;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
        if i == at + 1 && bytes[i].is_ascii_digit() {
            return None; // `$1`, a parameter
        }
        i += 1;
    }
    (bytes.get(i) == Some(&b'$')).then_some(i + 1 - at)
}

struct Statement<'t> {
    text: &'t str,
    tokens: &'t [Token],
}

enum Altered {
    Key(String, Vec<String>),
    Unique(String, Index),
}

impl<'t> Statement<'t> {
    fn raw(&self, t: &Token) -> &'t str {
        &self.text[t.start..t.end]
    }

    /// Is token `i` this keyword?
    fn is(&self, i: usize, keyword: &str) -> bool {
        self.tokens
            .get(i)
            .is_some_and(|t| t.kind == Kind::Word && self.raw(t).eq_ignore_ascii_case(keyword))
    }

    fn punct(&self, i: usize, c: &str) -> bool {
        self.tokens
            .get(i)
            .is_some_and(|t| t.kind == Kind::Punct && self.raw(t) == c)
    }

    /// A name as an identifier: unquoted, its case kept.
    fn name(&self, i: usize) -> Option<String> {
        let t = self.tokens.get(i)?;
        match t.kind {
            Kind::Word => Some(self.raw(t).to_string()),
            Kind::Quoted => {
                let raw = self.raw(t);
                let quote = &raw[..1];
                let inner = raw.get(1..raw.len().saturating_sub(1)).unwrap_or("");
                Some(inner.replace(&format!("{quote}{quote}"), quote))
            }
            _ => None,
        }
    }

    /// `public.users`, `"users"`, `` `users` ``: the table's own name, and the
    /// index after it.
    fn qualified(&self, mut i: usize) -> Option<(String, usize)> {
        let mut name = self.name(i)?;
        while self.punct(i + 1, ".") {
            i += 2;
            name = self.name(i)?;
        }
        Some((name, i + 1))
    }

    /// Past the `)` matching the `(` at `open`.
    fn close(&self, open: usize) -> Option<usize> {
        let mut depth = 0;
        for (i, _) in self.tokens.iter().enumerate().skip(open) {
            if self.punct(i, "(") {
                depth += 1;
            } else if self.punct(i, ")") {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
        }
        None
    }

    /// The comma-separated elements between `(` at `open` and `)` at `close`.
    fn elements(&self, open: usize, close: usize) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut depth = 0;
        let mut start = open + 1;
        for i in open + 1..close {
            if self.punct(i, "(") || self.punct(i, "[") {
                depth += 1;
            } else if self.punct(i, ")") || self.punct(i, "]") {
                depth -= 1;
            } else if depth == 0 && self.punct(i, ",") {
                out.push((start, i));
                start = i + 1;
            }
        }
        if start < close {
            out.push((start, close));
        }
        out
    }

    /// The source from token `from` up to, not including, token `to`, its
    /// whitespace collapsed.
    fn source(&self, from: usize, to: usize) -> String {
        if from >= to {
            return String::new();
        }
        let text = &self.text[self.tokens[from].start..self.tokens[to - 1].end];
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// Column names in `( … )`: an expression is kept as written.
    fn column_list(&self, open: usize) -> Option<Vec<String>> {
        let close = self.close(open)?;
        Some(
            self.elements(open, close)
                .into_iter()
                .map(|(from, to)| match to - from {
                    1 => self.name(from).unwrap_or_else(|| self.source(from, to)),
                    // `name varchar_pattern_ops`, `created_at DESC`: the
                    // column is what is indexed.
                    _ if matches!(self.tokens[from].kind, Kind::Word | Kind::Quoted)
                        && !self.punct(from + 1, "(") =>
                    {
                        self.name(from).unwrap_or_default()
                    }
                    _ => self.source(from, to),
                })
                .collect(),
        )
    }

    fn create_table(&self, lines: &LineIndex) -> Option<Table> {
        if !self.is(0, "CREATE") {
            return None;
        }
        let mut i = 1;
        while ["GLOBAL", "LOCAL", "TEMPORARY", "TEMP", "UNLOGGED"]
            .iter()
            .any(|w| self.is(i, w))
        {
            i += 1;
        }
        if !self.is(i, "TABLE") {
            return None;
        }
        i += 1;
        if self.is(i, "IF") {
            i += 3; // IF NOT EXISTS
        }
        let (name, open) = self.qualified(i)?;
        // `PARTITION OF parent`, `AS SELECT …`: no columns of its own here.
        if !self.punct(open, "(") {
            return None;
        }
        let close = self.close(open)?;
        let mut table = Table {
            name,
            line: lines.pos(self.tokens[0].start).line,
            ..Table::default()
        };
        let mut key = Vec::new();
        for (from, to) in self.elements(open, close) {
            if self.constraint(from, &mut key, &mut table.indexes) {
                continue;
            }
            if let Some(column) = self.column(from, to, lines, &mut key) {
                table.columns.push(column);
            }
        }
        if !key.is_empty() {
            table.primary_key = PrimaryKey::Columns(key, None);
        }
        Some(table)
    }

    /// A table constraint, or mysqldump's inline `KEY`: recorded, and `true`.
    fn constraint(&self, mut i: usize, key: &mut Vec<String>, indexes: &mut Vec<Index>) -> bool {
        if self.tokens[i].kind == Kind::Quoted {
            return false; // a quoted name is always a column's
        }
        // `key`, `index` are not reserved: as a column they are followed by a
        // type, as a constraint by `(` or the index's quoted name.
        let opens = |at: usize| {
            self.punct(at, "(")
                || self.tokens.get(at).is_some_and(|t| t.kind == Kind::Quoted)
                    && self.punct(at + 1, "(")
        };
        let starts_constraint = [
            "CONSTRAINT",
            "PRIMARY",
            "UNIQUE",
            "FOREIGN",
            "CHECK",
            "EXCLUDE",
            "LIKE",
        ]
        .iter()
        .any(|w| self.is(i, w))
            || ["KEY", "INDEX", "FULLTEXT", "SPATIAL"]
                .iter()
                .any(|w| self.is(i, w))
                && (opens(i + 1) || self.is(i + 1, "KEY") || self.is(i + 1, "INDEX"));
        if !starts_constraint {
            return false;
        }
        if self.is(i, "CONSTRAINT") {
            i += 2;
        }
        let unique = self.is(i, "UNIQUE");
        if self.is(i, "PRIMARY") && self.is(i + 1, "KEY") {
            if let Some(columns) = self.paren_after(i + 2) {
                *key = columns;
            }
        } else if (unique || ["KEY", "INDEX"].iter().any(|w| self.is(i, w)))
            && let Some(columns) = self.paren_after(i + 1)
        {
            indexes.push(Index { columns, unique });
        }
        true
    }

    /// The column list in the first `( … )` at or after `i`.
    fn paren_after(&self, i: usize) -> Option<Vec<String>> {
        let open = (i..self.tokens.len()).find(|&at| self.punct(at, "("))?;
        self.column_list(open)
    }

    fn column(
        &self,
        from: usize,
        to: usize,
        lines: &LineIndex,
        key: &mut Vec<String>,
    ) -> Option<Column> {
        let name = self.name(from)?;
        let stops = |i: usize| {
            [
                "NOT",
                "NULL",
                "DEFAULT",
                "CONSTRAINT",
                "PRIMARY",
                "UNIQUE",
                "CHECK",
                "REFERENCES",
                "COLLATE",
                "GENERATED",
                "AUTO_INCREMENT",
                "COMMENT",
                "ON",
                "AS",
            ]
            .iter()
            .any(|w| self.is(i, w))
                || self.is(i, "CHARACTER") && self.is(i + 1, "SET")
        };
        // The type runs to the first constraint word outside parentheses.
        let mut depth = 0;
        let mut i = from + 1;
        while i < to {
            if self.punct(i, "(") {
                depth += 1;
            } else if self.punct(i, ")") {
                depth -= 1;
            } else if depth == 0 && stops(i) {
                break;
            }
            i += 1;
        }
        let sql_type = self.source(from + 1, i);
        if sql_type.is_empty() {
            return None;
        }
        let mut column = Column {
            class: class_of(&sql_type),
            sql_type,
            name,
            null: true,
            default: None,
            pos: lines.pos(self.tokens[from].start),
        };
        while i < to {
            if self.is(i, "NOT") && self.is(i + 1, "NULL") {
                column.null = false;
                i += 2;
            } else if self.is(i, "PRIMARY") && self.is(i + 1, "KEY") {
                key.push(column.name.clone());
                column.null = false;
                i += 2;
            } else if self.is(i, "DEFAULT") {
                let start = i + 1;
                let mut end = start + 1;
                let mut depth = 0;
                while end < to {
                    if self.punct(end, "(") {
                        depth += 1;
                    } else if self.punct(end, ")") {
                        depth -= 1;
                    } else if depth == 0 && stops(end) {
                        break;
                    }
                    end += 1;
                }
                let value = self.source(start, end.min(to));
                if !value.is_empty() && !value.eq_ignore_ascii_case("NULL") {
                    column.default = Some(value);
                }
                i = end;
            } else {
                i += 1;
            }
        }
        Some(column)
    }

    /// `CREATE [UNIQUE] INDEX [CONCURRENTLY] [IF NOT EXISTS] name ON [ONLY]
    /// table [USING method] (columns)`.
    fn create_index(&self) -> Option<(String, Index)> {
        if !self.is(0, "CREATE") {
            return None;
        }
        let unique = self.is(1, "UNIQUE");
        if !self.is(1 + usize::from(unique), "INDEX") {
            return None;
        }
        let on = (0..self.tokens.len()).find(|&i| self.is(i, "ON"))?;
        let at = on + 1 + usize::from(self.is(on + 1, "ONLY"));
        let (table, after) = self.qualified(at)?;
        let columns = self.paren_after(after)?;
        Some((table, Index { columns, unique }))
    }

    /// `ALTER TABLE [ONLY] table ADD [CONSTRAINT name] PRIMARY KEY (…)` — where
    /// pg_dump puts a primary key — and its `UNIQUE (…)` sibling.
    fn alter_table(&self) -> Option<Altered> {
        if !(self.is(0, "ALTER") && self.is(1, "TABLE")) {
            return None;
        }
        let mut i = 2;
        if self.is(i, "IF") {
            i += 2;
        }
        if self.is(i, "ONLY") {
            i += 1;
        }
        let (table, mut i) = self.qualified(i)?;
        if !self.is(i, "ADD") {
            return None;
        }
        i += 1;
        if self.is(i, "CONSTRAINT") {
            i += 2;
        }
        if self.is(i, "PRIMARY") && self.is(i + 1, "KEY") {
            return Some(Altered::Key(table, self.paren_after(i + 2)?));
        }
        if self.is(i, "UNIQUE") {
            let columns = self.paren_after(i + 1)?;
            return Some(Altered::Unique(
                table,
                Index {
                    columns,
                    unique: true,
                },
            ));
        }
        None
    }
}

/// The class a column of this SQL type reads as, by the Rails type it maps
/// to. An array column is an Array whatever it holds.
pub(crate) fn class_of(sql_type: &str) -> Option<&'static str> {
    let lower = sql_type.to_ascii_lowercase();
    if lower.ends_with("[]") || lower.contains(" array") {
        return Some("Array");
    }
    super::column_class(rails_type(&lower)?)
}

/// `character varying(60)` → `string`: the Rails type a dumped SQL type is,
/// as both adapters map them.
fn rails_type(lower: &str) -> Option<&'static str> {
    // `public.citext` is the extension's type wherever it was installed.
    let bare = lower.rsplit_once('.').map_or(lower, |(_, t)| t);
    let (base, args) = match bare.split_once('(') {
        Some((base, rest)) => (base.trim(), rest.split(')').next().unwrap_or("")),
        None => (bare, ""),
    };
    let base = base
        .trim_end_matches(" unsigned")
        .trim_end_matches(" zerofill")
        .trim();
    Some(match base {
        "character varying" | "varchar" | "character" | "char" | "nvarchar" | "nchar" => "string",
        "text" | "tinytext" | "mediumtext" | "longtext" => "text",
        "citext" => "citext",
        "tinyint" if args.trim() == "1" => return None, // MySQL's boolean
        "integer" | "int" | "int4" | "smallint" | "int2" | "tinyint" | "mediumint" | "serial" => {
            "integer"
        }
        "bigint" | "int8" | "bigserial" => "bigint",
        "numeric" | "decimal" | "money" => "decimal",
        "real" | "float" | "float4" | "float8" | "double precision" | "double" => "float",
        "date" => "date",
        "datetime" | "timestamp" | "timestamptz" => "datetime",
        "time" | "timetz" => "time",
        "json" | "jsonb" | "hstore" => "json",
        "uuid" => "uuid",
        "inet" | "cidr" => "inet",
        "bytea" | "blob" | "tinyblob" | "mediumblob" | "longblob" | "binary" | "varbinary" => {
            "binary"
        }
        _ if base.starts_with("timestamp") => "datetime",
        _ if base.starts_with("time ") => "time",
        _ if base.starts_with("character varying") => "string",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PG: &str = "\
SET statement_timeout = 0;
--
-- Name: widgets; Type: TABLE; Schema: public; Owner: -
--

CREATE FUNCTION public.touch() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
  NEW.updated_at := now(); -- a semicolon inside the body
  RETURN NEW;
END;
$$;

CREATE TABLE public.widgets (
    id bigint NOT NULL,
    name character varying(60) NOT NULL,
    \"order\" integer DEFAULT 0 NOT NULL,
    tag_ids bigint[] DEFAULT '{}'::bigint[] NOT NULL,
    note text DEFAULT 'it''s; fine'::text,
    seen_at timestamp(6) without time zone,
    key character varying,
    status public.widget_status
);

CREATE TABLE public.parts (
    widget_id bigint,
    CONSTRAINT parts_pkey PRIMARY KEY (widget_id)
);

ALTER TABLE ONLY public.widgets
    ADD CONSTRAINT widgets_pkey PRIMARY KEY (id);

CREATE UNIQUE INDEX index_widgets_on_name ON public.widgets USING btree (name);
CREATE INDEX index_widgets_on_lower_name ON ONLY public.widgets USING btree (lower((name)::text));
";

    const MYSQL: &str = "\
/*!40101 SET @OLD_CHARACTER_SET_CLIENT=@@CHARACTER_SET_CLIENT */;
CREATE TABLE `gadgets` (
  `id` bigint NOT NULL AUTO_INCREMENT,
  `title` varchar(255) CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci DEFAULT NULL,
  `active` tinyint(1) NOT NULL DEFAULT '0',
  `price` decimal(10,2) DEFAULT NULL,
  `body` mediumtext,
  `made_at` datetime(6) NOT NULL,
  `count` int unsigned DEFAULT '1',
  PRIMARY KEY (`id`),
  UNIQUE KEY `index_gadgets_on_title` (`title`),
  KEY `index_gadgets_on_made_at` (`made_at`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
";

    fn table<'a>(tables: &'a [Table], name: &str) -> &'a Table {
        tables.iter().find(|t| t.name == name).expect(name)
    }

    #[test]
    fn reads_a_pg_dump_past_the_statements_it_does_not_model() {
        let tables = tables(PG.as_bytes());
        assert_eq!(tables.len(), 2, "{tables:?}");
        let widgets = table(&tables, "widgets");
        assert_eq!(widgets.line, 15);
        let names: Vec<&str> = widgets.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "id", "name", "order", "tag_ids", "note", "seen_at", "key", "status"
            ]
        );
        let name = widgets.column("name").unwrap();
        assert_eq!(
            (name.sql_type.as_str(), name.class, name.null, name.pos.line),
            ("character varying(60)", Some("String"), false, 17)
        );
        assert_eq!(
            widgets.column("order").unwrap().default.as_deref(),
            Some("0")
        );
        let tags = widgets.column("tag_ids").unwrap();
        assert_eq!(
            (tags.class, tags.default.as_deref()),
            (Some("Array"), Some("'{}'::bigint[]"))
        );
        assert_eq!(
            widgets.column("note").unwrap().default.as_deref(),
            Some("'it''s; fine'::text")
        );
        assert_eq!(widgets.column("seen_at").unwrap().class, Some("Time"));
        assert_eq!(widgets.column("key").unwrap().class, Some("String"));
        assert_eq!(
            widgets.column("status").unwrap().class,
            None,
            "a type of the app's own"
        );
        assert_eq!(widgets.primary_key.names(), ["id"]);
        assert_eq!(
            widgets.indexes,
            [
                Index {
                    columns: vec!["name".into()],
                    unique: true
                },
                Index {
                    columns: vec!["lower((name)::text)".into()],
                    unique: false
                }
            ]
        );
        assert_eq!(table(&tables, "parts").primary_key.names(), ["widget_id"]);
    }

    #[test]
    fn reads_a_mysql_dump() {
        let tables = tables(MYSQL.as_bytes());
        let gadgets = table(&tables, "gadgets");
        let facts: Vec<(&str, Option<&str>, bool, Option<&str>)> = gadgets
            .columns
            .iter()
            .map(|c| (c.name.as_str(), c.class, c.null, c.default.as_deref()))
            .collect();
        assert_eq!(
            facts,
            [
                ("id", Some("Integer"), false, None),
                ("title", Some("String"), true, None),
                ("active", None, false, Some("'0'")),
                ("price", Some("BigDecimal"), true, None),
                ("body", Some("String"), true, None),
                ("made_at", Some("Time"), false, None),
                ("count", Some("Integer"), true, Some("'1'")),
            ]
        );
        assert_eq!(gadgets.column("title").unwrap().sql_type, "varchar(255)");
        assert_eq!(gadgets.primary_key.names(), ["id"]);
        assert_eq!(gadgets.indexes.len(), 2);
        assert!(gadgets.indexes[0].unique);
    }

    #[test]
    fn an_unterminated_dump_still_ends() {
        for cut in [
            "CREATE TABLE a (",
            "CREATE TABLE a (b text DEFAULT 'x",
            "$$",
            "/*",
        ] {
            let _ = tables(cut.as_bytes());
        }
    }
}
