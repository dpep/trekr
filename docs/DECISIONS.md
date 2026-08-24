# Decisions

ADR-lite: what was decided, why, and what would reverse it. Check this before
proposing an alternative — the rejections carry the reasoning that settled them.

## DEC-001 — A git repository is required

**Decided.** `--index` refuses a directory that is not a git checkout.

**Why.** Content addressing is the product, and `git ls-files -s` hands us the
OID of every tracked file for free. Without git we would hash every byte on
every run, which is a different tool with different performance. Supporting both
would mean two scan paths and two performance stories before either is measured.

**Reverses if** indexing gem source outside a checkout becomes necessary (PLAN
Phase 3 keys gems by `(gem, version)`, which may want exactly this). The seam is
already right: a non-git walker would return the same `Files` map, so the change
is additive.

## DEC-002 — Four fact tables, not one generic one and not seven

**Decided.** `def`, `ancestry`, `const_ref`, `call_site`.

**Why.** A generic `fact(kind, …)` table is untyped mush that every query has to
re-discriminate. At the other end, splitting `superclass` from `include` would be
splitting one concept: both say "this scope gains that ancestor", and the
linearization order they imply belongs to the tree layer. Conversely,
`const_ref` and `call_site` stayed separate despite a similar shape — they are
resolved by different machinery (lexical nesting vs the receiver ladder) and
merging them buys nullable columns and a discriminator on every query. Two
similar things are duplication; tolerate it.

**Reverses if** a fifth fact kind arrives that fits none of them, or if the tree
layer finds itself joining `const_ref` and `call_site` constantly.

## DEC-003 — Blobs are never garbage collected

**Decided.** `--drop` forgets a checkout's file map; the blobs stay.

**Why.** A blob no checkout currently references is exactly the blob a branch
switch back would want, and re-reading bytes we have already parsed is the one
cost this design exists to avoid. Deleting on drop would also break the shared
case in the obvious way.

**Reverses if** measurement shows the database growing past what the sharing
saves. Then the fix is an explicit `--gc`, not an implicit sweep.

## DEC-004 — `private :foo` is a definition row, marked by `via`

**Decided.** A bare visibility call with symbol arguments emits a `def` row with
`via = 'private'` (or `protected`/`public`/`module_function`) and no parameters.

**Why.** `private :foo` can target a method inherited from an ancestor, so it is
not a mutation of an already-emitted definition — Ruby creates an implicit entry
on the child. It needs to be its own fact. Giving it a table of its own for one
column's worth of difference is worse than widening the meaning of a `def` row
to *"this blob asserts something about a name in a scope"* — most rows assert
existence, a few assert only visibility, and `via` is already the column that
tells them apart.

**Reverses if** the tree layer finds the distinction expensive to re-derive at
query time.

## DEC-005 — Traverse with Prism's `Visit`, not a generated `children()`

**Decided.** Scope state lives on the visitor: push a frame, call the free
`ruby_prism::visit_*` to descend, pop.

**Why.** rwr generates a 3.8k-line `children()`/`dup()` table because it compares
and duplicates trees. One-way extraction never needs a node out of its visit, so
the stack-on-self shape gets the same threaded state for none of the code.
Overriding `visit_statements_node` to walk the statement list by hand is also
what makes Sorbet `sig` pairing free: a sig and the thing it describes are always
adjacent statements.

**Reverses if** a future pass genuinely needs to hold nodes across visits.

## DEC-006 — Query speed comes from statistics, not from a planner override

**Decided.** `PRAGMA optimize` runs on every `Store` close. `--refs` is written
as a plain join with no `INDEXED BY`.

**Why.** Without statistics, `--refs new` on rails takes 90 s; `INDEXED BY
file_blob_checkout` brings it to 50 ms and `ANALYZE` brings it to 45 ms. The
hint is worse than the statistics at the same speed: it silently pins one plan,
breaks if the index is ever renamed, and teaches nothing to the *next* query
somebody writes over these tables. `PRAGMA optimize` re-analyzes only tables
that have moved, so a no-op reindex stays at 67 ms.

**Reverses if** a query is found that the planner gets wrong even with current
statistics. Then pin that one query and say why in a comment, rather than
adopting hints generally.
