//! The on-disk schema. Kept in sync with `docs/ARCHITECTURE.md` in the same
//! commit — that document is the contract, this file is its implementation.

/// Bump with every migration appended below.
pub(crate) const VERSION: i64 = 1;

/// The current schema, applied whole to a fresh database. Migrations below
/// bring an older one up to it; this block is never replayed through them.
pub(crate) const SCHEMA: &str = r#"
-- ── Layer 1: facts, a pure function of a blob's bytes ────────────────────
-- Nothing below `blob` may mention a path, a checkout, or a repository. That
-- restraint is the whole product: N worktrees of one repo cost one index.

CREATE TABLE blob (
  id           INTEGER PRIMARY KEY,
  oid          TEXT    NOT NULL UNIQUE,   -- git blob sha1
  lines        INTEGER NOT NULL,
  parse_errors INTEGER NOT NULL
);

-- A name this blob binds. `via` distinguishes a literal definition (NULL) from
-- a macro expansion (`attr_reader`) and from a bare visibility assertion
-- (`private`), which claims nothing about where the method is defined.
CREATE TABLE def (
  blob_id     INTEGER NOT NULL REFERENCES blob(id) ON DELETE CASCADE,
  name        TEXT    NOT NULL,
  kind        TEXT    NOT NULL,           -- class | module | method | constant
  nesting     TEXT    NOT NULL,           -- lexical scopes, innermost first, ';'
  singleton   INTEGER NOT NULL,
  visibility  TEXT    NOT NULL,
  params      TEXT    NOT NULL,           -- 'req:a;opt:b;…', Ruby's vocabulary
  via         TEXT,
  target      TEXT,                       -- alias source, or `def Foo.x`'s Foo
  sig_returns TEXT,                       -- class named by an inline Sorbet sig
  line        INTEGER NOT NULL,
  col         INTEGER NOT NULL,
  end_line    INTEGER NOT NULL
);

-- `class Foo < Bar`, include, prepend, extend: one shape, so one table. The
-- linearization order they imply is the tree layer's business, not this one's.
CREATE TABLE ancestry (
  blob_id  INTEGER NOT NULL REFERENCES blob(id) ON DELETE CASCADE,
  nesting  TEXT    NOT NULL,
  relation TEXT    NOT NULL,              -- superclass | include | prepend | extend
  target   TEXT    NOT NULL,              -- constant as written, or 'self'
  line     INTEGER NOT NULL,
  col      INTEGER NOT NULL
);

CREATE TABLE const_ref (
  blob_id INTEGER NOT NULL REFERENCES blob(id) ON DELETE CASCADE,
  name    TEXT    NOT NULL,
  nesting TEXT    NOT NULL,
  line    INTEGER NOT NULL,
  col     INTEGER NOT NULL
);

-- The receiver shape is the fact Rubydex does not carry, and the reason this
-- engine is not a wrapper around it.
CREATE TABLE call_site (
  blob_id   INTEGER NOT NULL REFERENCES blob(id) ON DELETE CASCADE,
  name      TEXT    NOT NULL,
  recv      TEXT    NOT NULL,             -- implicit | self | const | local | ivar | other
  recv_text TEXT,
  nesting   TEXT    NOT NULL,
  argc      INTEGER,                      -- NULL when a splat hides the count
  block     INTEGER NOT NULL,
  line      INTEGER NOT NULL,
  col       INTEGER NOT NULL
);

-- ── The path→blob map: the only place a path appears ─────────────────────

CREATE TABLE checkout (
  id         INTEGER PRIMARY KEY,
  root       TEXT    NOT NULL UNIQUE,     -- absolute worktree path
  indexed_at INTEGER NOT NULL             -- unix seconds
);

CREATE TABLE file (
  checkout_id INTEGER NOT NULL REFERENCES checkout(id) ON DELETE CASCADE,
  path        TEXT    NOT NULL,           -- relative to the checkout root
  blob_id     INTEGER NOT NULL REFERENCES blob(id),
  PRIMARY KEY (checkout_id, path)
) WITHOUT ROWID;

CREATE INDEX def_name       ON def(name);
CREATE INDEX def_blob       ON def(blob_id);
CREATE INDEX ancestry_blob  ON ancestry(blob_id);
CREATE INDEX const_ref_name ON const_ref(name);
CREATE INDEX const_ref_blob ON const_ref(blob_id);
CREATE INDEX call_site_name ON call_site(name);
CREATE INDEX call_site_blob ON call_site(blob_id);
CREATE INDEX file_blob      ON file(blob_id);
"#;

/// Cumulative migrations for databases already on disk. Append, never edit.
pub(crate) const MIGRATIONS: [(i64, &str); 0] = [];
