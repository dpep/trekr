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

## DEC-007 — The tree layer is rebuilt, never invalidated

**Decided.** Every invocation assembles the whole checkout's namespace from SQL.
No incremental machinery, no persistence, no memo keyed on contributing blob
OIDs.

**Why.** PLAN §4 took the Glean/Kythe lesson — per-file facts cache perfectly,
the cross-file graph is where invalidation bites — and said keep the tree cheap
to rebuild. Measured: 41 ms for rails, 58 ms for discourse's 11k files. At that
price an invalidation scheme buys nothing and costs a whole class of staleness
bug. Linearization is memoized *within* one build, which is where the repeated
work actually is.

**Reverses if** a resident LSP front makes per-keystroke rebuilds visible, or a
100k-file repo pushes the rebuild past ~200 ms. The first fix then is caching
one built tree per process, not patching one in place.

**Update (gems).** The rebuild has now crossed that line: 202 ms on rails,
309 ms on discourse, against 43 ms when this was decided. The progression —
43 ms constants, 120 ms once method tables arrived, 202 ms once gems did, with
CRuby unmoved at 116 ms because it has no gems — says the cost is assembling a
larger namespace, not querying it (batching 258 per-gem queries into 3 moved it
233 → 221 ms). The decision **stands** for now, because the named remedy is a
per-process cache and a one-shot CLI invocation builds the tree exactly once
either way. What has changed is that a resident front is no longer optional if
sub-100 ms answers are wanted: that is PLAN Phase 4's job, and it is now on the
critical path rather than a nicety.

**Update (the resident front, measured).** The named remedy — "caching one built
tree per process" — is now deployed and does its job, so the decision **stands**
and the reverses-if is spent rather than triggered. One `--serve` session,
release build, client rooted at an unrelated repo, `goToDefinition` repeated:

| checkout  | first request | every one after |
| --------- | ------------- | --------------- |
| rails     | 508 ms        | 0.21–0.35 ms    |
| discourse | 975 ms        | 0.49–0.66 ms    |

End-to-end per request, from the serve log, so the tree build is the bulk of the
first number and nothing but parse-and-resolve is in the rest. A session holds a
tree per checkout (DEC-024), and returning to rails after discourse's build was
still 0.23 ms — the memo holds both, it does not thrash. The staleness re-check
(schema version + file count, two queries per request) is inside those warm
numbers, so it is not worth avoiding.

What this does **not** fix is the cold first query, now 0.5–1.0 s per checkout,
and the CLI, which pays a fresh build on every invocation — 0.31 s for a rails
`--def`, 0.67 s for discourse, end to end. Both are the same unpaid bill:
nothing persists an assembled tree between processes. Incremental *patching* is
still the wrong answer to it; persisting or lazily assembling one is the
question worth opening, and only if the cold second matters more than the
staleness class it would reintroduce.

**Update (what decides a rebuild).** The decision — rebuild whole, never patch
— stands. What was wrong was the *trigger*. A resident session keyed its tree on
(schema version, file count), and **editing a file moves neither**, so a session
went on answering from a tree assembled before the edit. Adding a file happened
to work, which is why nothing caught it.

The trigger is now content-derived. Each blob carries a `surface`: a digest of
exactly the facts the tree layer reads — its definitions and its ancestry edges,
positions included — and nothing else. A checkout folds every file's path
together with its blob's surface into one `surface_key` at index time, so the
staleness check stays a single-row read rather than an aggregate over the map.
It moves whenever any answer would, and it does not move when only method
*bodies* changed.

**Measured, and this is the number the edit-churn design rests on.** Over the
last 500 commits of rails, discourse and CRuby — 5,158 modified Ruby blobs —
**71 % leave the definition structure identical**, and **46 % additionally leave
every definition on its original line**. Per corpus: rails 65 %, discourse 70 %,
CRuby 79 %. Reproduce with `script/bench.py`'s neighbour, `script/churn.py`.

