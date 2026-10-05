//! The on-disk schema. Kept in sync with `docs/ARCHITECTURE.md` in the same
//! commit — that document is the contract, this file is its implementation.

/// Bump on any change to the schema **or to what the extractor emits**.
///
/// The second half is easy to miss and was: facts are cached by blob OID on the
/// premise that they are a pure function of the bytes — but when the *function*
/// changes, identical bytes must still be re-read. An extractor fix otherwise
/// ships silently dead, because every blob it would affect is already "known".
/// `store::golden` fails when the testbed's extraction output moves and this
/// does not.
///
/// There are no migrations, and that is deliberate:
/// every row below `blob` is derived from bytes this machine can read again, so
/// the database is a **cache of a pure function**, not a system of record. A
/// version mismatch drops it and reindexes — which costs seconds and removes an
/// entire class of migration bug.
///
/// The one exception is a table in [`OPTIONAL`]: nothing reads it to answer, so
/// a store without it is still this version's (DEC-300).
pub(crate) const VERSION: i64 = 62;

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
  surface      INTEGER NOT NULL,
  -- The part of it the tree snapshot holds: declarations and ancestry, not
  -- methods. A method edit moves `surface` and leaves this (DEC-194).
  namespace    INTEGER NOT NULL,
  -- The schema version that wrote the row. No reader needs it; it is here so
  -- a writer from before it fails to insert. A 0.2 LSP left running across an
  -- upgrade never rechecks the version, and a blob row is never rewritten, so
  -- its old-format facts would otherwise outlive every reindex. NOT NULL with
  -- no default is what makes the old INSERT fail.
  written_by   INTEGER NOT NULL
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
  target_line INTEGER,                    -- an alias's body, when written above it
  target_col  INTEGER,
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
  relation TEXT    NOT NULL,              -- superclass | include | prepend | extend | singleton_prepend | load_hooks | dynamic | macro
  target   TEXT    NOT NULL,              -- constant as written, or 'self'; for dynamic, maker[|side|shape]
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

-- Which names a blob calls, and how often: a posting list, not the sites.
-- Every question about a call site reparses its file — the receiver ladder
-- needs the file's assignments (DEC-012), and an edit since the index must
-- still count — so the index only has to say which files to read (DEC-193).
CREATE TABLE call_name (
  blob_id INTEGER NOT NULL REFERENCES blob(id) ON DELETE CASCADE,
  name    TEXT    NOT NULL,
  calls   INTEGER NOT NULL,               -- call sites of `name` in the blob
  symbols INTEGER NOT NULL                -- of which a symbol naming the method
);

-- A class or module body's call on itself, outside any method, with its
-- positional arguments where each is a literal name ('' otherwise), joined by
-- tabs: the classes a macro runs on, and the names it is handed (DEC-162).
CREATE TABLE body_call (
  blob_id INTEGER NOT NULL REFERENCES blob(id) ON DELETE CASCADE,
  name    TEXT    NOT NULL,
  nesting TEXT    NOT NULL,
  args    TEXT    NOT NULL,
  line    INTEGER NOT NULL
);

-- ── The path→blob map: the only place a path appears ─────────────────────

CREATE TABLE checkout (
  id          INTEGER PRIMARY KEY,
  root        TEXT    NOT NULL UNIQUE,    -- absolute worktree path
  -- When an index pass last vouched for this checkout, unix seconds: its own
  -- index (a no-op included), or — for a gem — a bundle naming it. `--gc`
  -- reads it as "last seen" (DEC-049).
  indexed_at  INTEGER NOT NULL,
  -- repo | gem | stdlib. They are kept alive by different evidence (DEC-049):
  -- a repo by its root being on disk, a gem or a Ruby's stdlib by a surviving
  -- repo's bundle naming it. Not inferable from disk — bundler's git gems
  -- carry a `.git` of their own.
  kind        TEXT    NOT NULL DEFAULT 'repo',
  -- The file map's whole surface, folded into one number at index time: the
  -- sum over files of hash(path) ^ blob.surface. A resident front checks
  -- staleness by reading this one row rather than re-aggregating the map.
  surface_key INTEGER NOT NULL,
  -- The same fold over blob.namespace: what the tree snapshot is keyed by.
  namespace_key INTEGER NOT NULL,
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
  -- The gem's name, as the bundle resolved it; NULL for the Ruby's stdlib,
  -- which the app runs on rather than bundles. A default gem the app bundles
  -- by name hides the stdlib's copy (DEC-180).
  name        TEXT,
  PRIMARY KEY (checkout_id, gem_root)
);

-- The files of a stdlib checkout that belong to a default gem, from the
-- gemspec rubygems wrote for it (DEC-180). An app bundling its own copy of the
-- gem sees these hidden, so it answers from one json, not two.
CREATE TABLE default_gem (
  checkout_id INTEGER NOT NULL REFERENCES checkout(id) ON DELETE CASCADE,
  name        TEXT    NOT NULL,
  version     TEXT    NOT NULL,
  path        TEXT    NOT NULL,           -- relative to the stdlib root
  PRIMARY KEY (checkout_id, name, path)
) WITHOUT ROWID;

