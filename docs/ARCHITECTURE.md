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

| command | answers |
|---|---|
| `--index [PATH]` | scan a checkout and store what is new |
| `--status` | what is indexed, per checkout, plus the shared totals |
| `--symbols FILE` | one file's definitions, in source order |
| `--refs NAME` | every mention of a name in this checkout |
| `--drop [PATH]` | forget a checkout's file map |

`--refs` is **name-level, not resolved**: two unrelated `Config` classes both
answer, and so does every `#save` on every receiver. Each row says what sort of
mention it is and, for a call, what shape the receiver had — disclosure instead
of a guess. Narrowing it is layer 3's job.

| exit | meaning |
|---|---|
| 0 | something was indexed, or a query matched |
| 1 | nothing matched, nothing to do — a definitive answer |
| 2 | the request could not be served (not a repo, unreadable file) |

`$TREKR_DB` overrides the database path (default
`~/.local/share/trekr/trekr.db`); the e2e tests use it for isolation.

## Measurements

2026-08-24, Apple M2 (8 cores), release build, warm page cache. Reproduce with
`make bench`. Cold time is a single run — a second one is by definition not
cold; the no-op and query timings are medians of five, which is the precision
they are quoted to.

| corpus | files | cold | no-op reindex | defs | const refs | call sites | DB |
|---|---:|---:|---:|---:|---:|---:|---:|
| rails | 3,307 | 1.5 s | **61 ms** | 50,353 | 91,178 | 308,453 | 47 MB |
| discourse | 11,287 | 3.2 s | **121 ms** | 59,194 | 206,215 | 1,227,403 | 117 MB |
| CRuby | 7,931 | 2.4 s | **98 ms** | 59,224 | 174,711 | 676,971 | 73 MB |

- **A no-op reindex parses nothing** — the property the whole design exists for.
  About 40 ms of rails' 61 ms is the three `git` calls (`ls-files -s` 7 ms,
  `diff-files` 9 ms, `ls-files -o` 24 ms); the rest is rewriting the file map.
  Rubydex pays 177 ms on rails and 845 ms on GitLab for the same no-op (PLAN
  §8), *and* pays it again on every process boot.
- **A second worktree costs ~0.2 s and zero parses.** A `--shared` clone of
  rails indexes with `parsed: 0` — the facts were already on disk.
- **One edited file costs ~75 ms**, of which ~61 ms is the scan floor above.
- Cold time is not the headline and is not uniformly better than Rubydex's
  (rails 1.5 s vs their 1.35 s index+resolve; discourse 3.2 s vs their 2.4 s).
  About 0.3 s of ours is the `ANALYZE` that keeps queries fast — a cost paid at
  write time so it is not paid at read time.
  The difference is that ours happens once per machine and theirs happens once
  per process, and ours ends with the facts on disk rather than in RAM. It is
  also not yet a like-for-like comparison: they resolve, and this layer does not.

Fact shape across all three corpora (2.2 M call sites):

| receiver shape | share | what it costs to resolve |
|---|---:|---|
| implicit | 44.6 % | nothing — the enclosing class is the receiver |
| other | 26.1 % | chains, literals, operators — the residue |
| local | 14.3 % | a constructor / identity walk |
| const | 11.4 % | constant resolution |
| ivar | 3.1 % | an assignment walk |
| self | 0.4 % | nothing |

So **56 % of call sites need no inference at all**, and 71 % are reachable by
the first three rungs of the ladder. (rwr measured implicit self at 53–66 %;
the gap is a counting difference — this figure includes operator calls, which
inflate `other`.)

Sorbet `sig` extraction is exercised at scale on `graph_weaver`: 3,757 of its
methods get a concrete return class. None of the three corpora above use
Sorbet, so the sig path contributes nothing to their numbers.

**The query planner needs statistics, and this is not optional.** Without them
SQLite plans `--refs` as a nested scan of the checkout's files: `--refs new` on
rails took **90 seconds** for 13,684 rows. With `ANALYZE` run, the planner
reverses the join — files drive, a bloom filter rejects — and the same query
takes **66 ms**. `PRAGMA optimize` on close is what keeps it that way; it
re-analyzes only tables that have grown enough to matter, so a no-op reindex is
still 61 ms. Anyone adding a query over these tables should check
`EXPLAIN QUERY PLAN` on a *populated* database — a fixture-sized one hides this
entirely.

| `--refs NAME` on rails | rows | time |
|---|---:|---:|
| `find_each` | 26 | 8.8 ms |
| `save` | 625 | 11 ms |
| `each` | 1,994 | 26 ms |
| `new` | 13,684 | 70 ms |

**Where the bytes go.** 236 MB for the three corpora, ~10 KB per blob. Measured
with `dbstat`:

| | MB | share |
|---|---:|---:|
| `call_site` + its two indexes | 171 | **72.5 %** |
| `const_ref` + indexes | 37 | 15.8 % |
| `def` + indexes | 21 | 8.9 % |
| `file`, `blob`, everything else | 7 | 2.8 % |

Call sites are the index. Extrapolating linearly to a 100k-file repo gives
~1 GB. Two things that would move the number are recorded but **not** acted on
until there is a reason: the `other` receiver shape is 26 % of call-site rows
(so ~19 % of the whole database) and may not earn its bytes once the resolve
layer says what it can do with it; and `call_site_blob` / `const_ref_blob`
(34 MB, 14 %) exist for a foreign-key cascade that DEC-003 means never fires. Not
optimized, deliberately: the encoding is boring on purpose and there is no
measurement yet saying it needs to be otherwise.

*Provenance: the local `discourse` and `mastodon` checkouts carry no `.git`, so
discourse was staged into a scratch git repo to be measured. That is DEC-001
biting on a real corpus.*

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