Positions are in the digest deliberately, and that is what costs the difference
between those two numbers. The tree carries each definition's site, so a
definition that merely moved still changes an answer. Including positions buys
correctness by construction for 46 % of edits; excluding them and patching the
moved sites afterwards would buy 71 %, at the price of a patch that has to be
right. The 25 points are available whenever someone wants to write that patch,
and the mechanism to key it is already here.

## DEC-008 — Constant confidence is 1 or 0, and the doubt is reported separately

**Decided.** A resolved constant carries `confidence: 1.0`, a residue `0.0`.
There is no decay by ladder depth.

**Why.** The house rule is that a score must be derived from what backs it, and
that a flat score is a guess wearing the clothes of a measurement. The honest
reading here is that the judgement genuinely *is* binary: the ladder is Ruby's
own constant lookup, so within the indexed set a hit is what Ruby would find,
not a ranked guess. Inventing a decay constant per rung would be exactly the
fake measurement the rule warns about. What is genuinely uncertain — that the
index is partial — is reported as countable evidence instead: `scopes_tried`,
and `unresolved_ancestors` when a gem superclass truncated the chain, so a
residue with an incomplete chain is visibly a weaker "no" than one without.

**Reverses if** the method ladder arrives (session 3), where the rungs have
*measured* yields (rwr: 64% of sigs name a usable class against 3.9% from
syntax). Grading there will be derived from those numbers, not picked.

## DEC-009 — A schema change drops the database instead of migrating it

**Decided.** `store::init` compares `user_version` and, on a mismatch, drops
every table and recreates. `MIGRATIONS` no longer exists.

**Why.** Every row below `blob` is a pure function of bytes this machine can
read again — the database is a **cache**, not a system of record. Reindexing
costs seconds (1.5 s for rails) and removes an entire class of bug: a migration
that half-converts, or that has to reason about facts extracted by an older
extractor. This came up immediately: renaming `ancestry.nesting` to `owner`
changed what the column *means*, so a column rename would have silently kept
wrong data.

**Reverses if** indexing ever becomes expensive enough that a rebuild is a real
cost — gems keyed by `(gem, version)` might get there, since they are shared
across projects.

## DEC-010 — A partial ancestor chain reports, it does not stop

**Decided.** When linearization cannot resolve an ancestor, the chain continues
without it and the answer carries `unresolved_ancestors`.

**Why.** Rubydex stops at the first unresolved ancestor and retries, so that a
later ancestor cannot win a lookup an earlier unresolved one might have
shadowed. That is right for them: they index RBS core, so a partial chain is
rare and usually temporary. Here gems are not indexed at all, so nearly every
Rails model has an unresolved `ActiveRecord::Base` — stopping would turn almost
every answer into a refusal. Full disclosure says return the ranked answer with
the reason, not nothing.

**Reverses if** gem indexing (PLAN Phase 3) lands, at which point a partial
chain becomes rare enough that stopping is the more accurate choice.

## DEC-011 — Method confidence is a count of agreeing evidence

**Decided.** A rung that is a *language rule* — implicit or explicit `self`, a
constant receiver — reports `confidence: 1.0`. A rung that *infers* a type from
assignments reports `agreeing / total`: two assignments to one local that name
different classes give `0.5`, and `agreement: "1/2"` travels with it.

**Why.** DEC-008's discipline extends: a grade must trace to a count or be 1/0.
The measured yields available (rwr D61/D62: 64 % of sigs name a usable class,
implicit self is 53–66 % of call sites) are **coverage** numbers — how often a
rung applies — not **accuracy** numbers. Using coverage as confidence would be a
category error dressed as rigour. What *is* countable is how much of the
evidence agreed, so that is what the number reports.

The assignment scan is file-wide rather than flow-sensitive on purpose: an
assignment in an unrelated method still votes, which over-counts disagreement
and pushes confidence **down**. For a number a caller may act on, erring low is
the safe direction.

