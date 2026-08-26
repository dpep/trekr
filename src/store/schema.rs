//! The on-disk schema. Kept in sync with `docs/ARCHITECTURE.md` in the same
//! commit — that document is the contract, this file is its implementation.

/// Bump on any change to the schema **or to what the extractor emits**.
///
/// The second half is easy to miss and was: facts are cached by blob OID on the
/// premise that they are a pure function of the bytes — but when the *function*
/// changes, identical bytes must still be re-read. An extractor fix otherwise
/// ships silently dead, because every blob it would affect is already "known".
///
/// There are no migrations, and that is deliberate:
/// every row below `blob` is derived from bytes this machine can read again, so
/// the database is a **cache of a pure function**, not a system of record. A
/// version mismatch drops it and reindexes — which costs seconds and removes an
/// entire class of migration bug.
pub(crate) const VERSION: i64 = 19;

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
  parse_errors INTEGER NOT NULL,
  -- Digest of just the facts the tree layer reads (defs + ancestry). Two
  -- blobs sharing it assemble the same tree, which is how an edit's effect
  -- on the tree is decided without rebuilding it. See `Facts::surface`.
  surface      INTEGER NOT NULL
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
-- `owner` is the scope stack **including the receiving class or module**,
-- which is not the same as where the target name is written: Ruby evaluates a
-- superclass expression outside the body it opens. The tree layer drops the
-- first entry for that one relation.
CREATE TABLE ancestry (
  blob_id  INTEGER NOT NULL REFERENCES blob(id) ON DELETE CASCADE,
  owner    TEXT    NOT NULL,
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
  singleton INTEGER NOT NULL,             -- written inside `def self.x`
  argc      INTEGER,                      -- NULL when a splat hides the count
  block     INTEGER NOT NULL,
  line      INTEGER NOT NULL,
  col       INTEGER NOT NULL
);

-- ── The path→blob map: the only place a path appears ─────────────────────

CREATE TABLE checkout (
  id          INTEGER PRIMARY KEY,
  root        TEXT    NOT NULL UNIQUE,    -- absolute worktree path
  indexed_at  INTEGER NOT NULL,           -- unix seconds
  -- The file map's whole surface, folded into one number at index time: the
  -- sum over files of hash(path) ^ blob.surface. A resident front checks
  -- staleness by reading this one row rather than re-aggregating the map.
  surface_key INTEGER NOT NULL,
  -- The file map itself, folded the same way: the sum over files of
  -- hash(path) ^ hash(blob oid). Identical key means an identical map, so the
  -- rewrite below can be skipped outright — which is the whole cost of a
  -- no-op index at scale. Distinct from `surface_key`, which is deliberately
  -- blind to a body-only edit: that edit moves a blob and must still be
  -- written, so the two keys answer different questions.
  map_key     INTEGER NOT NULL,
  -- git's own view of the checkout when it was last indexed: one stat of
  -- `.git/index`, folded (DEC-035). A query compares this in O(1) to decide
  -- whether the checkout *might* have moved, because the full scan cannot sit
  -- on a query path at target scale.
  git_state   INTEGER NOT NULL
);

-- Which app resolves which gem. A gem is indexed as a checkout of its own, and
-- on its own it is a tree of one gem plus Ruby core — so a method it gets from
-- a sibling gem is unreachable by construction (DEC-029). This says which
-- bundles a gem belongs to, so a position inside it can be answered against an
-- app that actually has the rest of the bundle.
CREATE TABLE gem_use (
  checkout_id INTEGER NOT NULL REFERENCES checkout(id) ON DELETE CASCADE,
  gem_root    TEXT    NOT NULL,           -- the gem's own checkout root
  PRIMARY KEY (checkout_id, gem_root)
);

CREATE TABLE file (
  checkout_id INTEGER NOT NULL REFERENCES checkout(id) ON DELETE CASCADE,
  path        TEXT    NOT NULL,           -- relative to the checkout root
  blob_id     INTEGER NOT NULL REFERENCES blob(id),
  PRIMARY KEY (checkout_id, path)
) WITHOUT ROWID;

CREATE INDEX gem_use_gem    ON gem_use(gem_root);
CREATE INDEX def_name       ON def(name);
CREATE INDEX def_blob       ON def(blob_id);
CREATE INDEX ancestry_blob  ON ancestry(blob_id);
CREATE INDEX const_ref_name ON const_ref(name);
CREATE INDEX const_ref_blob ON const_ref(blob_id);
CREATE INDEX call_site_name ON call_site(name);
CREATE INDEX call_site_blob ON call_site(blob_id);
CREATE INDEX file_blob      ON file(blob_id);
"#;

/// Every table, newest first, so dropping respects nothing (foreign keys are
/// off during the drop anyway).
pub(crate) const TABLES: [&str; 8] = [
    "gem_use",
    "file",
    "checkout",
    "call_site",
    "const_ref",
    "ancestry",
    "def",
    "blob",
];