-- The files of a stdlib checkout whose classes are partly compiled, with the
-- extension each answers to (DEC-181). A method absent from their Ruby may be
-- in C, so its absence hedges, as a `dynamic` marker's does.
CREATE TABLE compiled (
  checkout_id INTEGER NOT NULL REFERENCES checkout(id) ON DELETE CASCADE,
  path        TEXT    NOT NULL,           -- relative to the stdlib root
  feature     TEXT    NOT NULL,           -- as `require` names it: `monitor`
  PRIMARY KEY (checkout_id, path)
) WITHOUT ROWID;

-- A Ruby's signatures, read from the rbs gem it carries and written as the
-- Ruby stubs core and the stdlib are served from (DEC-240). Content-
-- addressed, so every app on one Ruby shares one row: the key folds the
-- stdlib, the rbs gem and the code that reads it.
CREATE TABLE rbs (
  id      INTEGER PRIMARY KEY,
  key     TEXT    NOT NULL UNIQUE,
  version TEXT    NOT NULL,               -- the rbs gem's
  dir     TEXT    NOT NULL,               -- where it was read
  core    TEXT    NOT NULL,               -- every core class and method
  stdlib  TEXT    NOT NULL,               -- the stdlib's compiled half
  sigs    TEXT    NOT NULL                -- returns lent to its Ruby half
);

-- Which signatures a stdlib checkout is served with. None when its Ruby
-- carries no rbs gem: then there are no stubs at all.
CREATE TABLE rbs_use (
  checkout_id INTEGER PRIMARY KEY REFERENCES checkout(id) ON DELETE CASCADE,
  rbs_id      INTEGER NOT NULL REFERENCES rbs(id),
  chosen      TEXT    NOT NULL            -- bundled | installed | other (DEC-242)
);

CREATE TABLE file (
  checkout_id INTEGER NOT NULL REFERENCES checkout(id) ON DELETE CASCADE,
  path        TEXT    NOT NULL,           -- relative to the checkout root
  blob_id     INTEGER NOT NULL REFERENCES blob(id),
  PRIMARY KEY (checkout_id, path)
) WITHOUT ROWID;

-- ── The store itself ──────────────────────────────────────────────────────

-- A rebuild that threw an older index away, so a checkout missing afterwards
-- is reported as an upgrade to reindex after, not as never indexed.
CREATE TABLE upgrade (
  from_version INTEGER NOT NULL,
  at           INTEGER NOT NULL           -- unix seconds
);

-- Which trekr laid this schema down (`schema_by`), so another can name it
-- (DEC-300). Optional: a store an older trekr built at this version lacks it.
CREATE TABLE meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE INDEX gem_use_gem    ON gem_use(gem_root);
CREATE INDEX def_name       ON def(name);
CREATE INDEX def_blob       ON def(blob_id);
CREATE INDEX ancestry_blob  ON ancestry(blob_id);
CREATE INDEX const_ref_name ON const_ref(name);
CREATE INDEX const_ref_blob ON const_ref(blob_id);
CREATE INDEX call_name_name ON call_name(name, blob_id);
CREATE INDEX call_name_blob ON call_name(blob_id);
CREATE INDEX body_call_name ON body_call(name);
CREATE INDEX file_blob      ON file(blob_id);
"#;

/// The fact tables' secondary indexes: what a bulk load drops and rebuilds
/// (DEC-057). Each statement is also in `SCHEMA`, and a test holds them equal.
pub(crate) const BULK_INDEXES: [(&str, &str); 8] = [
    ("def_name", "CREATE INDEX def_name       ON def(name);"),
    ("def_blob", "CREATE INDEX def_blob       ON def(blob_id);"),
    (
        "ancestry_blob",
        "CREATE INDEX ancestry_blob  ON ancestry(blob_id);",
    ),
    (
        "const_ref_name",
        "CREATE INDEX const_ref_name ON const_ref(name);",
    ),
    (
        "const_ref_blob",
        "CREATE INDEX const_ref_blob ON const_ref(blob_id);",
    ),
    (
        "call_name_name",
        "CREATE INDEX call_name_name ON call_name(name, blob_id);",
    ),
    (
        "call_name_blob",
        "CREATE INDEX call_name_blob ON call_name(blob_id);",
    ),
    (
        "body_call_name",
        "CREATE INDEX body_call_name ON body_call(name);",
    ),
];

/// Tables an older schema had and this one does not, dropped with the rest.
pub(crate) const RETIRED: [&str; 1] = ["call_site"];

/// Every table, newest first, so dropping respects nothing (foreign keys are
/// off during the drop anyway).
pub(crate) const TABLES: [&str; 15] = [
    "meta",
    "upgrade",
    "rbs_use",
    "rbs",
    "compiled",
    "default_gem",
    "gem_use",
    "file",
    "checkout",
    "body_call",
    "call_name",
    "const_ref",
    "ancestry",
    "def",
    "blob",
];

/// Tables a store at its version may lack: added without a version bump,
/// because nothing needs them to answer (DEC-300).
pub(crate) const OPTIONAL: [&str; 1] = ["meta"];

/// A schema as a rebuild lays it down: this trekr's, or in tests another's.
pub(crate) struct Layout {
    pub(crate) version: i64,
    pub(crate) sql: &'static str,
    pub(crate) tables: &'static [&'static str],
}

/// This trekr's schema.
pub(crate) const LAYOUT: Layout = Layout {
    version: VERSION,
    sql: SCHEMA,
    tables: &TABLES,
};