**Reverses if** the TracePoint gold set (PLAN §5) is built. Then each rung has a
measured accuracy and confidence can be calibrated rather than counted — which
is the only honest way to make these numbers comparable across rungs.

## DEC-012 — Assignments are extracted but never stored

**Decided.** `Facts::assigns` is produced by the extractor and written to no
table. The local and instance-variable rungs read it from the reparse `--def`
already does.

**Why.** What a local holds is a question about one file, and the answer is
wanted only at the moment someone asks about a position in that file. Storing it
would add roughly as many rows as `call_site` — already 72 % of the database —
for a fact that never crosses a file boundary. The layer split stays clean: the
blob layer stores what other files need, and this is not that.

**Reverses if** cross-file ivar typing is wanted (`@foo` assigned in a concern,
used in the model). That is a real gap, and it is the point at which these stop
being a within-file question.

## DEC-013 — The cache version covers the extractor, not just the schema

**Decided.** `schema::VERSION` is bumped for any change to *what the extractor
emits*, not only for changes to table definitions.

**Why.** Facts are cached by blob OID on the premise that they are a pure
function of the bytes. True — but when the *function* changes, identical bytes
must still be re-read. This bit within one session: fixing class-body call
dispatch changed no table, so every already-indexed blob stayed "known" and the
fix shipped dead until the version moved. A stale cache that looks fresh is
worse than a slow one.

**Reverses if** the extractor is ever versioned separately from the schema —
which would be worth doing if reindexing became expensive, since an extractor
change need only invalidate the fact tables and not the checkout map.

## DEC-014 — `--jobs` defaults to physical cores, but the writer is the real cost

**Decided.** `--jobs 0` (the default) picks `num_cpus::get_physical()`. Not
capped.

**Why, and what the measurement actually said.** User feedback reported index
times improving ~25 % when jobs moved closer to the physical core count. A/B on
discourse (11.3k files, 1.23 M call sites), best of two cold runs each, Apple M2:

| jobs | wall | scan | parse | store-write | parse MB/s |
|---:|---:|---:|---:|---:|---:|
| 1 | 4.03 s | 82 ms | 1267 ms | 2633 ms | 33 |
| 2 | 3.24 s | 78 ms | 542 ms | 2565 ms | 78 |
| **4** | **2.92 s** | 74 ms | 280 ms | 2519 ms | 150 |
| 6 | 3.01 s | 75 ms | 249 ms | 2639 ms | 169 |
| 8 (auto) | 3.06 s | 86 ms | 271 ms | 2660 ms | 155 |
| 12 | 3.03 s | 78 ms | 240 ms | 2673 ms | 176 |
| 16 | 3.20 s | 105 ms | 306 ms | 2748 ms | 137 |

Three things, in order of how much they matter:

1. **The store write is 85 % of the wall time and is flat in `jobs`.** Parse
   speeds up 5× from 1 to 4 workers and then stops mattering, because it is
   only ~8 % of the total. rq's theory that a single SQLite writer serializes
   anyway is not just right, it *dominates* — ~1.5 M row inserts at ~575k/s.
   **This, not the worker count, is where index time goes.**
2. **The 25 % is reproducible in shape but not in cause.** 1 → 4 jobs is 27 %
   here. The logical-vs-physical distinction the feedback attributed it to
   could **not** be tested on this machine: Apple Silicon reports
   `hw.ncpu == hw.physicalcpu == 8`, so auto is unchanged by the switch. On an
   SMT x86 box it would differ, and that remains unmeasured.
3. **The flat region is wide (4–12) and physical cores lands inside it.** On
   this machine 4 — the *performance*-core count — is marginally best, but
   2.92 vs 3.06 s is inside run-to-run noise. The four efficiency cores
   contribute nothing measurable.

So the default is defensible rather than optimal, and uncapped because the
plateau is flat rather than falling.

