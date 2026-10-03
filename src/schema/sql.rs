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

use super::{Column, Index, PrimaryKey, Table, View};
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
    // Names are qualified by their schema while the dump is read: an older
    // pg_dump writes them bare under a `SET search_path` per schema.
    let mut search: Vec<String> = Vec::new();
    let mut app_search: Vec<String> = Vec::new();
    for statement in tokens.split(|t| t.kind == Kind::Punct && &text[t.start..t.end] == ";") {
        let s = Statement {
            text,
            tokens: statement,
            schema: search.first().map(String::as_str),
        };
        if let Some(path) = s.search_path() {
            if !path.is_empty() {
                app_search = path.clone();
            }
            search = path;
            continue;
        }
        // pg_dump writes what a table inherits or a view reads before it.
        if let Some(table) = s.create_table(&lines, &tables) {
            tables.push(table);
        } else if let Some(view) = s.create_view(&lines, &tables) {
            tables.push(view);
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
    unqualify(&mut tables, &app_search);
    tables
}

fn lex(text: &str) -> Vec<Token> {
    let bytes = text.as_bytes();
    // mysqldump escapes with a backslash and comments with `#`; pg_dump
    // doubles the quote, writes `E'…'` when it means escapes, and uses `#`
    // as jsonb's path operators.
    let backslash = is_mysql(bytes);
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

/// Is this mysqldump's dialect rather than pg_dump's? Read from what each
/// writes at the top and around every table, not from any backtick: a
/// Postgres dump has them inside strings (a `CHECK`'s regex, a comment).
fn is_mysql(bytes: &[u8]) -> bool {
    let has = |needle: &[u8]| find(bytes, 0, needle).is_some();
    let postgres = [
        b"PostgreSQL database dump".as_slice(),
        b"SET standard_conforming_strings",
        b"SET search_path",
        b"pg_catalog.",
    ];
    if postgres.iter().any(|marker| has(marker)) {
        return false;
    }
    [b"/*!40".as_slice(), b"ENGINE=", b"CREATE TABLE `"]
        .iter()
        .any(|marker| has(marker))
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

/// A table is named bare when the app's search path finds it — the last
/// `SET search_path` (Rails appends one), else `public` — and by its schema
/// otherwise, as `self.table_name = "audit.posts"` names it.
fn unqualify(tables: &mut [Table], search: &[String]) {
    let public = ["public".to_string()];
    let search = if search.is_empty() {
        &public[..]
    } else {
        search
    };
    let names: std::collections::HashSet<String> = tables.iter().map(|t| t.name.clone()).collect();
    for table in tables.iter_mut() {
        let Some((schema, bare)) = table.name.split_once('.') else {
            continue;
        };
        let found = search
            .iter()
            .find(|s| names.contains(&format!("{s}.{bare}")));
        if found.is_some_and(|s| s == schema) {
            table.name = bare.to_string();
        }
    }
}

struct Statement<'t> {
    text: &'t str,
    tokens: &'t [Token],
    /// Where a bare name is created: the search path's first schema.
    schema: Option<&'t str>,
}

/// A view's column, from one item of its select list.
struct Output {
    name: String,
    /// The token it is named at.
    at: usize,
    /// The column it is, when the item is a bare reference: its qualifier
    /// and name.
    reads: Option<(Option<String>, String)>,
}

/// The column of a table the view reads that a bare reference names, when
/// exactly one of them has it.
fn column_read<'a>(
    (qualifier, column): &(Option<String>, String),
    read: &[String],
    before: &'a [Table],
) -> Option<&'a Column> {
    let mut found = read
        .iter()
        .filter_map(|name| before.iter().find(|t| t.name == *name))
        .filter(|t| qualifier.as_deref().is_none_or(|q| bare(&t.name) == q))
        .filter_map(|t| t.column(column));
    let first = found.next()?;
    found.next().is_none().then_some(first)
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

    /// `public.users`, `"users"`, `` `users` ``: the table's name qualified
    /// by its schema — the one written, else the search path's — and the
    /// index after it.
    fn qualified(&self, mut i: usize) -> Option<(String, usize)> {
        let mut parts = vec![self.name(i)?];
        while self.punct(i + 1, ".") {
            i += 2;
            parts.push(self.name(i)?);
        }
        let table = parts.pop()?;
        let name = match parts.pop().as_deref().or(self.schema) {
            Some(schema) => format!("{schema}.{table}"),
            None => table,
        };
        Some((name, i + 1))
    }

    /// `SET search_path TO "$user", public`: the schemas a bare name is
    /// looked up in, those of the app's own.
    fn search_path(&self) -> Option<Vec<String>> {
        if !(self.is(0, "SET") && self.is(1, "search_path")) {
            return None;
        }
        let path = (3..self.tokens.len())
            .step_by(2)
            .filter_map(|i| match self.tokens[i].kind {
                Kind::Str => {
                    let raw = self.raw(&self.tokens[i]);
                    Some(raw.trim_matches('\'').to_string())
                }
                _ => self.name(i),
            })
            .filter(|s| !s.is_empty() && s != "$user" && s != "pg_catalog")
            .collect();
        Some(path)
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
                    // mysqldump's prefix index, `` `name`(191) ``.
                    4 if self.raw(&self.tokens[from]).starts_with('`')
                        && self.punct(from + 1, "(")
                        && self.punct(from + 3, ")") =>
                    {
                        self.name(from).unwrap_or_default()
                    }
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

    fn create_table(&self, lines: &LineIndex, before: &[Table]) -> Option<Table> {
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
        // `INHERITS (parent)`: Postgres puts the parent's columns first.
        if self.is(close + 1, "INHERITS") {
            let parents: Vec<String> = match self.close(close + 2) {
                Some(end) if self.punct(close + 2, "(") => self
                    .elements(close + 2, end)
                    .into_iter()
                    .filter_map(|(from, _)| Some(self.qualified(from)?.0))
                    .collect(),
                _ => Vec::new(),
            };
            let inherited = parents
                .iter()
                .filter_map(|p| before.iter().find(|t| t.name == *p))
                .flat_map(|parent| parent.columns.iter().cloned())
                .filter(|c| table.column(&c.name).is_none())
                .collect::<Vec<_>>();
            table.columns.splice(0..0, inherited);
        }
        Some(table)
    }

    /// `CREATE [OR REPLACE] [MATERIALIZED] VIEW name AS SELECT …`: a column
    /// per select-list item that names one, typed when it is a bare column
    /// of a table the view reads.
    fn create_view(&self, lines: &LineIndex, before: &[Table]) -> Option<Table> {
        if !self.is(0, "CREATE") {
            return None;
        }
        let mut i = 1;
        while [
            "OR",
            "REPLACE",
            "TEMP",
            "TEMPORARY",
            "RECURSIVE",
            "MATERIALIZED",
        ]
        .iter()
        .any(|w| self.is(i, w))
        {
            i += 1;
        }
        if !self.is(i, "VIEW") {
            return None;
        }
        i += 1;
        if self.is(i, "IF") {
            i += 3;
        }
        let (name, after) = self.qualified(i)?;
        let named = match self.punct(after, "(") {
            true => self.column_list(after),
            false => None,
        };
        let select = (after..self.tokens.len()).find(|&at| self.is(at, "SELECT"))?;
        let mut view = Table {
            name,
            line: lines.pos(self.tokens[0].start).line,
            ..Table::default()
        };
        let (items, read) = self.select_list(select);
        let mut unread = 0;
        for (at, (from, to)) in items.into_iter().enumerate() {
            let output = self.output(from, to);
            let written = named.as_ref().and_then(|names| names.get(at)).cloned();
            let Some(name) = written.or_else(|| output.as_ref().map(|o| o.name.clone())) else {
                unread += 1;
                continue;
            };
            let typed = output
                .as_ref()
                .and_then(|o| o.reads.as_ref())
                .and_then(|reads| column_read(reads, &read, before));
            view.columns.push(Column {
                name,
                sql_type: typed.map_or(String::new(), |c| c.sql_type.clone()),
                class: typed.and_then(|c| c.class),
                null: true,
                default: None,
                pos: lines.pos(self.tokens[output.map_or(from, |o| o.at)].start),
            });
        }
        view.view = Some(View { unread });
        Some(view)
    }

    /// The items of the select list starting at `select`, and the tables its
    /// `FROM` and `JOIN`s name.
    fn select_list(&self, select: usize) -> (Vec<(usize, usize)>, Vec<String>) {
        let mut i = select + 1;
        if self.is(i, "ALL") {
            i += 1;
        } else if self.is(i, "DISTINCT") {
            i += 1;
            if self.is(i, "ON") {
                i = self.close(i + 1).map_or(i + 1, |close| close + 1);
            }
        }
        let ends = [
            "FROM",
            "UNION",
            "INTERSECT",
            "EXCEPT",
            "WHERE",
            "GROUP",
            "HAVING",
            "ORDER",
            "LIMIT",
            "WINDOW",
            "WITH",
        ];
        let mut items = Vec::new();
        let mut depth = 0;
        let mut start = i;
        let mut end = self.tokens.len();
        while i < self.tokens.len() {
            if self.punct(i, "(") {
                depth += 1;
            } else if self.punct(i, ")") {
                depth -= 1;
            } else if depth == 0 && ends.iter().any(|w| self.is(i, w)) {
                end = i;
                break;
            } else if depth == 0 && self.punct(i, ",") {
                items.push((start, i));
                start = i + 1;
            }
            i += 1;
        }
        if start < end {
            items.push((start, end));
        }
        let mut read = Vec::new();
        let mut depth = 0;
        for at in end..self.tokens.len() {
            if self.punct(at, "(") {
                depth += 1;
            } else if self.punct(at, ")") {
                depth -= 1;
            } else if depth == 0
                && (self.is(at, "FROM") || self.is(at, "JOIN"))
                && let Some((table, _)) = self.qualified(at + 1)
            {
                read.push(table);
            }
        }
        (items, read)
    }

    /// The column a select-list item makes. `None` for `*`, or an expression
    /// with no name, which Postgres calls `?column?`.
    fn output(&self, from: usize, to: usize) -> Option<Output> {
        if to >= from + 2 && self.is(to - 2, "AS") {
            return Some(Output {
                name: self.name(to - 1)?,
                at: to - 1,
                reads: self.reference(from, to - 2),
            });
        }
        if let Some(reads) = self.reference(from, to) {
            return Some(Output {
                name: reads.1.clone(),
                at: to - 1,
                reads: Some(reads),
            });
        }
        // `lower(title)` is a column named `lower`.
        let call = self.tokens[from].kind == Kind::Word
            && self.punct(from + 1, "(")
            && self.close(from + 1) == Some(to - 1);
        if !call {
            return None;
        }
        Some(Output {
            name: self.name(from)?,
            at: from,
            reads: None,
        })
    }

    /// `title`, `posts.title`, `public.posts.title`: the column, and the
    /// table it is qualified by.
    fn reference(&self, from: usize, to: usize) -> Option<(Option<String>, String)> {
        let parts = to.checked_sub(from)?;
        if parts % 2 == 0 {
            return None;
        }
        let mut names = Vec::new();
        for at in (from..to).step_by(2) {
            if !matches!(self.tokens[at].kind, Kind::Word | Kind::Quoted) {
                return None;
            }
            names.push(self.name(at)?);
            if at + 1 < to && !self.punct(at + 1, ".") {
                return None;
            }
        }
        let column = names.pop()?;
        Some((names.pop(), column))
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
        // `exclude` is not reserved either, and pg_dump leaves it unquoted.
        let starts_constraint = [
            "CONSTRAINT",
            "PRIMARY",
            "UNIQUE",
            "FOREIGN",
            "CHECK",
            "LIKE",
        ]
        .iter()
        .any(|w| self.is(i, w))
            || self.is(i, "EXCLUDE") && (self.punct(i + 1, "(") || self.is(i + 1, "USING"))
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
        if self.is(i, "FULLTEXT") || self.is(i, "SPATIAL") {
            i += 1;
        }
        if self.is(i, "PRIMARY") && self.is(i + 1, "KEY") {
            if let Some(columns) = self.paren_after(i + 2) {
                *key = columns;
            }
        } else if (unique || ["KEY", "INDEX"].iter().any(|w| self.is(i, w)) || opens(i))
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

/// A table's name without its schema: `public.posts` → `posts`.
fn bare(name: &str) -> &str {
    name.rsplit_once('.').map_or(name, |(_, table)| table)
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
        .trim_end_matches(" zerofill")
        .trim_end_matches(" unsigned")
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
    fn a_backtick_in_a_pg_dump_does_not_make_it_mysqls() {
        let dump = "\
-- PostgreSQL database dump
SET standard_conforming_strings = on;
CREATE TABLE public.docs (
    data jsonb NOT NULL,
    title text GENERATED ALWAYS AS ((data #>> '{title}'::text[])) STORED,
    sep text DEFAULT '\\'::text,
    body text
);
CREATE INDEX index_docs_on_path ON public.docs USING btree (((data #> '{a}'::text[])));
CREATE TABLE public.tags (
    name text CONSTRAINT quotes CHECK ((name !~ '^[\"''`]'::text))
);
";
        let tables = tables(dump.as_bytes());
        let names: Vec<&str> = table(&tables, "docs")
            .columns
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(names, ["data", "title", "sep", "body"]);
        assert_eq!(table(&tables, "docs").indexes.len(), 1);
        assert_eq!(table(&tables, "tags").columns.len(), 1);
    }

    #[test]
    fn a_column_named_like_a_constraint_word_is_a_column() {
        let dump = "\
CREATE TABLE public.rules (
    exclude boolean,
    born date,
    CONSTRAINT no_overlap EXCLUDE USING gist (born WITH =)
);
";
        let rules = tables(dump.as_bytes());
        let columns: Vec<(&str, Option<&str>)> = rules[0]
            .columns
            .iter()
            .map(|c| (c.name.as_str(), c.class))
            .collect();
        assert_eq!(columns, [("exclude", None), ("born", Some("Date"))]);
    }

    #[test]
    fn an_inheriting_table_has_its_parents_columns_first() {
        let dump = "\
CREATE TABLE public.base_things (
    id bigint NOT NULL,
    name text
);
CREATE TABLE public.sub_things (
    extra integer
)
INHERITS (public.base_things);
";
        let tables = tables(dump.as_bytes());
        let sub = table(&tables, "sub_things");
        let names: Vec<&str> = sub.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["id", "name", "extra"]);
        assert_eq!(sub.column("name").unwrap().pos.line, 3, "at the parent's");
    }

    #[test]
    fn a_views_columns_are_its_select_lists_names() {
        let dump = "\
CREATE TABLE public.posts (
    id bigint NOT NULL,
    title character varying(60) NOT NULL,
    score integer
);
CREATE VIEW public.recent_posts AS
 SELECT posts.id,
    title,
    (score * 2) AS doubled,
    lower(title)
   FROM public.posts
  WHERE (score > 0);
CREATE MATERIALIZED VIEW public.post_stats AS
 SELECT count(*) AS n,
    max(score) AS top
   FROM public.posts
  WITH NO DATA;
CREATE VIEW public.everything AS
 SELECT * FROM public.posts;
";
        let tables = tables(dump.as_bytes());
        let recent = table(&tables, "recent_posts");
        let columns: Vec<(&str, Option<&str>)> = recent
            .columns
            .iter()
            .map(|c| (c.name.as_str(), c.class))
            .collect();
        assert_eq!(
            columns,
            [
                ("id", Some("Integer")),
                ("title", Some("String")),
                ("doubled", None),
                ("lower", None),
            ],
            "a bare column has its table's type"
        );
        assert!(recent.view.is_some_and(|v| v.unread == 0));
        let stats = table(&tables, "post_stats");
        assert_eq!(stats.columns.len(), 2);
        assert!(stats.view.is_some());
        let everything = table(&tables, "everything");
        assert!(everything.columns.is_empty());
        assert_eq!(
            everything.view.map(|v| v.unread),
            Some(1),
            "`*` is not read"
        );
    }

    #[test]
    fn reads_mysqls_zerofill_fulltext_and_prefix_indexes() {
        let dump = "\
CREATE TABLE `notes` (
  `id` int unsigned zerofill NOT NULL,
  `name` varchar(255) NOT NULL,
  `body` text,
  UNIQUE KEY `index_notes_on_name` (`name`(191)),
  FULLTEXT KEY `index_notes_on_body` (`body`)
) ENGINE=InnoDB;
";
        let notes = &tables(dump.as_bytes())[0];
        assert_eq!(notes.column("id").unwrap().class, Some("Integer"));
        assert_eq!(
            notes.indexes,
            [
                Index {
                    columns: vec!["name".into()],
                    unique: true
                },
                Index {
                    columns: vec!["body".into()],
                    unique: false
                }
            ]
        );
    }

    #[test]
    fn a_table_outside_the_search_path_keeps_its_schema() {
        // An older pg_dump writes names bare, under a search path per schema.
        let dump = "\
SET search_path = audit, pg_catalog;
CREATE TABLE posts (action text);
CREATE TABLE logs (line text);
SET search_path = public, pg_catalog;
CREATE TABLE posts (title text);
CREATE INDEX index_posts_on_title ON posts USING btree (title);
SET search_path TO \"$user\", public;
";
        let tables = tables(dump.as_bytes());
        let names: Vec<&str> = tables.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["audit.posts", "audit.logs", "posts"]);
        assert_eq!(table(&tables, "posts").indexes.len(), 1);
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
