# trekr architecture

The design contract. [PLAN.md](PLAN.md) says *why*; this says *what is built*.
Change them in the same commit as the code, per [CLAUDE.md](../CLAUDE.md).

Status: **Phase 1 (blob layer) built.** Layers 2 and 3 are sketched in PLAN §4
and not started.

## The one idea

> **A blob's facts are a pure function of its bytes.**

Everything else is a consequence. Facts are keyed by git blob OID, so N
worktrees of a repo cost one index, a branch switch reparses only genuinely new
content, and a reindex with no edits parses nothing at all. The moment a fact
knows what path it came from, that property is gone — which is why the layer
boundary below is stated as a prohibition rather than a preference.

## Layers

```text
┌─ 3. resolve + rank ────────────────── not started ─┐
│    receiver ladder, confidence, --explain          │
├─ 2. tree layer ────────────────────── not started ─┤
│    per checkout: constant namespace, MRO, methods  │
├─ 1. blob layer ───────────────────────── BUILT ────┤
│    scan/     checkout → path→OID map               │
│    extract/  bytes → facts       (pure)            │
│    store/    OID → facts, SQLite WAL               │
└────────────────────────────────────────────────────┘
```

### `scan/` — the only module that knows a path exists

`git ls-files -s` yields path→OID for every tracked file in ~100 ms at 100k
files. Files the working tree has changed (`git diff-files`) and files git has
never seen (`git ls-files -o`) are hashed the way git does —
`sha1("blob <len>\0" + bytes)` — so an uncommitted edit keys identically to the
commit that will later contain it. A file that has vanished from the worktree is
simply absent from the map; there is no deletion case to handle downstream.

Submodule (`160000`) and symlink (`120000`) entries are dropped: their OIDs name
a commit and a path string, not Ruby.

**A git repository is required.** Content addressing is the product, and git is
what makes it nearly free (DEC-001).

### `extract/` — bytes in, facts out

Prism (`ruby-prism`, vendored C, no Ruby toolchain) via the `Visit` trait, with a
lexical scope stack on the visitor: push a frame, call the free `visit_*`
function to descend, pop. Semantics are lifted from Shopify's Rubydex (MIT) —
`docs/ruby-behaviors.md` is the conformance spec — but the crate is not a
dependency (PLAN §8).

The fact set:

| fact | carries |
|---|---|
| **definition** | name, kind (class/module/method/constant), lexical nesting, singleton, visibility, parameters, `via`, `target`, Sorbet `sig` return |
| **ancestry edge** | nesting, relation (superclass/include/prepend/extend), target as written |
| **constant reference** | name as written, the nesting that will resolve it |
| **call site** | name, **receiver shape**, receiver text, arity, block |

Receiver shape — `implicit | self | const | local | ivar | other` — is the fact
Rubydex does not carry and the reason this engine is not a wrapper around it.
53–66% of Ruby call sites are implicit self and need no inference at all.

Macros are expanded at extraction, so no later layer needs to know they exist:
`attr_accessor :x` becomes `x` and `x=`; `module_function` turns one `def` into a
public singleton method and a private instance one; `alias` and `alias_method`
become methods with a `target`.

### `store/` — SQLite, WAL, no cleverness

Schema in [`src/store/schema.rs`](../src/store/schema.rs); it is the authority
and this table is its summary.

```text
blob(id, oid UNIQUE, lines, parse_errors)
  def(blob_id, name, kind, nesting, singleton, visibility, params,
      via, target, sig_returns, line, col, end_line)
  ancestry(blob_id, nesting, relation, target, line, col)
  const_ref(blob_id, name, nesting, line, col)
  call_site(blob_id, name, recv, recv_text, nesting, argc, block, line, col)

checkout(id, root UNIQUE, indexed_at)
  file(checkout_id, path, blob_id)          ← the only table naming a path
```

**No table under `blob` may mention a path, a checkout, or a repository.**

Two encodings share a column apiece rather than earning a table:

- `nesting` — lexical scopes innermost first, joined by `;`. Ruby constant paths
  are `[A-Za-z0-9_:]`, so the separator cannot occur inside one. The stack is
  stored rather than derived because `module A::B` opens **one** scope, not two,
  and only the stack shows that.
- `params` — `kind:name` pairs joined by `;`, using Ruby's own
  `Method#parameters` vocabulary (`req` `opt` `rest` `post` `keyreq` `key`
  `keyrest` `block` `nokey`). Arity is derivable; a glossary of ours is not
  needed.

`argc` is NULL when a splat makes the count unknowable — an honest absence
rather than a sentinel.

## CLI

Operations are flags, not subcommands (rq's convention), so no word is reserved
and the default action stays free for the query verbs layer 3 will add. Every
command that prints honors `--json` / `--ndjson`.

| exit | meaning |
|---|---|
| 0 | something was indexed, or a query matched |
| 1 | nothing matched, nothing to do — a definitive answer |
| 2 | the request could not be served (not a repo, unreadable file) |

`$TREKR_DB` overrides the database path (default
`~/.local/share/trekr/trekr.db`); the e2e tests use it for isolation.

## Known gaps

Deliberate, and cheap to close when they earn it:

- `Class.new` / `Module.new` bodies are owners but not lexical scopes; not
  modeled. Constants inside them will be attributed to the enclosing scope.
- `private_constant` / `private_class_method` are not read.
- Instance, class, and global variables are not recorded (not in PLAN §4's
  Phase 1 fact set).
- Multi-write constant targets (`A, B = 1, 2`) define nothing.
- `refine` is not modeled.
- Orphaned blobs are never collected (DEC-003).