**Reverses if** an SMT machine shows logical cores actually hurting (then cap at
physical and say so), or if the store write is made concurrent or substantially
faster — at which point parse becomes the majority and the worker count starts
to matter for real.

## DEC-015 — Ruby core is a vendored Ruby stub, not RBS

**Decided.** `src/tree/core.rb` is ~1000 lines of ordinary Ruby with empty
method bodies, read at tree-build time by the same extractor that reads a
checkout. Not RBS, not a hand-written Rust table.

**Why.** Three candidates:

- **Vendored core RBS** is the most accurate source, but consuming it needs an
  RBS parser. `ruby-rbs` is a C-binding crate, and PLAN §2 says consume RBS
  opportunistically and never require it. A required C dependency for the
  *baseline* case is the wrong trade.
- **A Rust table** (Rubydex's `built_in.rs` shape) needs no parser but invents a
  second way to say what a class is, which then has to be kept in step with the
  first.
- **A Ruby stub** needs nothing new. It goes through `extract()` and
  `Tree::assemble` exactly as a checkout does, so it is covered by every test
  those already have, and a contributor extends it by writing the method they
  went looking for. Bodies are empty because only names, arity, and ancestry
  are load-bearing.

The ancestry matters more than the method lists: the implicit `< Object` on
every class is what makes `Kernel#puts` reachable at all, and the
`Class → Module → Object` tail on singleton chains is what makes `Foo.new` and
a class body's `prepend` resolve.

Cost: reparsed on every tree build, ~1 ms against ~120 ms. A cache would need
the same invalidation rule DEC-013 exists for, and is not worth it.

**Reverses if** the stub grows past the point where hand-maintenance is
credible, or a pure-Rust RBS parser appears. The seam is right for either: both
would produce the same `DeclRow`/`EdgeRow`/`MethodRow` triple.

*Sorbet's `sorbet/rbi/` is a fourth source, and a good one for repos that have
it — Tapioca enumerates gem and DSL methods as Prism-parseable Ruby. It is not
built here because it is a per-repo source rather than a baseline, and building
both at once would leave neither measured.*

## DEC-016 — Gems are located by reading, never by running Ruby

**Decided.** `Gemfile.lock` is parsed directly; gem sources are found by
convention (`vendor/bundle/ruby/*/gems`, `$GEM_HOME`, `$GEM_PATH`, rbenv, rvm,
asdf, Homebrew, system). No `bundle`, no `gem`, no `ruby`.

**Why.** Shelling out to `bundle list --paths` would be more accurate and would
cost the product its first edge (PLAN §1): it needs the project's Ruby
installed, its bundle resolved, and its native extensions built. Reading a
documented text file and stat-ing a conventional directory needs none of that,
and works on a checkout of a repo you have never run.

**Degrading honestly.** A gem the lockfile names and disk does not have is
*reported*, not silently absent — it is a hole in every answer that would have
come from it. Path-sourced gems are excluded from that report because their
code is inside the checkout and already indexed; counting them would make
rails' own lockfile look 12 gems broken.

**Reverses if** convention stops predicting layout — a packager that unpacks
somewhere new. The fix then is another search root, not a subprocess.

## DEC-017 — A gem is keyed by its directory, and only `lib/` is read

**Decided.** Each located gem is indexed as its own checkout, rooted at the
unpacked directory (which already encodes `name-version`). Only `lib/` is
walked. A gem already present in the store is skipped outright.

**Why.** The directory key gives cross-project sharing for free: two projects
resolving `activesupport 7.1.0` name the same path, so the second pays nothing.
A gem's bytes never change, which makes "have I seen this root" a complete
incremental test — no OID diff needed. Measured on rails: 86 gems, 1 897 files,
indexed once; the second run reports 82 already known and reads nothing.

`lib/` because that is where a gem's public code is. `spec/` and `test/` are
often larger than `lib/` and are never navigated to from a consuming project;
`ext/` is C.

**Reverses if** a gem that matters puts code outside `lib/` — then widen the
walk for that shape rather than indexing everything. Also worth revisiting if
DB size becomes the binding constraint: a gem arguably needs `def` and
`ancestry` rows but not its 1.2 M call sites, since nobody asks "who calls this"
*inside* a dependency.

## DEC-018 — RBI needed no ingestion path; it needed measuring

**Decided.** `sorbet/rbi/**` is indexed by the ordinary checkout scan, because
`.rbi` was already in the Ruby extension list. No separate ingestion.

**Why.** An RBI *is* Ruby — `sig { ... }` plus bodiless `def`s — so the
extractor and the sig reader already handle it. Measured on graph_weaver:
27 219 defs and 3 620 sig returns come from `sorbet/rbi/gems/` with no code
written for it.

What the measurement then said is the useful part. Those sigs describe **gem**
methods, and graph_weaver's own `lib/` has 570 defs with **36** sigs. The
prediction on record — that a Sorbet repo's sig density would move its method
resolution far more than rails' — was wrong, and wrong in a specific way: rwr's
64 % is *of the signatures that exist, how many name a usable class*. It is a
property of signatures, not coverage of call sites. A repo can be full of RBIs
and still have almost no typed call sites of its own, because the RBIs describe
what it depends on rather than what it is.

RBI pays where code calls a gem method and keeps the result. It does not pay
where code calls its own untyped methods, which is most of what code does.

**Reverses if** a repo with dense first-party sigs is measured (Shopify-scale
Sorbet adoption) — the mechanism is built and tested, and only the corpus is
missing.

## DEC-019 — A Tapioca-generated method answers with the model

**Decided.** When a resolved method's only definition is under
`sorbet/rbi/dsl/`, the answer's sites are the *owner class's* real declarations
and `resolved_via` is `rbi_dsl`.

**Why.** Those methods are generated at runtime by Rails and have no source.
Sorbet's own go-to-definition lands in the generated file, which is the wrong
place to send someone reading code — beating that is the reason to consume RBIs
at all. If the class exists *only* in the RBI there is nowhere better to point,
so the generated site is kept rather than dropped.

**Unmeasured, and said plainly**: no corpus available here has a
`sorbet/rbi/dsl/` directory (graph_weaver and sorbet-uuid have `gems/` only).
The behaviour is unit-tested against a synthetic fixture and has never been run
against real Tapioca output.

**Reverses if** the redirect proves misleading in practice — e.g. a model whose
generated methods a reader genuinely wants to inspect. The fix then is to
report both locations rather than to pick differently.

## DEC-020 — Chained receivers are not attacked; 40 % typed plus ranked residue is the product

**Decided.** The `other` receiver bucket — chained calls, literals-as-receivers,
block parameters — gets no dedicated rung. Resolution stays where it is and the
effort goes into disclosing the residue well.

**Why.** Two independent measurements agree, which is why this is a decision and
not a deferral:

- rwr's D61 already measured the bucket: chained receivers are 15.8–27.4 % of
  call sites, but `X.new` is under 4 % of chains, **70 % of method definitions
  end in another call** (so the type would have to come from a return type that
  does not exist), and 20–25 % of chains are `expect(...)` — spec DSL, not a
  navigation target.
- Session 5's own split says the ceiling is a *type source*, not resolver
  effort. On rails, where the index is essentially complete (2 truncated
  samples in 120), resolution is 40 % and the residue is `local` 28 % + `other`
  16 %. Adding rungs to chase types that were never written cannot move that.

So the honest position: **40 % resolved with named rungs, plus ranked residue
carrying the receiver shape and the reason, is the product for untyped Ruby.**
No other tool ships even that — Ruby LSP's fallback for an unknown receiver is
the first ten methods with that name, and Rubydex does not attribute method
calls at all.

**Reverses if** a corpus arrives with dense first-party `sig`/RBI coverage —
the target repo is ~30 % Sorbet, which is the real test, and DEC-018 already
showed that a repo full of RBIs describing its *dependencies* is not that test.
Or if a new type source appears (RBS in the wild, a Ruby with inline types).
The rungs are built and tested; only the corpus is missing.

## DEC-021 — Exclusions are counted by reason, because the reasons are not equally strong

**Decided.** `--refs Owner#method` reports `excluded` broken into
`different_owner`, `no_such_method`, and `arity`, and `--include-excluded` lists
every ruled-out site with its reason.

**Why.** Auditing the first real run found the problem. `Arel::SelectManager#where`
on rails excludes 1 368 call sites, and a sample showed most of them are
`Topic.where(...)`, `Author.where(...)` — ruled out because **nothing indexed
defines `where` on `Topic`**. That is right for this query, and the *reasoning*
is unsound in general: Rails writes `delegate :where, to: :all`, so the method
is absent from the index without being absent from the program. A method a DSL
defines looks exactly like a method that does not exist.

Only `different_owner` is positive evidence — the receiver resolves and Ruby's
lookup lands somewhere else, so this call provably is not the queried one. On
that same query it is 21 of 1 368. `arity` is sound against the definition we
have, which is all a syntactic check can claim. `no_such_method` is the
1 260-strong majority and the weakest.

Blending them into one number would have made the product's headline claim
mostly rest on its weakest reason without saying so. Keeping the behaviour
(these sites are not listed) and splitting the count is the honest shape: the
answer is still far better than a grep, and the caller can see exactly how much
of it is inference.

**Reverses if** DSL-defined methods get modelled (`delegate`, `define_method`,
`scope`, the Rails family — PLAN Phase 3). Then `no_such_method` becomes nearly
as strong as `different_owner`, and the split stops earning its keep.

## DEC-022 — Schema attributes attach by convention, at extraction

**Decided.** `create_table "posts"` in `db/schema.rb` emits attribute methods
under `Post`, applying Rails' table-to-model convention in the **extractor**.
Generated names are getter, setter, and predicate; the reader is typed from the
column's SQL type.

**Why.** `posts` → `Post` is a pure function of the table name, so it is a blob
fact and belongs where blob facts are made. The point is not that `post.body`
exists but that it is a `String`: a column type names a class, which turns every
attribute into a typed receiver — the cheapest type source in a Rails app, and
ruby-lsp-rails' capability without a running app.

**The cutoff.** Getter, setter, predicate. The dirty-tracking family
(`_changed?`, `_was`, `_before_last_save`, `_will_change!`, …) is a dozen names
per column for a small fraction of the calls; `boolean` columns get no type
because `true` and `false` are different classes and neither is a useful
receiver.

**Known gap, stated plainly**: a model overriding `self.table_name` is not
matched. The override is in a different blob from the schema, so honouring it
means either a schema-column fact table or a tree-time join — neither of which
earns itself for the small share of models that do it.

**Reverses if** `self.table_name` turns out to be common in the target repo, or
if schema facts are wanted for anything besides attribute methods. Then the
column list becomes a stored fact and the convention match moves to the tree.

## DEC-023 — Core is written out beside the database so it has a location

**Decided.** `core.rb` is compiled into the binary *and* written to the
database's directory on first use. LSP locations for a core definition point at
that file; the CLI keeps printing `<core>`.

**Why.** The baseline found this: `require`, `Array#each` and `Module#undef_method`
resolved correctly and then answered **nothing**, because the stub had no file
to point at. ruby-lsp sends you to an RBS declaration — not source, but a
readable signature — and that is plainly better than silence.

Writing the file out is the cheapest honest fix. It is rewritten only when it
differs, so an editor watching it is not churned on every index; and the file's
own header says what it is, so nobody mistakes it for Ruby's real source.

The CLI keeps the `<core>` marker because there it is *information*: a JSON
consumer wants to know the answer is core rather than the project's code, and a
path to a generated stub would obscure that. An editor cannot open a marker,
which is why the two surfaces differ.

**Reverses if** real core sources or RBS become indexable, at which point the
stub stops being the best available answer.

## DEC-024 — The unit is the file's checkout, not the caller's directory

**Decided.** A question about a *position* is answered against the repository
that contains that file. `--def FILE:LINE:COL` runs `repo_root` on the file, not
on `.`; `--serve` holds a tree per checkout and finds the one each request's URI
belongs to. The client's workspace root survives only as the scope for
`workspaceSymbol`, and even there it widens to every checkout when it is not one
itself.

**Why.** Both P0 defects from the first live LSP session were this, wearing two
hats. `--def` on a rails file, run from a Rust repo's directory, built rails'
question against rq's namespace and answered `residue` with `"no indexed
constant by that name"` — a reason that reads like a finding about rails rather
than what it was, an artifact of `cd`. And `--serve`, rooted by Claude Code at
the session's cwd, could not make any rails path relative to that root, so all
nine operations returned empty; `documentSymbol`, which needs no index at all,
returned empty too, which is what made the serve layer rather than resolution
the suspect.

The premise was that a caller stands inside the code it asks about. Editors do.
Agents do not: they hold absolute paths and query across repositories from
wherever the session happens to be. The file's own repository is the only thing
in the request that identifies a checkout, so it has to be the key.

Two consequences worth stating. Finding a checkout forks `git rev-parse`, so the
serve session memoizes it per directory — including the negative answer, so a
file in no repository is not re-asked. And a question needing only a file's bytes
(`documentSymbol`, diagnostics, `callHierarchy/prepare`) no longer requires a
checkout at all; requiring one was pure coupling.

**Measured.** With the client rooted at rq (a Rust repo) and the file in rails:
`documentSymbol` 0 → 122 symbols, `definition` on `Batches` 0 → 2 sites,
`hover` null → `Resolved · confidence 1.0 · via Lexical`. Before the fix the
serve log showed all three answering in 0.03–0.05 ms, far too fast to have
looked at anything.

**Reverses if** a single session ever needs to answer for two checkouts that
disagree about the same absolute path — which git worktrees do not do, since
each has its own root.

## DEC-025 — The assembled tree is not persisted

**Decided.** The tree stays an in-memory artifact, rebuilt from the store by
whichever process needs it and cached for that process's lifetime (DEC-007). It
is not serialized to disk, and worktrees at the same commit do not share an
assembled tree — only the blob facts underneath it, which they always have.

**Why, measured.** The case for persisting is the rebuild cost, so that is what
was measured. A whole-checkout tree build is **0.32 s on rails** and **0.73–0.84 s
on discourse** (five runs each, isolated with `--ancestors`, which builds the
tree and then does almost nothing). Of rails' 0.32 s, reading the rows is about
a quarter — 74 ms to touch every column of the checkout's 65,227 definition rows
— and the rest is assembly: allocation, the namespace map, linearization.

Persisting would replace all of it with a deserialize of a few tens of megabytes.
Optimistically that is 2–4×. Set against:

* the resident front already amortizes the same cost to **0.2 ms** — a 1600×
  win, available today, and the surface it is reached through is the one this
  product tells agents to use;
* a serialized tree is a second on-disk format with its own version, its own
  corruption mode, and its own staleness surface — where the store today is a
  cache of a pure function that can always be dropped and rebuilt (DEC-009);
* the CLI is the only caller that pays per invocation, and the answer for a
  caller that minds is `--serve`.

A 2–4× on the surface we are steering people away from, bought with a new
persistent format, is not a trade worth making yet.

**Reverses if** the resident front stops being the primary surface, or a
checkout appears where the rebuild is slow enough that even one payment per
session is intolerable — the shape to watch is a repo whose build passes a few
seconds, not a few hundred milliseconds. The key to persist against already
exists: `checkout.surface_key` names exactly the tree that would be stored, so
the work would be serialization and nothing else.
