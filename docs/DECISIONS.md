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

**Revisited by DEC-049:** still true of a blob some checkout maps; a checkout
nothing can reach any more is collected, blobs only it mapped with it.

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

**Update (measured on a real app, and the rule generalized).** Session 12
banked the question "does `sorbet/rbi/` ingestion ever actually fire, or does
the DSL extractor always answer first?" It fires, and it was firing far too
eagerly.

widget_shop commits `sorbet/rbi/gems/` — a stub for every gem method the app
calls. Those stubs are indexed as part of the checkout, *after* the gems
themselves, and the method table took the last definition it saw. So **18 of
36 resolved app-code answers pointed at a Sorbet signature instead of the
code**: `belongs_to` resolved to `activerecord@8.1.3.1.rbi:2730` rather than
`associations.rb:1824`, with the owner exactly right both times.

Worse, the guard that was supposed to prevent this had been dead since session
12. It tested `path.starts_with("sorbet/rbi/dsl/")`, and site paths became
absolute when they were rooted to their own checkout — so it stopped matching
anything real, while its unit test went on passing against a synthetic
relative path. A rule with a test that cannot see the regression it exists to
catch is worth less than no rule.

The rule is now general and stated once: **an `.rbi` is a declaration, never an
implementation.** At a given owner, a real definition wins; the stub is used
only when it is all there is. This subsumes DEC-019's original case rather than
sitting beside it.

**Measured, app code, 63 sites against runtime truth:** correct 19 % → **43 %**,
confidently wrong 32 % → **8 %**. Of plain (non-Rails-generated) methods:
correct 27 % → **60 %**, found-the-definition 47 % → **80 %**.

**Update (the stub owns the chain, not just the site).** The sigs-on/sigs-off
experiment showed the rule above was only half of it. Preferring real source
*within* an owner fixed which file a method pointed at; it did nothing about
Tapioca describing methods in owners that **do not exist at runtime** —
`Widget::CommonRelationMethods`, `Widget::GeneratedAttributeMethods` — which
sit early in the ancestor chain and so win the lookup outright. `Widget.find`
answered from the RBI while Ruby dispatches to
`ActiveRecord::Core::ClassMethods`.

The rule now spans the chain: **real source wins the whole chain before a
declaration wins any of it.** Ruby's ancestor order is walked twice, the first
pass skipping `.rbi` declarations entirely, the second admitting them — so a
stub is still the answer when nothing real defines the name anywhere.

**The cost, stated plainly:** a genuine override declared *only* in an `.rbi`
now loses to a real definition further down the chain. That is a real
regression class, accepted because the measurement says the shadow case
dominates it and because residue candidates still disclose the alternative.

**Measured** on widget_shop with sigs on, 63 app sites: correct 42.9 % →
**46.0 %**, confidently wrong 7.9 % → **4.8 %**. That closes half the gap to the
sigs-off column (49.2 %), which is the same app with `sorbet/` deleted.

**Reverses if** a corpus appears where `.rbi`-only overrides are common and
correct — hand-written `sorbet/rbi/shims/` rather than generated `dsl/` and
`gems/` would be the shape to watch, since a shim exists precisely to say
something the source does not.

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
a partially-typed monorepo is the real test, and DEC-018 already
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

**Reverses if** `self.table_name` turns out to be common in real corpora, or
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
on `.`; `--lsp` holds a tree per checkout and finds the one each request's URI
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
  caller that minds is `--lsp`.

A 2–4× on the surface we are steering people away from, bought with a new
persistent format, is not a trade worth making yet.

**Reverses if** the resident front stops being the primary surface, or a
checkout appears where the rebuild is slow enough that even one payment per
session is intolerable — the shape to watch is a repo whose build passes a few
seconds, not a few hundred milliseconds. The key to persist against already
exists: `checkout.surface_key` names exactly the tree that would be stored, so
the work would be serialization and nothing else.

## DEC-026 — Path comparisons go through audited helpers, not a newtype

**Decided.** Every "is this path inside that one" and "does this path name that
file" goes through `core::paths::{under, names_file}`, whose tests use real
absolute paths. Two path *kinds* exist — store-absolute and checkout-relative —
and they are **not** distinguished by the type system. That was considered and
deferred; see below.

**Why the sweep happened.** Two selection rules were found silently dead, each
with a unit test that passed against a synthetic relative path while the real
one was absolute: DEC-019's `starts_with("sorbet/rbi/dsl/")` guard, and the
residue ranker's `site.path == path` same-file signal. Neither failed loudly;
both simply never fired. A test that cannot see the regression it exists to
catch is worth less than no test.

**What the sweep found.** Every path comparison, prefix test, join, strip and
canonicalization in `src/`:

| site | verdict |
| ---- | ------- |
| `Tree::in_checkout` — `starts_with(root)` | **bug**: `/a/repo` claimed `/a/repo2/x.rb`. This machine has `widget_shop` *and* `widget_shop-nosorbet`, so it was live. |
| residue ranker, same-file — `ends_with(path)` | **bug**: `b.rb` matched `/x/ab.rb`. (Was `==`, dead, until session 14 made it loose.) |
| `Store::checkout_containing` — `?1 LIKE root \|\| '/%'` | **bug**: `_` is a LIKE wildcard, so `widget_shop` matched `widgetXshop`. Masked by `ORDER BY LENGTH(root) DESC`. Now an exact `substr` prefix test. |
| `find_git_checkout` — `starts_with("{name}-")` | **bug**: `rails` claimed `rails-html-sanitizer-<sha>`. The remainder must now be a revision. |
| `Site::is_rbi` — `ends_with(".rbi")` | sound: an extension test does not care about shape. |
| `Site::is_dsl_rbi` — `contains(DSL_RBI)` | sound since session 13; `contains` is shape-independent. |
| `Session::locate` — `Path::strip_prefix` | sound: `Path` compares *components*, so `/a/repo2` is not under `/a/repo`. |
| `scan::walk` — `Path::strip_prefix` | sound, same reason. |
| `store::declarations`/`methods` — `c.root \|\| '/' \|\| f.path` | sound: builds absolute paths, never compares them. |
| `handlers::location` — `is_absolute()` then `root.join` | sound, and both branches are live: tree sites are absolute, `--refs` candidates are checkout-relative. |
| `/var` vs `/private/var` | sound: both sides canonicalized in `Session::locate` and `workspace_root`, and the store keys on git's own path. Verified on disk that `repo_root` and `checkout.root` agree byte-for-byte. |

Four bugs, all of the same shape: **a prefix or suffix test with no boundary.**

**Why not a newtype.** A `StoreAbsolute(String)` / `CheckoutRelative(String)`
pair would make the same-file bug unrepresentable, which is the standard this
project applied to the fabricated-path P1 (paths made absolute at the store so
the mistake could not be written). It was rejected here because the two kinds
meet in only a handful of places while the newtype would touch `store`, `tree`,
`resolve`, `serve` and `cli` — a wide refactor to catch a narrow class, in a
codebase where the *boundary* mistake, not the kind mistake, accounts for all
four findings. Helpers with honest tests catch all four; a newtype catches one.

**Reverses if** a fifth bug of this class appears, or a kind confusion appears
that a boundary check would not have caught — either says the helpers are not
carrying enough and the type system should.

## DEC-027 — A convention-based answer with competitors is `ambiguous`, not `resolved`

**Decided.** `Status` gains the third value PLAN §1 and CLAUDE.md always
promised. A `receiver_name` promotion reports **`resolved`** when the name is
the whole story — nothing else defines the method — and **`ambiguous`** when
other definitions could equally have been the answer. No threshold: the test is
simply whether a competitor exists.

**Why.** Session 16's rung promoted `@account.local?` to `resolved` with
confidence **0.03**, because thirty-one other classes define `local?`. That is
the failure this project's first principles name outright — nothing silently
promoted — arriving through a feature built to *stop* under-reporting. `status`
is the field a caller branches on; confidence is the field it often ignores. A
`resolved` carrying 0.03 invites exactly the trust the confidence is trying to
withhold.

The split falls where the evidence does. `@widget.supplier_region` stays
`resolved · 0.5`: `supplier_region` is defined once, so the name settles it.
`@account.local?` becomes `ambiguous · 0.03`: the name picked among equals.

Exit codes treat `ambiguous` as a match (0), because it *is* an answer — the
disclosure is in the status and the confidence, not in whether the command
failed.

**Also fixed here:** confidence was serialized at full float precision —
`0.03225806451612903` for one thirty-first. It is now rounded where it is
built, to the two figures two counts can support.

**Reverses if** callers turn out to treat `ambiguous` as a failure and stop
reading the candidates, which would make the honesty cost more than it buys —
the shape to watch is an agent that branches on `status == "resolved"` and
discards everything else.

### DEC-025 revisit (session 18) — **turned down again, on a measurement that names the real fix**

Authorized to reverse this after `--usage` showed the dominant operation's
observed median at 415 ms. Measured first, and the measurement moved the target
rather than the decision.

`--profile` now reports the tree build's phases. Warm, median of repeats:

| phase | rails | discourse |
| ----- | ----- | --------- |
| declarations (SQL) | 31 ms | 97 ms |
| ancestry (SQL) | 7 ms | 20 ms |
| **methods (SQL)** | **98 ms** | **218 ms** |
| assemble (namespace fixpoint) | 37 ms | 147 ms |
| **index-methods** | **137 ms** | **161 ms** |
| **total** | **310 ms** | **643 ms** |

Predicted the split as SQL 110 / assemble 110 / add-methods 80. The namespace
fixpoint is **three times cheaper** than predicted (37 ms) and the method work
much dearer: **methods are 235 ms of rails' 310 ms — 76 %** — 84,052 of them,
fetched and then materialized into `MethodDef`s and a `(owner, singleton, name)`
index.

**Why persistence still loses.** A persisted tree must still materialize those
84k methods and their index on load; that is `index-methods`, the larger half.
Persistence can only remove the SQL — 137 ms of 310 ms on rails, a **2.2×**
ceiling against the **2.5×** set in advance as the bar. It buys less than the
threshold while adding a second on-disk format, its own version, and a
concurrent-reindex race. Turned down again, and now for a *quantified* reason
rather than a comparative one.

**What the measurement names instead: load methods by name, on demand.**
Nothing needs 84k methods. A constant query needs none. A call query needs the
handful reachable from one receiver's chain, and residue needs one name's
candidates. Demand-loading by name addresses **both** expensive phases at once —
the 98 ms fetch and the 137 ms index — where persistence addresses only the
cheaper one. Ceiling: ~235 ms of rails' 310 ms, ~380 ms of discourse's 643 ms.

It is not free: `lookup` and `named` currently hand out `&MethodDef` borrowed
from the tree, which interior mutability cannot do, so they would return owned
values and every caller in `resolve/`, `refs/` and `serve/handlers` changes with
them. That is a session's work, specified and measured, not a guess.

**One cheap thing tried and reverted:** deferring only the `by_name` index (it
serves residue candidates alone) bought **3 ms of 137 ms**. The cost is
materializing the methods, not indexing their names. Keeping the `RefCell` for
1 % was not worth the complexity, so it went back.

**Reverses if** demand-loading lands and the remaining cold start still matters
— at which point what is left to persist is the *namespace*, which is small,
and the honest comparison can be made again.

### DEC-025 — demand-loading landed (session 19)

The design the last revisit named, built and measured. Warm-cache medians of
repeated runs; the checkout's methods are no longer fetched or indexed at build
time, only Ruby core's (752 rows, from the vendored stub, which no per-name
query could reach) and the `table_name` definitions.

| | before | after |
| --- | --- | --- |
| rails tree build | 310 ms | **73 ms** |
| discourse tree build | 643 ms | **259 ms** |
| methods materialized per build | 84,052 | **752** |
| rails `--def` wall clock | 0.31 s | **0.09 s** |
| discourse `--def` wall clock | 0.67 s | **0.30 s** |
| rails `--refs` wall clock | ~0.40 s | **0.13 s** |
| rails LSP first query | 508 ms | **85 ms** |
| discourse LSP first query | 975 ms | **272 ms** |

**Predictions, all four inside their range**: floor 75 ms / 264 ms (actual 73 /
259), rails `--def` ~95 ms (actual 90), discourse ~285 ms (actual 300), `--refs`
~150 ms (actual 130).

`--refs` gains the most proportionally, as predicted: it is dominated by *one*
name — the query's own — so it went from loading 84k methods to loading one
name's worth.

**Accuracy is unchanged.** The gold set on the no-Sorbet corpus is identical
before and after: app 54.8 % correct / 4.8 % wrong, gem 40 % / 3.6 %. A
performance change that moved an accuracy number would mean it had changed
semantics, and it did not.

**One caveat on how to read these.** A *cold OS page cache* costs a further
370 ms on rails and 620 ms on discourse — the first query in the first process
after the database has not been touched. That is disk, not trekr, and it is why
the LSP probe reads 460 ms on its first run and 85 ms on its second. The
numbers above are the warm ones, which is what a session that asks more than
one question sees.

**What is left, and what persistence would now cover.** The floor is
declarations SQL plus the namespace fixpoint: 31 + 7 + 33 ms on rails, 96 + 20 +
141 ms on discourse. Discourse's 141 ms `assemble` over 69k declarations is now
the largest single item, and it is a *shape* problem (a fixpoint over all
declarations), not a laziness one — the namespace cannot be demand-loaded the
way methods can, because `A::B` cannot be settled without knowing what `A` is.

So the reverses-if trail ends where the last revisit said it would: **if cold
start still matters, what remains to persist is the namespace** — 19,697 rows
for rails against the 84,052 methods that are now gone. That is a much smaller
thing to serialize, and a much better trade than the one turned down twice.

## DEC-028 — Two ranking features measured and **not** shipped

**Decided.** Ancestor-chain proximity and call-site/definition directory
affinity were built, measured against the gold set, and turned down. Neither
reached the bar set before running: **≥ 2.0 points on the #1 rate or ≥ 0.02
MRR**, on the gem sample.

| variant | truth ranked #1 | MRR |
| ------- | --------------- | --- |
| baseline | 61.5 % | 0.743 |
| + chain proximity | 61.5 % | 0.743 |
| + directory affinity | 63.1 % | 0.753 |
| both | 63.1 % | 0.753 |

**Chain proximity did nothing at all** — not a small gain, zero. Tier 0 ("the
enclosing class inherits from its owner") rarely holds more than one candidate,
so there is no order to improve. Predicted +1–2 points; the honest answer is
that the tier already captured everything the signal had.

**Directory affinity moved one site.** 1.6 points of a 65-site denominator is
40 → 41. Predicted +3–5 points. Reporting that as an improvement would be
reporting noise as signal, which is the exact failure the bar exists to prevent.

**The finding that matters is why there was no headroom.** The slice this
session was aimed at — 10.8 % of gem residue, recorded as `residue-ranked-out`
— was assumed to be truth that existed but sat past rank 8. It is not. Raising
the candidate cap from 8 to **500** did not shrink that bucket **by a single
site**: the true definition is not in the candidate pool at all. No ordering
can reach what is not there.

So the verdict was misnamed and is now `residue-truth-absent`. It is a second
kind of *coverage* gap, not a ranking gap: something with that name was found,
but the thing Ruby actually ran was not. Session 16 flagged that this bucket
could not distinguish those two cases; this settles it, in the direction that
invalidates three sessions of "sitting yield for ranking features".

**Reverses if** a corpus appears where the truth *is* in the pool and merely
ranked low — the cheap test is the one run here: raise the cap and see whether
the bucket moves. It costs one gold run and it should be the first thing done
before any future ranking work.

## DEC-029 — A gem position should resolve against a checkout that owns the gem

**Built in session 22.** Recorded below as first written; the settled design and
its measurement follow at the end of this entry.

**Decided in principle, not yet built.** When `--def` or the LSP is asked about
a position inside gem source, the checkout it resolves against is currently
*that gem's own directory* — which has no `Gemfile.lock`, and so a tree of one
gem plus Ruby core. Every method the gem gets from another gem is unreachable,
by construction rather than by any gap in extraction or lookup.

**Evidence.** `delegate` in actionpack's `metal.rb` answers residue with "the
receiver's type is known but nothing in its ancestors defines this name". Its
owner is `Module#delegate`, defined in activesupport. From the rails checkout
the same name resolves and finds 143 confirmed call sites; from the actionpack
gem directory it cannot.

**Why it is not built here.** The design question is *which* checkout owns a
gem: a machine may have several apps resolving the same version, and the answer
has to be picked, cached, and kept honest when it is wrong. That is a session's
work, not an afternoon's, and this session was asked to classify before
building.

**The measured ceiling.** 2,924 of the gold set's 2,987 sites are inside gem
files, and 37 % of them currently fail to name the true definition. An unknown
but large share of that is this. Re-measuring the gem floor after the fix is
the first thing session 22 should do, because it also tells us how much of
every gem number published since session 12 was this artifact.

**Reverses if** the pick turns out to be genuinely ambiguous in practice — two
apps resolving the same gem version with different bundles — in which case the
honest answer may be to resolve against the *union* of checkouts that resolve
it, or to require the caller to say which app it is asking from.

### DEC-029 settled (session 22)

**Ownership pick: most recently indexed app.** Several apps can resolve one gem
version, so the pick must be deterministic. Of the candidates — widest bundle,
first registrant, most recent — only the last *follows the work*: reindexing the
app you are in makes it the context, so a wrong pick self-heals through the
action a person was going to take anyway. Widest bundle is stabler and wrong
more often; first registrant is stablest and wrongest.

**Disclosure.** The answer carries `context`, naming the checkout whose
namespace answered, and `--explain` prints it. An answer that depends on which
app supplied the ancestors has to say which app, or the next person cannot tell
a good answer from a lucky one.

**Fallback.** A gem no indexed app resolves keeps the one-gem-plus-core tree and
names *itself* as the context. The degradation is the same as before; what is
new is that it is visible.

**Cache and invalidation.** The map is a `gem_use` table, rewritten wholesale
per checkout on every index — like the file map — so a gem dropped from a
`Gemfile.lock` stops being claimed. Rows die with their checkout by foreign key,
so `--drop` takes the ownership with it. There is no separate cache to go stale.

**Measured.** Gem-floor correct 38.2 % → 48.8 %, found-the-definition 64.0 % →
84.5 %, confidently wrong 3.8 % → 3.0 %. The artifact accounted for ~70 % of the
gem residue. Details and the correction note in `docs/BASELINE.md`.

**Residual staleness, stated.** The pick is a snapshot of "most recently
indexed", so it can name an app whose bundle has since changed on disk without
being reindexed. That is the ordinary staleness the whole store has — the
surface key catches content drift within a checkout, not a lockfile edit nobody
indexed — and the `context` field is what makes it diagnosable rather than
mysterious.

**Amended after 0.3.0: the editor's workspace comes first.** In the LSP, a gem
file opened from a workspace whose own app's bundle holds the gem is answered
from that app; the most-recently-indexed pick applies only when it does not.
The session caches the pick per directory, so with two apps sharing a gem,
indexing the other one once was enough to make the editor answer from it for
the rest of the session. The CLI keeps the store's pick: it has no workspace,
only a working directory, and nothing has shown that to be wrong yet.

## DEC-030 — `--gc` is dropped from the backlog, not deferred again

**Decided.** No garbage collection, and it comes off the list rather than
rolling a fifth time. DEC-003 already decided blobs are never collected; this
records why the follow-up that kept being scheduled should stop being.

**The hypothesis was measurable, and it is false here.** The driver was always
"edit-churn orphans accumulate" — blobs from edited-away file versions that no
checkout references any more. On this machine's database:

| | |
| --- | --- |
| database | 384 MB |
| checkouts | 642 |
| blobs | 37,171 |
| **blobs referenced by no file** | **0** |

Not "few". None. A dry-run reporting reclaimable bytes by category would print
zeros, and building it to print zeros is how a backlog rots.

**Why zero.** Two reasons, and only one of them lasts. Pre-1.0 the schema keeps
moving, and a version bump drops the database wholesale (DEC-009) — this
project has done that a dozen times, and each one is a total collection. The
durable reason is that indexing is keyed by blob and the corpora here are
re-indexed from clean checkouts, so few versions of a file ever exist.

**What would reopen it, stated so nobody has to re-derive it.** A machine where
the second row above is *not* zero: hundreds of engineers editing between
indexes, or a long-lived database that outlives several schema versions once
the schema settles. The check is the one query above, and it costs nothing to
re-run. Reopen on an observation, not on a hunch — that is what four rollovers
were trying to tell us.

**Reopened by DEC-049** on exactly that: the query was right and the unit was
wrong — an old gem version is never an orphan, it maps its own blobs.

### DEC-028 revisited (session 23) — one of the two ships, on the same bar

The features were re-measured against the candidate pool as it exists *after*
gem context (DEC-029), which is a third larger than the pool they were rejected
against. Same corpus, same seed, same sample, same bar: **≥ 2.0 points on the
#1 rate or ≥ 0.02 MRR**.

| variant | truth ranked #1 | MRR |
| ------- | --------------- | --- |
| baseline | 49.6 % | 0.648 |
| + chain proximity | 50.4 % | 0.652 |
| + directory affinity | **52.8 %** | **0.666** |
| both | 52.8 % | 0.666 |

**Directory affinity ships**: +3.2 points, clearing the bar it missed at +1.6
against the smaller pool. The feature did not change; the measurement did. That
is the whole lesson — it was rejected for a real reason, and the reason expired
when the pool it was measured against stopped being wrong.

**Chain proximity is rejected again**, and now for the second time on
independent data: +0.8 points, +0.004 MRR, and adding nothing on top of
affinity. Tier 0 rarely holds more than one candidate, so there is no order for
it to improve, and a bigger pool did not change that. The code is removed rather
than kept behind a flag — it measured zero twice.

**Ranking stayed in its lane**: `correct`, `wrong` and `found the definition`
are byte-identical with the signal on and off (52.2 % / 4.2 % / 84.0 %). Only
the order within the offered set moved, which is what a ranking feature is
allowed to do.

`TREKR_RANK_OFF=affinity` switches it off so the next person can re-size it
without a custom build, and testbed case 013 pins it — with the near definition
sorting *later* by path, so the case fails when the signal is off. An earlier
draft of that case put it first and passed either way.

### DEC-029, measurement vs product (session 24)

**The product pick stays "most recently indexed". The measurement pins its
context explicitly. They differ on purpose.**

The product wants the pick to *follow the work*: reindex the app you are in and
it becomes the context, so a wrong pick self-heals through an action you were
going to take anyway. That is the right behaviour for a person and the wrong
behaviour for an instrument — it makes the answer depend on when you last
indexed something unrelated.

Demonstrated rather than argued. Reindexing the five corpora in **reverse
order**, so a different app owns the shared gems:

| | pinned to the app the corpus was traced from | unpinned |
| --- | --- | --- |
| gem correct | **48.8 %** | 52.2 % |
| gem found | **84.5 %** | 84.0 % |
| gem confidently wrong | **3.0 %** | 4.2 % |

The pinned column is identical to the run before the reindex, to the decimal,
and to a second consecutive run. The unpinned column reproduces session 22's
52.2 % exactly — so that figure was never wrong, it was *a different question*:
what the gem floor looks like answered from rails rather than from the small app
the gold set was traced in.

**Canonical gem figures, from here on: 48.8 % correct, 84.5 % found, 3.0 %
confidently wrong**, pinned to `widget_shop-nosorbet` — the app whose bundle the
TracePoint run actually executed. Answering those sites from rails scores better
partly because rails *is* the gems' own source tree, which flatters the number
for a reason that has nothing to do with the engine.

`--def --context CHECKOUT` is the pin, and it is a real affordance rather than a
test hook: "answer this gem position as if I were working in that app" is a
question an agent can legitimately ask.

**Rule for future gem numbers.** Two consecutive full runs on an untouched store
must agree to the decimal, and the context must be stated. A gem figure quoted
without its context is one draw.

## DEC-031 — A lexical record of a deferred effect is worse than no record

**Decided.** The extractor emits an ancestry edge only for a mixin written
**directly in a class or module body**. `include`/`extend`/`prepend` inside a
`def` is recorded as the ordinary call it is and nothing more. Conversely,
`class_methods do … end` now opens the concern's `ClassMethods`, because that
block's effect is *not* deferred: `ActiveSupport::Concern` creates the module at
load time either way.

**Why they are one decision.** Both are the same question — *when does this line
take effect, and against what?* — answered in opposite directions, and getting
either wrong costs more than the missing fact would.

A mixin inside a method runs when the method runs, against whatever `self` is
then. Rails writes `include ActiveModel::Validations` inside
`has_secure_password`, in a `ClassMethods` body; recorded lexically, that one
line put the module's instance methods into the class-level lookup chain of
**every ActiveRecord model**, where `alias_method :validate, :valid?` beat the
real `ClassMethods#validate`. An invented edge is worse than a missing one
because it *wins*: a missing edge yields a ranked residue, an invented one
yields a confident wrong answer. Seven of discourse's eight confidently-wrong
app sites were that shape.

`class_methods do` is the mirror image. Session 13 recorded its methods without
the module and pinned the behaviour as deliberate. What that cost was not
visible until the declined receivers were classified: discourse's
`Service::Base` writes `class_methods do include StepsHelpers end`, so the
entire DSL surface of 224 service objects — `step`, `model`, `policy`, `params`
— sat on the concern as instance methods, unreachable from a class body.
**396 of 1,401 declined app sites, 28.3 %, one shape.**

**Measured**, discourse app code, 498 sites, context pinned:

| | baseline | + mixin rule | + `class_methods` |
| --- | --- | --- | --- |
| correct | 42.0 % | 43.4 % | **59.2 %** |
| found the definition | 82.5 % | 84.9 % | 84.5 % |
| confidently wrong | 1.6 % | **0.2 %** | 0.6 % |
| residue, truth offered | 40.6 % | 41.6 % | **25.3 %** |

Both arms predicted before running and both inside their ranges. Each arm was
also checked site by site against the one before, because a schema bump forces a
store rebuild between arms and a corpus total cannot tell a fix from a store
difference (session 23).

**The cost, stated.** Confidently wrong rose 0.2 → 0.6 % on two sites, both a
call inside `StepsHelpers` where the `includer` rung now chooses among five
includers instead of one and promotes at confidence 0.2. That is DEC-027's rule
— a convention-based pick among competitors is `ambiguous`, not `resolved` —
never having been applied to that rung. Recorded rather than fixed here.

**Reverses if** a corpus appears where a method-body mixin is the only thing
naming a real ancestor and its absence costs more than the invented edges did —
the shape to watch is a plugin system that installs modules from a loop. The
`class_methods` half reverses only if a non-Concern `class_methods do` is found
in the wild, which the no-arguments-and-a-block guard already declines.

## DEC-032 — `workspaceSymbol` is not denormalised; the number that justified it was cold

**Decided.** No `def_search` table, no checkout root carried below the file map.
`workspaceSymbol` keeps the three-table join and the leading-wildcard `LIKE`.

**Why, measured.** Session 24 recorded that a rare symbol costs 1.15 s while a
capped common one costs 0.10 s, and named a schema change as the honest remedy.
Re-measured with each query run **first in a fresh process**, the ordering is
what mattered: `%each%` — the common one — costs **0.78 s** in the first slot,
and every query after it costs ~0.10 s whether it matches 200 rows, 93, or none.
Session 24 put `%Widget%` first and read a cold cache as selectivity.

Prototyped on a copy of the store anyway, because the write-side cost was the
question asked: 601,623 rows against 509,151 (**1.18×**, and 1.37× on a store
with more checkouts sharing blobs), **+22 %** database, ~0.17 s added to a 2.9 s
discourse index, and a warm query of 0.036–0.045 s against 0.10 s. **A 60 ms win,
not a 1.1 s one.**

It is also not implementable as phrased. `def` is keyed by blob; ARCHITECTURE's
layer-1 rule forbids a path or checkout below `blob` precisely so that N
worktrees of one repo cost one index. Carrying a root means one row per
(definition, checkout) — the blow-up factor above *is* the sharing being given
up, and it grows with the case the design exists for.

**Reverses if** substring search becomes a hot path with a warm-cache budget it
misses. The instrument then is FTS5 or a trigram index over the **168,718
distinct names** — a third of the rows and no second home for a path — not a
denormalised copy of every definition.

### DEC-027 extended to the includer rung (session 26)

The rule — *a convention-based answer with competitors is `ambiguous`, not
`resolved`* — was written for the receiver-name rung and applied only there. The
`includer` rung, which answers a call inside a module by asking the classes that
mix it in, reported `resolved` however many includers disagreed.

`class_methods do` (DEC-031) is what exposed it: widening every concern's
includer set turned one candidate into five, and the rung promoted at confidence
**0.2** on two discourse sites. Same fix, same reasoning: `ambiguous` when the
includers disagree about where the name is defined, with the definitions they
offered listed as candidates.

The **scorer** gained the matching split in the same change, and that is the
part worth stating. `confidently wrong` has always meant *resolved, and pointed
elsewhere*; an `ambiguous` answer that points at the wrong site was being
counted in it. Reporting them apart is the session-16 discipline — ask what
distinct realities land in a bucket — not a softened metric: both numbers are
published, and the `ambiguous` one is only smaller because the engine already
said it was unsure.

Measured, discourse: app confidently wrong 0.6 % → **0.2 %** with 0.4 %
ambiguous-wrong beside it; gem floor 4.0 % → **3.3 %** with 0.7 % beside it.
`correct` and `found the definition` identical to the decimal on both columns.

## DEC-033 — `define_model_callbacks` is built, measured, and **not** shipped

**Decided.** ActiveRecord's model callbacks — `after_save`, `before_create`,
`after_destroy` and kin — stay unmodelled. The macro entry, the `only:` filter
and the `included do` routing were written, measured against runtime truth, and
reverted.

**Why it looked right.** 114 declined app sites on discourse, the truth **never
named** on any of them (offered 0, ranked first 0; 17 offering nothing at all),
and a mechanism trekr already models for `belongs_to` and `enum`: a macro whose
literal arguments name the methods it creates. `define_model_callbacks :save,
:create, :update, :destroy` at activerecord/callbacks.rb:416 states every one of
them.

The design worked. `included do` is `class_eval`'d into the includer, so a
class-level macro written there defines methods on **every includer's
singleton** — the same destination Concern gives `ClassMethods`, and
`ActiveRecord::Callbacks` has a real `ClassMethods`, so routing them there is a
restatement rather than an invention. `after_update` in a discourse model went
from nothing to `resolved · confidence 1`.

**Why it is not shipped.** The definition's honest location is the macro call,
in **activerecord**/callbacks.rb:416. Ruby runs
**activemodel**/callbacks.rb:144, inside `_define_after_model_callback`. A
different file, so every one of those answers points somewhere Ruby did not go.

Measured over **all 114 sites**, not a sample:

| | before | after |
| --- | ---: | ---: |
| confidently wrong | 0 | **112** |
| residue (declined, truth not named) | 114 | 2 |

On the pinned 498-site sample the four predictions recorded before the code was
written all landed exactly: `residue-nothing-known` 3.2 % → **3.0 %**, `correct`
**62.4 %** unchanged, `found` **87.8 %** unchanged, **confidently wrong 0.2 % →
0.8 %** against a bar of **≤ 0.4 %** set in advance.

**So the trade is 112 declines converted into 112 confident answers that do not
point at the running code**, to recover one site of `residue-nothing-known`.
That is the exact trade this product exists not to make: PLAN §1's whole
argument is that a wrong go-to-definition costs an agent a file read and a retry
with nothing in the answer to warn it.

**The scorer was not changed to make this pass**, and the temptation is worth
recording because it was real. `is_generated()` already excuses exactly this
shape for `belongs_to` — runtime truth at the generator, trekr at the macro —
and its `GENERATOR_FILE` list is an enumeration rather than a principle. But its
`declaration` verdict also requires trekr's answer to be **inside the app**, a
guard added in session 15 to fix a scorer artifact, and relaxing it for a gem
answer would let genuine gem-side errors through. Widening a bucket so that a
change scores well is the failure this project has already corrected twice from
the other direction.

**Reverses if the engine learns to say which kind of answer it is giving.** The
gap is not in the extraction, it is in the disclosure: a `def` row already
carries `via = 'define_model_callbacks'`, and the answer does not surface it, so
a caller cannot tell a macro *declaration* from the line that runs. If `--def`
reported that — a `declaration` flag, or `resolved_via` carrying the macro —
then this answer is a feature rather than an error, the scorer can read trekr's
own disclosure instead of a hardcoded regex, and the app-side generated bucket
stops needing one too. **Do that first, then re-measure this.** It is the
largest remaining idea in this arc and it is a product change, not an extraction
one.

**Also learned, and worth a line.** A store newer than the binary is a hard
refusal — *"database is schema v16 but this trekr speaks v15"* — not a silent
drop. That is right, and it means reverting an extractor change requires
dropping the database by hand. The database is a cache (DEC-009), so that costs
one reindex and nothing else.

## DEC-034 — An answer says which kind of location it is

**Decided.** `MethodAnswer` carries `kind: definition | declaration`, and
`defined_via` names the macro when it is a declaration. Residue candidates carry
their own `kind`. On the LSP side it lives in **hover**.

**The field shape, since it is public API.** Three candidates were weighed:

* `defined_via` alone, with "is it a declaration" implied by the field's
  presence. Rejected: the thing a caller branches on should not be inferred
  from an absence.
* `declaration: true|false`. Rejected: a boolean cannot grow a third case, and
  there may well be one (a `.rbi` declaration is arguably neither).
* **`kind` plus `defined_via`.** Chosen. `kind` is the branch and the most
  guessable name; `defined_via` is the detail, because "declaration" alone tells
  a reader what the answer is *not* without telling them what it is.

`kind` sits beside `sites[].kind`, which is class/module/method/constant. That
was the one real objection and it is tolerated rather than dodged: they are
different questions at different nesting levels — one about a *location's*
nature, one about a *symbol's* — and every place that documents one documents
the other. A longer unambiguous name (`definition_kind`) was the alternative,
and guessability won.

**The discriminator is "is the body at this location", not "was a macro
involved"** — which the data insisted on. `module_function` clones a real `def`
and points at its line; `define_method`'s block *is* the body, and session 28
measured those as `correct` against runtime truth. Both are definitions. A macro
that generates methods elsewhere, an alias whose body is another method's, and a
bare `private :foo` that asserts only visibility are declarations.

**Why it is worth API surface.** Session 15 invented a `declaration` verdict in
the *scorer* because trekr's macro answers were neither right nor wrong in the
usual sense, and it identified them with an allowlist of three Rails files plus
a guard that the answer be inside the app. Both were proxies for a question only
the engine could answer. The scorer now reads trekr's own word, corroborated by
a gold-side check that the truth is not a written `def`; `GENERATED_OWNER`,
`GENERATOR_FILE`, `in_app` and `checkout_root` are gone. Swapping them moved one
site on each column — both from `residue-truth-absent` to `declaration-offered`,
declarations the allowlist could not see, one of them in a gem where `in_app`
had structurally forbidden the verdict.

**Reverses if** a caller is found branching on `kind` in a way that wants a
third value, which would mean the definition/declaration line is drawn in the
wrong place. The shape to watch is `.rbi`: today a Sorbet stub answers
`definition` because its `via` is empty, and it is a declaration in every sense
except the one this field measures.

### DEC-033 reversed on disclosure (session 30)

`define_model_callbacks` is modelled and shipped. Nothing about the extraction
changed — it is the code session 29 wrote, re-applied as this entry recorded it.
What changed is that the answers now say `kind: declaration · define_model_callbacks`,
so the same 114 sites that scored **112 wrong** score **112 declaration, 0
confidently wrong**.

The measurement that turned it down was correct at the time and correct now: it
was measuring a real defect, in the *disclosure* rather than in the extraction.
Recording the rejection with its numbers is what made the reversal a re-run
rather than a rediscovery.

One thing the corpus could not show and the testbed did: routing into
`ClassMethods` requires that module to exist, and `ActiveRecord::Callbacks`
declares one. A concern that only writes `included do define_model_callbacks`
does not, so the module is now emitted when we route into it. No corpus change;
case 018 fails without it.

### DEC-034 revisited (session 31) — the third case arrived and did not need a third value

The entry rejected a boolean partly because "there may well be a third case (a
`.rbi` declaration is arguably neither)". That case arrived, and the answer is
that an `.rbi` stub is a **declaration** by the discriminator already stated: a
bodiless `def` is not a body, wherever it sits.

`defined_via: rbi` carries the detail the way a macro's name does, so callers
written against session 30 keep working and the branch stays binary. A distinct
`stub` value was weighed and rejected: it would have split the branch on a
question — *was the method caused here or merely described here* — that changes
nothing a caller does, since the action for both is "do not look for a body
here".

The reverses-if narrows rather than closes: a third value is warranted only if a
caller is found that must act differently on a description than on a generator.

**Measured**, on the one corpus that commits `sorbet/rbi/`: **0 of 63 app sites,
9 of 400 gem sites**. Rare by design rather than by luck — DEC-019 makes real
source win the whole chain before a stub wins any of it, so a stub answers only
when it is all there is, which is also what makes an `rbi` answer worth
reacting to: *the implementation is not indexed.*

### DEC-022 revisited (session 35) — the gap is a schema *format*, not a convention

Session 34 left 134 sites where trekr should have answered from `db/schema.rb`
and did not, and guessed at DEC-022's own named gaps: plugin-added columns, or a
`self.table_name` override. Hand-checked, it is neither.

**Discourse has no `db/schema.rb`.** It sets
`config.active_record.schema_format = :sql` and keeps `db/structure.sql`, where
`users.name` and `users.staged` are declared exactly as expected. DEC-022 reads
one file and that app does not have it — so every attribute method in the app is
unmodelled, for a reason that has nothing to do with conventions.

That format is not exotic: any Rails app using database features the Ruby DSL
cannot express keeps `structure.sql`, which is most Postgres apps of size.

**Sized, over the whole declined population** (270 sites whose owner is a
`GeneratedAttributeMethods` module):

| | sites | what a reader would do |
| --- | ---: | --- |
| plain attribute, receiver typed | **91** | resolve outright |
| plain attribute, receiver untyped | 129 | offer a candidate; ranking, not resolution |
| dirty-tracking (`_changed?`, `will_save_change_to_…`) | 50 | **nothing** — DEC-022 excludes these on purpose |

**Not taken this session, and the reason is honest**: 91 resolutions is the
largest tractable slice three sessions of classification have turned up, but it
needs a *new input format* — SQL, not Ruby — which the blob layer has no path
for. `extract()` takes bytes with no path, so content-sniffing would be the
fragile way in; the clean way is a reader beside the gem pass, keyed on
`db/structure.sql`. That is a session's work, not an afternoon's, and it is
specified here so the next one does not re-derive it.

**Reverses if** the cost estimate is wrong once someone opens the file: the
grammar in `structure.sql` that matters is `CREATE TABLE [public.]<name> (…)`
with one column per line, which is far simpler than the Ruby DSL already parsed.

## DEC-038 — Dead-code candidates are tiered by what was checked, never asserted

**Decided.** `trekr --dead <path>…` reports **candidates for deletion or
inlining**, graded, each carrying what was looked for and what was found. The
word "dead" never appears in an answer, and nothing is ever `dead: true`.

**Why the vocabulary matters more than the algorithm.** The tool cannot know
that a method is unreachable — `send`, a name built at runtime, and a call from
an ERB template that this engine does not index are all invisible, and they are
stated limits rather than bugs. What it *can* say precisely is: *I looked for
these five kinds of evidence, across the whole index, and found none.* That is
useful, and it is true. "Dead" would not be.

### The scope is the argument; the evidence is the whole index

`--dead app/models/user.rb app/services/` takes files or directories. Every
definition **in scope** is classified by evidence gathered **everywhere** — all
634 checkouts, gems included — because a method used by one caller outside the
scope is not a candidate, and a scope-local search would say it is.

*Amended by DEC-074:* the evidence is the whole **checkout**, not the whole
index. Only the name-count pre-filter ever read other checkouts, and it made
the answer depend on what else was indexed.

### The tiers

| tier | means |
| --- | --- |
| `unreferenced` | no confirmed, no possible, no symbol reference, anywhere |
| `convention-only` | reached **only** by a symbol handed to a macro |
| `single-caller` | exactly one confirmed or possible reference — Daniel's inlining candidate |
| `referenced` | not reported |

**`convention-only` is its own tier rather than folded into referenced**, because
it is the shape most likely to be a genuine deletion candidate *and* the shape
most likely to be a false positive — `after_create :thing` is real use, while a
same-named symbol somewhere in a gem is coincidence. Splitting it lets a caller
decide; blending it would hide the decision.

### Confidence is graded per candidate, never flat

The house rule (DEC-008, DEC-011): a score traces to what backs it. A candidate
in a file that also contains `send`, `public_send`, `method_missing`, or an
interpolated constant carries **lower** confidence, and says which of those it
saw. A schema-generated attribute is not reported at all — an unreferenced
column is a fact about the database, not about dead code.

### Validation, before anyone deletes anything

**Git history as ground truth**, per DEC-037: run against an old discourse
commit and score candidates against what humans actually deleted since.
Precision at the moment the tool would have spoken, needing no test suite and no
judgement call. Predicted precision on `unreferenced` is **60–75 %**; the bar to
call it more than disclosure is **≥ 70 %**.

### Shaped for a consumer, not a reader

Daniel's `reaper` workflow buckets references by kind, rates deletion cost, and
stages revertible commits. This is the **evidence layer** such a workflow
consumes: per-symbol, machine-readable, reasons attached, `--json` first-class.
It deliberately stops short of recommending a deletion, because the cost of
being wrong is borne by whoever runs the delete, and they need the reasons to
weigh it.

### Measured, session 36 — the bar was failed, and the tool ships anyway as disclosure

Predicted 60–75 % precision on `unreferenced`; measured **19.8 %** against a
**19.0 % base rate** for any method in the same scope. A lift of **1.04×**,
which is no signal. The reverses-if above is therefore in force: `--dead` ships,
makes no precision claim, and its tiers are advisory.

The prediction was wrong toward overclaiming, which is the direction that costs
trust, and it would have read as a weak-but-real result had the base-rate
control not been run. It cost one command. **Any future precision claim in this
project needs its control measured in the same breath.**

`convention-only` came out strong in the opposite direction — 3.3 % deleted,
0.17× the base rate, so those methods survive nearly six times more often than
average. It is finding genuinely-used code that looks dead, which validates the
symbol-reference prerequisite on its own terms.

**What would move `unreferenced`**: the references it cannot see. ERB templates
and the spec suite hold them, and until those are indexed, "no references found"
in a Rails app says as much about trekr's inputs as about the user's code.

## DEC-035 — Freshness is a probe and a budget, not a daemon

**Decided (design; `not_indexed` shipped, the rest specified).** A query never
blocks on an index and never spawns one. Three layers:

1. **An O(1) probe** decides whether the checkout *might* have moved.
2. **A bounded, query-biased refresh** inside the query when it has, prioritising
   the file being asked about.
3. **Disclosure** — `not_indexed`, and `coverage: warming` — instead of waiting.

### What the measurements forced

Daniel's 10M-line monorepo indexes cold in **3 minutes** and re-indexes with
nothing changed in **6 seconds**. Our corpora, no-op, steady state:

| | files | total | scan | known-diff | store-write |
| --- | ---: | ---: | ---: | ---: | ---: |
| rails | 3,307 | 294 ms | 117 ms | 77 ms | 78 ms |
| discourse | 11,301 | **185 ms** | 135 ms | 5 ms | 42 ms |
| the monorepo | ~30× discourse | **6 s** | most of it | — | trailing |

**Extrapolating discourse's 185 ms by file count predicts ~5.5 s at 30×, and
the observed number is 6 s.** The no-op cost is very nearly linear in worktree
size, which is the calibration worth keeping: for this shape of work, linear
extrapolation from discourse is trustworthy to about 10 %.

**So folding a scan into the query path is off the table at target scale**, and
that is the whole reason the probe exists. 185 ms is already too slow to pay per
query; 6 s is not a policy, it is an outage.

**Where the scan time actually goes**, measured on discourse (24,447 tracked
files):

| git call | time |
| --- | ---: |
| `ls-files -s -z` (tracked + OIDs) | **10–30 ms** |
| `diff-files --name-only -z` | 20 ms |
| **`ls-files -o --exclude-standard -z`** (untracked) | **300 ms** |

Untracked-file discovery is ~90 % of it, and on a no-op it by construction finds
nothing. It cannot be made cheap — it is a full worktree walk that has to honour
`.gitignore` — so it belongs in an explicit `--index`, not in a query.

**This is *not* rq's D5 disease**, which was checked before assuming: trekr's
scan is three `git` invocations total (`scan/mod.rs:121–129`), not a glob per
extension. The 6 s is git walking a very large worktree, not trekr asking git
the same question repeatedly.

### The probe

`stat(.git/index)` — mtime and size — against what the store recorded, plus
`HEAD`. Measured on discourse: **~0 ms for the stat** on a 2.3 MB index, ~5 ms
for `rev-parse` (a subprocess, and the floor is the fork). Both are O(1) in
repo size, which is the property that matters.

**What the probe cannot see, stated rather than discovered later:** a tracked
file edited in the working tree with nothing having refreshed the git index, and
a brand-new untracked file. Git touches `.git/index` on many ordinary operations
(`status`, `diff`, `add`, `checkout`), so the gap is narrower in practice than in
theory — but it is real, and the answer is disclosure plus an explicit
`--index`, never a claim of freshness we cannot back.

### Lifted from rq, and what was left

rq solved this first (`~/code/lib/rust/rq/src/index/mod.rs`):

* **Taken — budgeted, query-biased refresh** (`index_budgeted`, `index/mod.rs:113`):
  files relevant to the current query are refreshed first and ignore the budget;
  the rest stream within a time slice; coverage is marked `complete` vs
  `warming`. It is the shape that makes freshness free at the point of use, and
  trekr's disclosure vocabulary already has room for `warming`.
* **Taken — the idea that the budget should track observed cost**
  (`cli/mod.rs:2883`). At 6 s scans a fixed budget is meaningless.
* **Adapted — mtime.** rq trusts mtime alone. trekr can afford better:
  **mtime as filter, blob hash as truth**. A moved mtime with identical bytes
  costs one hash, not a parse — and trekr is content-addressed, so an unchanged
  hash means the facts are already there.
* **Rejected — mtime in the fact layer.** rq's unit is a file; trekr's is a blob
  in a store shared across checkouts. Per-file mtime is *checkout-side
  bookkeeping* and must live with the file map, never below `blob`, or the
  sharing that makes N worktrees cost one index is lost (ARCHITECTURE layer 1).

### Why no background process

The obvious design — kick an index and return — was rejected, and it is worth
saying why, because it is the first thing anyone proposes.

trekr is **daemon-free by first principle** (CLAUDE.md; PLAN §202: *state on
disk, any process can answer*), and there is today **no detached work anywhere
in `src/`**. A background indexer would introduce: a second writer racing
SQLite's single write lock, orphaned processes outliving the CLI that spawned
them, and the question of who owns an index that nobody asked for. The
budget-and-probe design gets the same user-visible property — freshness without
waiting — with none of that, because the work happens *inside* a query that was
going to run anyway.

The one thing it does not give is a cold 3-minute index happening on its own.
That stays explicit, and `not_indexed` is how it asks: the root, the command,
exit 2.

### The no-op write, removed — and a prediction to check at scale

The file map was rewritten wholesale on every index (`DELETE` + one `INSERT` per
file), so a repeat run paid O(files) to write back what was already there. A
`map_key` over (path, blob oid) now gates it.

| no-op, steady state | before | after |
| --- | ---: | ---: |
| rails — store-write | 78 ms | **0 ms** |
| discourse — store-write | 42 ms | **1 ms** |
| discourse — total | 185 ms | **145 ms** |

**The prediction, for the next `--index --profile` on the 10M-line monorepo.**
Two claims, and the first is the sharp one because it assumes nothing about that
repo's phase split:

1. **`store-write` reads ≤ 10 ms on a no-op**, whatever the file count. The
   phase is now O(1): one keyed read and a comparison. If it reads in hundreds
   of milliseconds, the fold is being defeated by something — most likely paths
   that differ run to run — and that is the thing to look at.
2. **Total no-op falls from 6 s to roughly 4.6 s**, if that repo's phases are
   proportioned like discourse's (where store-write was 23 % of the total). The
   honest range is **4.6–5.9 s**: the less of its 6 s was store-write, the less
   this moves. What remains is scan, and ~94 % of *that* is git's untracked-file
   discovery, which no key can skip.

Claim 1 is the one worth reading first: it is falsifiable on its own and does
not depend on how the earlier 6 s was distributed.

### Built, session 33

The probe and the query-biased refresh are in on `--def`. One stat of
`.git/index` decides; on a hit the queried file alone is re-read and re-parsed
(only when the blob is new), both checkout keys are updated incrementally, and
the answer carries `index: {stale, refreshed, hint}`.

**The bound is structural rather than a time budget**, which is the one place
this departs from rq. rq spends a slice of milliseconds and streams as many
files as fit; trekr refreshes *exactly one* — the file the question is about.
At 6-second-scan scale a time budget mostly buys a partially-refreshed index
whose coverage nobody can describe, whereas "the file you asked about is
current, everything else is disclosed as lagging" is a sentence an agent can act
on. The streaming remainder is still available if a corpus ever shows it paying.

Sampling order matters and is easy to get backwards: the fingerprint is taken
**before** the scan. Afterwards, it would cover edits the index never saw and
the next query would call them fresh. Taken first, the worst case is a probe
that reports stale when it is not — one wasted re-read, never a wrong answer.

Only `--index` moves the recorded fingerprint. A refreshed query deliberately
leaves it, so the next query still says `stale: true`: one file being current
is not the checkout being current, and the disclosure should keep saying so.

### Shipped now

`not_indexed`, because it is the half that needs no policy: `--def`, `--refs`
and `--ancestors` on an unindexed checkout say so and exit 2, instead of
answering `residue` and reading as a finding about the code.

**Reverses if** the probe's blind spot turns out to bite in practice — the shape
to watch is an editor-driven workflow where files change constantly and nothing
touches `.git/index`. The answer then is a filesystem watcher in the LSP front,
which is a resident process that already exists and may legitimately watch,
rather than a daemon for the CLI.

## DEC-036 — The CLI forgives a hand-typed position; the LSP does not

**Decided.** `--def` snaps to the nearest name on the line when the exact
column holds none, and discloses the snap. `FILE:LINE` is a valid spec. The
serve front keeps exact-position semantics.

**Why the two surfaces differ.** They have different callers. An editor sends
the column the cursor is actually on, and a server that quietly answered about a
*different* name would fight what the protocol promises — hover and
goToDefinition are expected to describe the thing under the cursor, and an
editor has no way to show "actually, I answered about the token to your right".
A person typing `--def app/models/user.rb:42:11` into a terminal is
estimating, and was being told "no name at this position" for being one
character out.

**The rules, and what each is protecting.**

* **Snap only when the exact position holds nothing**, so an exact hit can never
  be reinterpreted.
* **Bounded to the line.** Snapping across lines answers a different question
  than the one asked, and a column is evidence about *where on the line*, not
  about which line.
* **Nearest by column, leftmost on a tie.** With column 0 (`FILE:LINE`) that
  reduces to "the first name on the line".
* **Always disclosed** — `snapped_to: {name, col, alternatives}` in JSON, one
  stderr line in text. The alternatives carry their columns so the next query
  can be exact. This is the house rule: nothing silently promoted, and an answer
  about a name the caller did not type is exactly that.

**The ranking is free, and that is the part worth noticing.** "Which name did
they mean" needed no heuristic, because the fact model only records *interesting*
names: `w = Widget.new` has `Widget` and `new` in it and no `w`, so nearest-first
lands on the constant without anyone having to rank locals below constants.

**Rejected: making `FILE:LINE` list names and exit non-zero.** It is honest and
it is a dead end — an agent gets a refusal it must parse and re-issue. Answering
the first name and disclosing the rest gives the same information and an answer.

**Reverses if** a snap is measured picking the wrong name often enough to
mislead — the tell is `snapped_to` appearing in answers whose top result is then
ignored. The fix would be ranking by kind rather than distance, which the fact
model already supports.

### Item recorded, not built: the bare-argument grammar

`trekr <input>` dispatching on shape — `FILE:LINE:COL` → `--def`,
`Owner#method` → a symbol card, a bare constant → definition plus ancestors —
is designed and deferred. Two notes for whoever builds it:

* **The boundary with rq is the point.** "Where is this name defined", across
  languages, stays rq. trekr's bare-name answer is the Ruby-rich card —
  definition site, `kind`, reference tier counts — not a second rq. The skill
  should say so at the same time the grammar ships, or agents will reach for the
  wrong tool and conclude one of them is broken.
* **Flags stay the explicit form.** The grammar is sugar over them, so every
  shape it dispatches to must remain reachable by flag; scripts should never be
  made to depend on shape inference.

## DEC-037 — Dead-code candidates: the design, and the fact that is missing first

**Design only. Nothing built, and one prerequisite named that must come first.**

Rubydex ships a dead-code endpoint and its weakness is convention-invoked false
positives — a method reached by a name the source never spells as a call.
trekr has the pieces that weakness needs (receiver-tiered references with
reasons, gem context, macro modelling, and a disclosure vocabulary), and one
piece it does not have, which turns out to be the whole problem.

### The measurement that sizes it

Discourse defines **29,325 distinct method names**. **9,054 of them (31 %) never
appear as a call-site name anywhere in the index** — across all 634 checkouts,
gems included. That is the candidate pool *before* any receiver reasoning, and
nobody believes a third of discourse is dead. The gap between 31 % and the truth
is exactly the false-positive surface, so the design is about that gap.

### The missing fact: a symbol argument is an invocation

```ruby
after_create :ensure_in_trust_level_group      # line 185
def ensure_in_trust_level_group                # line 2076
```

`ensure_in_trust_level_group` has **zero call sites**. trekr records
`after_create` as a call and expands what the macro *defines*, and it does not
record that the macro's symbol argument *invokes a method by name*. Every Rails
callback, validation, `delegate`, and route target is in this shape.

**This is the prerequisite.** A dead-code answer built before it would be
reporting a defect in our fact model as a finding about the user's code — the
exact failure DEC-021 caught for `--refs` exclusions, one layer up. The fact is
cheap and useful on its own: it improves `--refs` for every callback-registered
method whether or not dead code is ever built.

The shape: a new reference kind, `symbol_ref`, emitted when a symbol argument
sits in a macro known to take a method name. The list is the same table
`macros::generated` already curates, read in the other direction — which is why
this is an extension of something owned rather than a new mechanism.

### What the tiers would mean, and what they would not

Given that fact, a candidate is a definition with **zero confirmed, zero
possible, and zero symbol references**. Read strictly:

* **`confirmed` = 0** is strong. The receiver resolved and lookup landed
  elsewhere, so those call sites provably are not this method.
* **`possible` = 0** is the load-bearing one and the weakest. It means no
  same-named call site survived, and DEC-021 already established that
  `no_such_method` is the weakest exclusion reason. A candidate resting mostly
  on that is a candidate to disclose, not to assert.
* **The stated limits are all false-positive sources**: `send`/`public_send`,
  a name built at runtime (`"#{setting}_validator".camelize`), and — the one
  that would bite hardest — **anything called only from a view template**, which
  trekr does not index at all. ERB is not Ruby to this engine.

So the output is **tiered candidates with the reason each survived**, never a
`dead: true`. Something like `unreferenced` (nothing at all),
`convention-only` (reached solely by symbol), and `dynamic-risk` (the file or
class shows `send`, `method_missing`, or interpolated dispatch nearby).

### How it would be validated, before anyone deletes anything

Two independent checks, and the first is the good one:

1. **Git history as ground truth.** Methods that humans later deleted are known
   dead; methods still present years later are known live. Run the analyzer
   against an old commit of discourse and score its candidates against what the
   next N months of history actually removed. This needs no test suite, no
   deletion, and no judgement call — and it measures *precision at the time the
   tool would have spoken*.
2. **Delete-and-run-tests on discourse**, as a spot check on a sample. Slower,
   noisier (its suite is large and needs the full environment), and only
   confirms what the tests cover — which is precisely the code most likely to
   have references the analyzer already saw.

**Predicted precision, recorded before building.** On candidates that survive
all three counts *and* the symbol-reference fact: **60–75 %** against the
git-history check, with the residue dominated by view-template callers and
`send`. Without the symbol fact I would predict **under 30 %**, which is the
number that says build the prerequisite first.

**Bar for shipping**: ≥ 70 % precision on the history check for the strictest
tier, and every candidate carrying its reason. Below that it ships as
`--candidates` disclosure and never with the word "dead" attached.

**Reverses if** the history check turns out to be confounded — a method deleted
because its whole feature was removed is not evidence the analyzer would have
been right about it in isolation. The mitigation is to score only deletions that
left the surrounding file alive.

## DEC-039 — The LSP front indexes in a child process, and refreshes on save

**Decided.** `trekr --lsp` keeps the index current itself, three ways, and
never waits for any of them:

1. **didSave** refreshes the saved file in place — `Store::refresh_file`, the
   same bounded one-file refresh DEC-035 gave the CLI, minus the probe (a save
   *is* the signal).
2. **didChangeWatchedFiles** (registered dynamically when the client offers it)
   refreshes up to 32 changed Ruby files the same way. More than that, or any
   deletion, is an operation on the checkout — a branch switch, a pull — and
   gets a full index, because `refresh_file` can add or replace a file's facts
   but cannot remove them.
3. **An unindexed checkout** — the workspace root when it has a `Gemfile`, or
   any checkout a question lands in — gets a background `trekr --index` child,
   reported as `$/progress` when the client can show it. Answers meanwhile
   come from core and gems, and `hover` says the checkout is not indexed.

**Why a child process, when DEC-035 rejected background work for the CLI.**
DEC-035's objection was to *detached* work with no owner: a second writer, an
orphan, an index nobody asked for. Here the owner is the editor session that
asked a question about the checkout, the work is an ordinary `--index` run
that exits when done, and it writes through SQLite's locking exactly as a CLI
invocation would. DEC-035 itself named the LSP front as the process that "may
legitimately watch". What stays true: no daemon, no lockfile, no state the
server owns — the child's result lives in the store, and the server's tree is
rebuilt from it on the next question because the checkout's surface key moved.

**Rejected: indexing in-process on a thread.** It would need the index
pipeline (`cli/`) exposed as a library API and a second store connection held
by the server, for no user-visible gain over a child — and a child that is
killed with the editor still leaves the store consistent, since `--index`
writes in one transaction.

**Rejected: killing the child on shutdown.** A cold index of the design-point
monorepo is minutes; a short editor or agent session killing it each time
would never finish one. The child runs to completion and exits.

**Also decided: the root's tree is built when the server is idle** — after
`initialize`, and after each background index — instead of on the first
question. The first question after spawn was paying the 300–470 ms build.

**Reverses if** concurrent writers are measured contending — a child index and
a burst of saves both writing — enough to stall saves visibly. The answer would
be queueing saves behind a running index rather than dropping the child.

## DEC-040 — Completion is built, reversing PLAN §1 for completion alone

**Decided (2026-09-26, at Daniel's direction).** `--lsp` answers
`textDocument/completion`. PLAN §1 listed completion under "what not to build"
because the only client then was Claude Code's `LSP` tool, whose nine
operations do not include it. The surface is now an **editor** as well: trekr
is meant to replace Ruby LSP and Sorbet in VS Code, and an editor language
server without completion is not one people keep enabled. Formatting, rename,
semantic tokens and type checking stay on the list.

**What it is.** Receiver-aware and ranked, the same engine as `--def`:

| context | what is offered, in rank order |
|---|---|
| `recv.` (typed by the ladder) | the type's methods, own first, then each ancestor in Ruby's lookup order; private only when the receiver is `self` |
| `recv.` (untyped) | nothing until a prefix is typed; then at most 20 same-prefix names, most-defined first, each labelled "receiver type unknown", list marked incomplete |
| `Scope::` | constants declared in that namespace and its ancestors |
| bare word | locals and parameters, then the enclosing class's methods up its chain, then constants from the innermost lexical scope outward |

Operators and setters are not offered after a dot, nothing is offered in a
comment, string or symbol, and a list cut at 300 items is marked incomplete so
the client asks again as the prefix narrows.

**The mid-edit problem, and the trick that solves it.** The buffer rarely parses
at the cursor — `w.` is a syntax error. The word being typed is replaced with a
placeholder identifier before parsing (`w.trekr_completion_placeholder`), which
the extractor records as a call with its receiver, nesting and singleton-ness:
exactly the input `resolve::receiver_type` needs. No second parser, no
completion-specific inference.

**Cost, measured** over the stdio audit (25 files × 4 positions per corpus,
completion right after the dot and after two typed characters):

| | p50 | p90 | max |
|---|---:|---:|---:|
| rails, after the dot | 0.2 ms | 0.5 ms | 0.9 ms |
| rails, two characters typed | 3.4 ms | 3.7 ms | 4.1 ms |
| discourse, after the dot | 0.3 ms | 1.4 ms | 4.2 ms |
| discourse, two characters typed | 6.9 ms | 15 ms | 17 ms |

The price is the **whole method table**, which demand-loading (DEC-025) exists
to avoid on the lookup path: listing needs every method, not one name. It is
loaded once per tree — 220 ms on rails, 520 ms on discourse — while the server
is idle after the tree itself is warm, so no keystroke pays it in the common
case. Two additive APIs carry it: `Tree::method_table` and `Tree::declared`
(tree/), and `resolve::receiver_type` (resolve/).

**How often the right name is offered** after two typed characters: 18 of 33
sampled rails call sites and 37 of 42 discourse. The misses are receivers the ladder cannot
type — the same ceiling as `--def` (ARCHITECTURE: rails resolves ~40 % of
method call sites) — where the short guess list did not contain the name.

**Rejected: word-based completion for untyped receivers.** Every name in the
index matching the prefix is what Ruby LSP's fallback amounts to and what the
editor's own word completion already provides; ranking it as though it were
knowledge is the flood this engine exists to avoid.

**Reverses if** the member listing's memory or build time is measured hurting
the design-point monorepo — then it becomes a store query per owner instead of
a whole-table load.

**Amended after 0.3.0: an `ambiguous` receiver lists as incomplete.** A chain
typed by name (DEC-077) is a guess, and completion listed it as the whole
answer: `x.strip.` offered String's methods with `isIncomplete: false` though
a `strip` in the app declares nothing. Any receiver the ladder calls
`ambiguous` now marks the list incomplete, so the client asks again as the
prefix narrows. The guess is still listed rather than dropped: `x.to_s.` is
ambiguous in every real app (`NilClass#to_s` alone declares nothing), and
dropping it would remove chain completion outright.

## DEC-041 — A bundle's gems are written in one transaction

**Decided.** `--index` writes the checkout in its own transaction, as before,
and then every newly-indexed gem inside **one** transaction (`Store::batch`).
Each `write` is a savepoint, so it is still atomic on its own and still works
outside a batch.

**Why, measured.** DEC-014 found the store write was 85 % of a cold index and
named batching as where to start. With gems it is worse than that, and not for
the reason it looks: a cold discourse index spent **10.1 s** in store-write for
2.8 M rows (280k rows/s), while discourse alone, one transaction, writes at
750k rows/s. The difference is 297 commits. A commit writes every page the
transaction dirtied into the WAL, the `name` indexes are keyed in effectively
random order, so each gem — ~36 files on average — dirtied pages across most of
`call_site_name` and paid to write them all again; and a WAL growing that fast
checkpoints constantly, each with an `fsync`.

Discourse, fresh database per run, five interleaved rounds, medians:

| | wall | store-write | CPU | peak RSS |
| --- | ---: | ---: | ---: | ---: |
| one transaction per gem | 12.7 s | 10.1 s | 12.2 s | 421 MB |
| **one per bundle** | **7.6 s** | **5.3 s** | 10.8 s | 444 MB |

Rails with its gems: 2.7 s → 1.6 s. Discourse with `--no-gems` does not move
(3.6 vs 3.8 s), which is the control: it was always one transaction. The two
databases' logical contents — every fact keyed by blob OID, every file map,
every `gem_use` row — hash identically.

**Tried and not taken.** A larger `wal_autocheckpoint` bought most of the same
win (100k pages: 11.3 → 7.8 s) by attacking the checkpoints rather than the
commits, at the price of a WAL that can grow to 400 MB mid-index — a knob
tuned to one machine, where batching removes the cause. A 256 MB page cache
changed nothing (11.3 vs 11.7 s): the pages were not being evicted, they were
being committed. Turning off foreign-key checks saved ~4 % and would have
removed a check for a gain inside the noise.

**The cost.** An index interrupted while writing gems keeps none of them, where
it kept the ones already finished. Content addressing makes that a re-parse,
not a loss, and the checkout's own map is committed before any gem is read.
The write lock is held for the whole gem phase — ~5 s cold on discourse, the
same order as the 3 s the checkout's own single write already held it.

**Reverses if** a gem phase long enough to matter appears — the shape would be
a first index of a very large bundle being interrupted routinely. Then commit
in groups sized by rows written, not per gem.

## DEC-042 — Statistics are regathered when the store outgrows them, not after every write

**Decided.** `--index` runs `ANALYZE` only when `blob` or `checkout` has grown
more than a tenth past the row count `sqlite_stat1` recorded at the last
analysis. The check is two `MAX(id)` reads and two `sqlite_stat1` rows.

**Why, measured.** Session 24 made `ANALYZE` run after any index that parsed
something, because `PRAGMA optimize` alone let statistics go 13 rows stale
across 633 checkouts (DEC-006's argument, applied). That fixed the accumulation
and put a full `ANALYZE` — every index of every table read end to end — behind
a one-file edit. Re-indexing a discourse checkout after appending one line to
one file, 313 MB store, eight interleaved rounds, medians:

| | wall | of which `analyze` |
| --- | ---: | ---: |
| analyze after any parse | 643 ms | 383 ms |
| **analyze when outgrown** | **258 ms** | 0 ms |

A single earlier run on a busier machine read 1.9 s of `analyze` in a 2.3 s
reindex, and the doc comment it replaces quoted ~3 s on a 384 MB store: the
cost scales with the store, not with the edit, which is the problem.

**Why a tenth, and why cumulative.** Statistics steer the planner by orders of
magnitude — DEC-006's bad plan was 90 s against 45 ms, from statistics that
were *absent*, not 10 % off. And the comparison is against the count at the last
analysis, not the last index, so the session-24 trap — many small increments,
each under a threshold — still adds up to a regather. `checkout` is counted as
well as `blob` because that trap was checkouts accumulating, and a new worktree
adds a checkout without adding a blob.

**Reverses if** a plan is found that goes wrong inside a tenth of growth. Then
the threshold is the wrong instrument, and the query should be pinned (DEC-006's
own reverses-if).

## DEC-043 — The scan asks `git status`, so git's untracked cache can answer

**Decided.** `scan` finds changed and untracked files with one
`git --no-optional-locks status --porcelain -z --untracked-files=normal
--no-renames --ignore-submodules=all`, instead of `git diff-files` plus
`git ls-files -o --exclude-standard`. A wholly untracked directory, which
`normal` collapses to `dir/`, is listed with `ls-files -o -- dir/`, which walks
only that directory.

**Why, measured.** DEC-035 found untracked-file discovery was ~90 % of a no-op
scan and called it irreducible — "a full worktree walk that has to honour
`.gitignore`". That is true of `ls-files -o`, which never consults git's
untracked cache. `status` does: the cache records each directory's mtime and
skips the readdir of any that has not changed. On discourse with the cache on,
the untracked walk is 13–17 ms inside a 41–51 ms `status`, against 96–100 ms
for `ls-files -o` alone.

No-op `--index`, eleven interleaved rounds, medians:

| | before | after |
| --- | ---: | ---: |
| discourse, cache on (as configured here) | 165 ms | **86 ms** |
| rails, cache on | 64 ms | **41 ms** |
| discourse, cache forced off | 166 ms | 158 ms |
| rails, cache forced off | 64 ms | 59 ms |

With the cache off `status` does the same walk, and the second process saved
is the small win left. The file map is identical — the no-op's `store-write`
stays at 1 ms because the map key matched.

**What `status` reports that `diff-files` did not**: staged-only changes. They
are rehashed like the rest and hash to the OID the index already gave them, so
the map cannot differ; it costs one read per staged file. `status` also diffs
the index against `HEAD`, about 5 ms on 24k tracked files.

**The lock is load-bearing.** `status` refreshes and rewrites `.git/index` when
it can, and `.git/index` is exactly what the freshness probe watches (DEC-035)
— a scan that rewrote it would make the next query's probe lie.
`--no-optional-locks` stops the write; an e2e test fails without it.

**What trekr does not do** is turn the cache on. It is the user's repository
and the user's config, and `-c core.untrackedCache=true` on a read-only call
adds nothing: the cache lives in the index, which the call is forbidden to
write. The changelog tells a monorepo user to enable it.

**Unmeasured, and the number worth having next**: the 10M-line monorepo whose
6 s no-op started DEC-035. If its untracked walk is ~94 % of that and its cache
is on, the prediction is a no-op dominated by `ls-files -s` and the
`HEAD` diff — well under 2 s. If it reads near 6 s, check
`git config core.untrackedCache` before anything else.

**Reverses if** a git version or configuration is found where `status`
reports a different set of changed files than `diff-files` would — the e2e
test pins edits, untracked directories and ignores, not every porcelain state.

## DEC-044 — Completion's member listing is built off the request thread

**Decided.** The idle warm-up's second step — listing every member of the
root checkout for completion (DEC-040) — runs on a worker thread. The worker
opens its own connection, assembles its own tree, lists it, and sends back the
tree and the listing; the session installs both if the checkout's stamp has
not moved meanwhile. Warm steps now run back to back while the inbox is quiet,
rather than one per incoming message. `Tree` swapped `Rc<Ancestry>` for `Arc`
to be `Send`; nothing else about it changed.

**Why, measured.** Listing loads every method in the checkout: ~0.5 s on
discourse (176k methods across the app and its gems). It ran on the serve loop,
and because the loop only woke for a message, it ran *just after* the first
answer — so the second request of a session, whatever it was, waited for it.
`trekr --lsp` on discourse, a scripted client, four alternating runs each:

| | before | after |
| --- | ---: | ---: |
| idle, then ask: second request (`hover`) | 495–560 ms | **13–24 ms** |
| idle, then ask: first `references` | 138–170 ms | 149–167 ms |
| ask at once: `documentSymbol` (needs no tree) | 576–622 ms | **25–32 ms** |
| ask at once: first `completion` | 4–5 ms | 580–605 ms |

The stall moved to the one request that needs the listing, when it is asked for
before the listing exists; every other request stopped paying for it.

**Why hand back the worker's tree.** The alternative kept the session's own
tree and dropped the worker's. Its first `references` rose to ~340 ms in half
the runs, because the session's tree then loaded each method name on demand
where the listed tree already had them all — and RSS did not fall, since the
allocator keeps the worker's freed pages (450 vs 448 MB).

**Memory, measured alongside** (RSS, one session): 123 MB with discourse's
tree; ~450 MB once its members are listed; +70 MB for references; +7 MB for a
second checkout's (rails') tree; +30–110 MB for its members; +4 MB for 300 open
documents. Completion's listing is the bulk. Slimming the listing to what an
item shows (name, privacy, macro, `.rbi`) took the session from 678 to 635 MB.
What would move it further is not loading every method into the tree to list
them — a streaming listing from the rows — at the cost of the per-name loads
above. Not done: recorded as the lever.

**Reverses if** a client is found that asks for completion before anything
else, often enough that the 0.6 s moving onto that request is the common case.
Then the listing should start at `initialize` rather than after the tree.

## DEC-045 — Completion's listing streams method rows; the tree stays demand-loaded

**Decided.** `Members::of` walks every method through `Tree::each_method`,
which visits the names the tree already holds and streams the rest from the
store one row at a time — owner resolved, listed, dropped — instead of loading
all of them into the tree first (`method_table`, removed). The worker that
lists (DEC-044) now hands back only the listing and drops its tree on its own
thread; the session keeps its own tree. `Tree` is back to `Rc<Ancestry>`: it
is no longer sent anywhere.

**Why, measured.** DEC-044 recorded the listing as the bulk of an LSP
session's memory and named this lever, with a warning: `references` would then
pay per-name method loads, which it had measured at up to +200 ms on the first
call. `trekr --lsp` on discourse, scripted client, alternating runs:

| | before | after |
| --- | ---: | ---: |
| RSS, discourse tree + members | 451–453 MB | **294–295 MB** |
| live heap at that point (`heap`) | 289 MB | **117 MB** |
| RSS, whole session (+ rails, 300 docs) | 629–640 MB | **359 MB** |
| ask at once: first `completion` | 581–591 ms | **370–383 ms** |
| idle, then ask: first `references` | 134–143 ms | 143–149 ms |
| 40 fresh `references`, total | 9.4 s | 8.5 s |

The feared regression is ~10 ms, not 200. The first `references` on
`Topic#category` loads 12 names (~30 ms of SQL) — the per-name cost was never
the 200 ms. What DEC-044 measured was the other variant it tried, keeping the
session's tree while the listed one carried every method. Tried here as well:
handing the worker's tree back, as DEC-044 does, made the first `references`
161–242 ms against 140–145 ms for keeping the session's, and noisier —
consistent with dropping a 120 MB tree on the request thread. So the session
keeps its tree.

**Made checkable first.** Each owner's members came out in hash-map order, so
a list cut at 300 items held a different 300 names in each process: two
sessions of the same binary disagreed on 477 of 944 LSP answers. They are now
sorted by name (stable, so a name's definitions keep lookup order). With that,
the differential is exact: 944 LSP requests (completion, hover, definition,
references) and 1,455 CLI queries byte-identical before and after this change.

**What is left.** Most of the RSS above the tree after listing is the
worker's freed tree, which the allocator keeps for reuse: the live heap with
the tree and the listing is 117 MB against 294 MB of RSS. Rails' listing on
top costs 6 MB where it cost 105.

**Reverses if** a request is found that walks many method names on a fresh
tree — the shape would be a first `references` or `incomingCalls` that reads
hundreds of names. Then preload the names a scan will ask for in one query,
rather than bring back the whole table.

## DEC-046 — An index reads the known blobs once, not once per gem

**Decided.** `--index` loads the set of known blob OIDs once (`Store::blob_oids`)
and adds each write's OIDs to it, instead of calling `known()` — a full scan
of `blob` — for the checkout and again for every gem. A single-file refresh
(`--def`'s DEC-035 refresh, the LSP's save) asks `Store::has_blob`, one probe
of the OID index, where it too scanned the whole table for one OID.

**Why, measured.** A cold discourse index calls it 298 times against a table
growing to 22k rows: `known-diff` was 420 ms of the cold index. Discourse with
its gems, fresh database per run, five interleaved rounds, medians:

| | wall | known-diff |
| --- | ---: | ---: |
| scan per gem | 7.3 s | 420 ms |
| **once per index** | **6.8 s** | 0 ms |

A one-file reindex (183 vs 181 ms) and a no-op do not move: they pay the one
scan either way. The two databases' logical contents hash identically.

**Correctness is the in-memory set's to keep.** Gems do share bytes — two
versions of a gem, a vendored copy. The set has to learn each write's OIDs,
or the second gem re-parses the shared file and `INSERT OR REPLACE` gives the
blob a new id under the first gem's map. An e2e test indexes two gems sharing
a file and fails without that step.

**Rejected: probing per OID for the whole index.** It scales with the files
asked about rather than the store, which is right for one file and wrong for
a no-op at scale: ~100k probes against one scan.

## DEC-047 — The parse streams into the write

**Decided.** `index_files` parses on the pool into a bounded channel (256
files) while the calling thread writes, inside the same savepoint, whatever
has arrived. `Store::write` takes an iterator of facts rather than a `Vec`.
Nothing else about the write changed; rows land in completion order, which was
already arbitrary (the parse fanned out over a hash map).

**Why, measured.** DEC-014 found the write was most of an index and that trekr,
unlike rq, parsed everything before writing anything. The time that ordering
cost turned out small; the memory did not. Fresh database per run, seven
interleaved rounds, medians:

| | wall | peak RSS |
| --- | ---: | ---: |
| discourse + gems, parse then write | 7.8 s | 444 MB |
| **discourse + gems, overlapped** | **7.5 s** | **159 MB** |
| rails + gems, parse then write | 1.73 s | 216 MB |
| **rails + gems, overlapped** | **1.63 s** | **99 MB** |

The wall gain is ~5 %, not the ~0.7 s of parse it could hide: with the two
overlapped the write itself runs slower (parse + write went from 0.7 + 5.2 s
to a combined ~5.6 s), consistent with eight parse workers competing with the
writer for the machine. The memory is the result. Holding every fact of a
checkout before writing it grows with the checkout — ~0.3 GB for discourse,
so on the order of 10 GB for the 30× monorepo DEC-035 measured — and the
channel bounds it by 256 files. Both databases' logical contents hash
identically.

**The profile changed meaning.** `parse` is now wall time until the last file
is parsed, which includes waiting on the writer, and `store-write` is what the
write took after that; they still sum to the whole. Parse speed is reported
from the files' own parse times, per worker.

**Rejected: a larger or unbounded channel.** It trades back the memory for
nothing — the writer is the bottleneck either way, so the queue is always
full.

## DEC-048 — The file map is written as a delta (DEC-035 revisited)

**Decided.** When the map key says the map moved, `Store::write` reads the
stored map once (path, blob, surface), diffs it against the scan, and writes
only what moved: an upsert per new or edited path, a delete per vanished one.
The surface key is folded over the final map exactly as before.

**Why now.** DEC-035 kept the wholesale rewrite — "a delta would have to be
right about deletes and renames to save a few milliseconds". The milliseconds
grew: with the scan (DEC-043) and `ANALYZE` (DEC-042) cheaper, rewriting
discourse's 11k rows was ~95 ms of a ~180 ms one-file reindex. The scratch
discourse clone, one line appended to one file per run, twelve interleaved
rounds:

| | wall median | p90 | store-write |
| --- | ---: | ---: | ---: |
| wholesale rewrite | 188 ms | 202 ms | 95 ms |
| **delta** | **154 ms** | **166 ms** | **62 ms** |

Of the 62 ms left, timed inside the write: the edited blob's facts ~20 ms,
reading the map ~7 ms, the diff ~3 ms, and the commit ~43 ms — which is
the next lever, and looks like WAL checkpointing rather than anything the
map does.

**The correctness DEC-035 worried about is tested, not argued.** A unit test
writes a map, then one with a deleted file, an edited one, a rename and an
addition, and requires the result to equal a fresh write of the second map,
surface key included; it fails without the delete. On discourse, an edit, a
deletion, a rename and a new file indexed by each build into copies of the
same store leave logical contents that hash identically. A first index reads
an empty map and writes every row, as before; a branch switch touching every
file costs about what the rewrite did.

## DEC-049 — Checkouts nothing can reach are collected (DEC-003 and DEC-030 revisited)

**Decided.** `trekr --gc [--dry-run] [--older-than AGE] [--vacuum]` removes
every checkout a future index could not reach, unless an index saw it within
`AGE` (default 7 days), and then the blobs no remaining checkout maps. Reach is
decided per `checkout.kind`: a **repo** is reachable while its root is on disk;
a **gem** while it is on disk *and* a surviving repo's bundle names it
(`gem_use`). It is explicit — `--index` never sweeps.

**Why DEC-030's zero was the wrong question.** It counted blobs referenced by no
file, and there are still none. But a gem version every project has moved past
is not an orphan: it maps its own blobs, so it is kept forever. The same holds
for a deleted worktree's file map. Measured on this machine's store (schema v21,
16 days old, a `.backup` copy):

| | |
| --- | ---: |
| database | 446 MB |
| checkouts | 684 (676 gems, 8 repos) |
| gem names with more than one version | 125 of 505 (171 extra versions) |
| gems no current lockfile names, or gone from disk | 32 |
| gems no surviving repo's last index resolved | 47 |
| repos whose root is gone (deleted agent worktrees, a scratch dir) | 3 of 8 |
| **collectable checkouts** (this rule) | **69** |
| blobs only they map | 1,091 of 38,036 (2.9 %) |
| fact rows in those blobs | 289k of 4.8M (6.0 %) |
| pages freed | 23.5 MB |
| after `VACUUM` | 399 MB (−47 MB; 20 MB of that is fragmentation a `VACUUM` alone recovers) |

Most gem versions with siblings are live — different projects pin different
versions — which is why "delete the older version" would be wrong and the rule
reads the bundles instead.

**Last seen is `indexed_at`, not a new column.** A repo's no-op index already
moves it, so it already meant "an index last vouched for this". A gem now gets
the same: every index that names it moves its `indexed_at`, one indexed
`UPDATE` per gem inside the gem batch (DEC-041), so no query pays anything,
and a no-op index barely does: rails with its gems, ten interleaved rounds,
162 ms median before and 140 ms after — noise. A second timestamp would equal
the first for every repo.

**`kind` is a column because disk cannot say it.** The first shape inferred it
— a repo is a directory with `.git` — and bundler's git gems
(`bundler/gems/<name>-<sha>/`) carry a `.git` of their own, so every stale
revision of a git gem, the kind that churns most, would have been kept as a
live repo. It is stamped where a bundle names the checkout. The column costs a
schema bump (DEC-009: the store is rebuilt once, cold).

**Why 7 days, and why a window at all.** The window is hysteresis, not the
criterion — a checkout that is reachable is never collected however old, which
matters because on the measured store every repo was last indexed 15–31 days
ago. It exists so a lockfile flipped by a branch switch and flipped back does
not re-parse; a week covers a branch parked over a weekend. Being wrong is
cheap in both directions: a gem collected too early is re-read by the next
index that names it, and one kept too long is ~340 KB (the average here). 30
days would have collected 20 of the 69 here, because the store had not been
written for 15 days — a window that long mostly measures how recently someone
last ran `--index`.

**Why not a sweep inside `--index`.** 6 % of fact rows after 16 days of heavy
churn (Ruby upgrades, agent worktrees) is worth a command, not a stall: the
collection took 2.3 s on this store, and `--index` is the LSP's refresh path.
DEC-003 already said the fix is an explicit `--gc`. A pre-1.0 schema bump
collects everything anyway (DEC-009).

**Why `--vacuum` is opt-in.** A delete returns pages to SQLite's free list, which
later indexes reuse, so the store stops growing without it. Shrinking the file
is a `VACUUM` — 5.6 s here, holding the write lock — plus a WAL checkpoint so
the bytes actually leave the disk.

**The dry run is the real run, rolled back**, so the size it reports is
measured, not estimated; on this store it cost 0.9 s.

**Rebuild is on demand, and tested.** `--index` indexes any gem the lockfile
names that the store lacks (`has_checkout`), so collecting a gem a project still
wants costs one re-parse. An e2e test collects a gem version, switches the
lockfile back, and requires the next index to re-read it and answer into it.

**Reverses if** a third checkout kind arrives whose reachability is neither
"on disk" nor "named by a bundle" — then `kind` grows a rule, not a special
case — or if a store is observed where the collectable share is large enough
(say a quarter) that waiting for someone to run `--gc` is the problem; then a
bounded sweep during `--index`, off the LSP path, is the next step.

## DEC-050 — A replaced binary takes over the LSP session in place

**Decided.** `trekr --lsp` watches the file it was launched as. When that
changes, the server waits for a moment with nothing read and unanswered and no
background index running. It asks the new binary whether it can resume, writes
a handoff file, flushes its output, and `exec`s the new binary with the same
argv plus `TREKR_LSP_RESUME=<handoff>`. The pid and the stdio pipes survive
exec, so the client's connection carries on. The new process reads and deletes
the handoff, skips the `initialize` handshake the client will not repeat, and
keeps serving. This replaces retiring (exit, and let the client restart),
which stays only as a fallback.

**What crosses** (handoff format 1, JSON, `0600`, `create_new` in the temp
dir):

- `initialize`'s params, verbatim. Root, client capabilities,
  `initializationOptions` and the client's path spelling all derive from them.
- The registration ids the client holds (`trekr-watch`). Registering again
  would duplicate it.
- The editor's buffers: path, version and full text, since unsaved text is on
  no disk.
- Bytes read off stdin that are not yet a whole message.

**What does not.** Trees, completion listings and the disk-read cache are
rebuilt by the idle warm-up. Progress tokens cannot be open, because the swap
waits for the `--index` child to finish: the successor does not know the child
and could never send its `end`. Diagnostics are not republished; the client
keeps the ones it has until the next edit.

**Why the wire had to move.** lsp-server reads stdin on a thread, through
std's buffered `Stdin`, and parks each parsed message in a rendezvous `send`.
Bytes taken off the pipe could sit where the loop cannot see them, and exec
would destroy them. `serve/wire.rs` reads the raw descriptor with `poll` on the
loop's own thread, so every byte read is in a buffer the handoff can carry.
Output keeps a writer thread with a flush barrier. A side effect: with no
reader thread there is nothing to join, so leaving no longer needs
`process::exit` to dodge the Linux read that `close` does not wake.

**Detect: the launch path, stamped by inode** (contour's `launch_path` /
`stamp_of`). This is argv[0], resolved on `PATH` when bare, which is how an
editor launches `trekr`. It is stat'd through symlinks for (dev, inode, size,
mtime, mode). `current_exe()` would miss `brew upgrade` on Linux, where it
resolves through the relinked symlink to the old Cellar file. The inode catches
a rename-over even when an APFS clone keeps the old mtime. The mode means a
`chmod +x` on a refused file counts as a change. The stamp is checked at every
quiet moment, and every 2 s when idle. A stat through brew's symlink costs
~4 µs (100k stats, measured), against ~0.6 ms for a warm request, so checking
often costs nothing worth rate-limiting.

**Probe before exec.** If exec succeeds into a binary that then dies, the
connection dies with it, and nothing can bring it back. So the candidate is run
first as `--lsp` with `TREKR_LSP_PROBE=1`, stdin closed and logging off. It
answers `{"handoff": N, "version": …}`. That gives three outcomes:

| candidate | action | logged |
| --- | --- | --- |
| reads format `N` = ours | exec | `reload`, then `resume` from the new process |
| runs but cannot resume: a different `N`, or no answer while `--version` succeeds (a build from before this) | retire: exit 0 so the client starts the new build | `retire` |
| does not run: not executable, missing, exits with an error, or no answer in 5 s | keep serving; this stamp is not probed again, and the next change to the file is | `reload_failed` |

`ETXTBSY`, a file still open for writing, means an install is in progress, so
it is retried rather than settled. An exec that fails after a good probe (the
file replaced again in between) deletes the handoff and keeps serving.

Probe cost on the release build: 20–40 ms warm. The first run of a freshly
written binary takes 0.66 s, because macOS validates the signature on first
exec. It is paid once per upgrade, at a quiet moment; a request arriving
during it waits.

**Who restarts a retired server.** vscode-languageclient's default error
handler restarts a server whose connection closes, up to five times in three
minutes (read in 8.1.0). A client without that is no worse off than it was
before this decision: the user restarts the server from the editor. If a
resumed process cannot read its handoff despite the probe, it exits non-zero
for the same reason. The client will not send `initialize` again, so there is
no session left to start.

**Store VERSION.** A new binary with a new schema VERSION drops the index when
it opens the store (DEC-009; `--gc` moved 21 → 22). The resumed server is then
a cold start that still has its session. A root with a Gemfile is indexed in
the background with progress, and answers are partial until that finishes. The
e2e test deletes the database under a running server to stand in for this.

**Not chosen.**

- **Retiring only** (the design before this). It fixed staleness at a cost:
  the warmed tree and listing (0.2–0.5 s on a large app), a restart counted
  against the client's crash budget, and the whole server in any client that
  does not restart one.
- **A proxy parent that owns the pipes and respawns a child** (contour's
  DEC-025 idea). That is an extra process in every session to handle an event
  that happens once per upgrade. Exec gets the same continuity with no extra
  process.
- **Carrying queued messages.** Not needed, because the swap waits for a quiet
  inbox. Only a partial frame can be pending, and it crosses as bytes.

**Reverses if** a client is found that notices the swap: one that tracks the
server's executable, or treats an unchanged pid with new behaviour as an error.
It would also reverse if a stdio transport appears whose descriptors do not
survive exec.

## DEC-051 — SQLite reads through `mmap` (1 GiB)

**Decided.** Every connection sets `PRAGMA mmap_size=1073741824`. The store is
read from the shared page cache in place instead of being copied page by page
into each connection's own cache.

**Why, measured.** An earlier pass tried it on the tree build alone, saw ~5 %
(206 → 196 ms), and turned it down over the SIGBUS risk without writing it
down. rq adopted the same pragma (its D8) and accepted that risk, so the two
tools disagreed with no recorded reason. Re-measured here on the paths where
mapped reads are likelier to pay, discourse and rails in a 363 MB store.

CLI, warm page cache, nine interleaved rounds, medians, same binary with the
pragma on or off:

| | off | on |
| --- | ---: | ---: |
| `--ancestors Topic` (tree build) | 320 ms | 308 ms |
| `--refs Topic#title` | 560 ms | 542 ms |
| `--refs save` (rails) | 329 ms | 322 ms |
| `--refs ActiveRecord::Persistence#save` | 328 ms | 320 ms |
| `--dead app/services` | 1000 ms | 963 ms |
| `--def` a call in `topic.rb` | 330 ms | 317 ms |

A consistent 2–4 %, output identical. Cold — each run on an APFS clone of the
database (`cp -c`), a new file whose pages are not cached, since `purge` needs
root — it is mixed: tree build and `--def` flat, discourse `--refs` −4 %,
rails `--refs` **+8 %** (926 → 1001 ms), `--dead` +1 %.

**The memory is the reason.** `trekr --lsp` on discourse after a warm-up,
definition, ten `references` and six completions, live heap from `heap -s`:

| | live heap per server |
| --- | ---: |
| off | 168 MB |
| 256 MiB cap (rq's) | 108 MB |
| **1 GiB** | **90 MB** |

Each connection kept up to 32 MB of pages it had read, and a session holds
two per checkout (its own and its tree's loader), plus the listing worker's
while it runs.
Mapped, those pages are the kernel's page cache, shared by every process on
the store — which is what makes this worth more with several servers open on
one machine. The cap is set to cover a store this size; SQLite clamps it at
its own compile-time maximum (2 GiB), and a larger store maps its first part.

**How to read memory here.** RSS is the wrong number twice over: it counts
pages the allocator has freed and handed back with `MADV_FREE` (496 of a
session's 673 MB), and with mmap on it counts the shared, clean file pages
(RSS rises 660 → 850 MB while private memory falls). `footprint`'s
`phys_footprint` excludes both, but it swings by ±240 MB between runs of one
build with how much freed memory the allocator has not yet returned (171 vs
410 MB, same build). The live heap is the stable measure, and the one quoted.

**The risk, and why it is taken.** An I/O error under a mapping arrives as
SIGBUS rather than `SQLITE_IOERR`. The store is a local cache of a pure
function (DEC-013); on a local disk that error is a failing disk, where the
process was not going to give a useful answer anyway. The case that
shrinks a mapped file is `--gc --vacuum` (DEC-049), and it was tried
directly: two LSP servers reading discourse through the mapping answered 140
requests while another process collected and vacuumed the store from 307 to
284 MB — no errors, no crash, every answer unchanged. SQLite coordinates the
truncation through its own locking, as it does for ordinary reads.

**Reverses if** the store is put on a network filesystem, or the cold rails
`--refs` regression is found to generalise — the thing to watch is a first
query after a reboot.

## DEC-052 — Hover reads the doc comment when asked, and says a guess in words

**Decided.** A hover shows the definition's signature as written, the first
paragraph of its doc comment, and a linked `Defined in path:line` (with the gem
named, for gem code). The doc is read from the definition's file at hover time,
not extracted at index time. `status`, `confidence` and `resolved_via` are no
longer printed in the hover. When the answer is a guess, the hover says so in a
sentence. `--json` is unchanged.

**Why the internals left the hover.** Users read `status: Resolved ·
confidence: 1.00 · via local:new` as noise, and on a confident answer it tells
a person nothing. The principle that every answer carries its disclosure (see
CLAUDE.md) is about what a *caller* can branch on, and `--json`/`--ndjson`
still carry all three fields. A hover is read by a person. That person needs to
know one thing: whether this might not be the method that runs. So a
confident answer carries no caveat. A guess carries one, in words and never as
a number:

| answer | the hover says |
|---|---|
| resolved, certain | nothing extra |
| resolved, assignments disagree | "The receiver's type is inferred from assignments that do not all agree." |
| ambiguous | "Best guess — the receiver's type is inferred, and N other definitions of `x` exist." |
| residue, receiver unknown | "receiver type unknown — N possible definitions: `A#x`, `B#x`, `C#x`, N more" |
| residue, type known | "`T` has no `x` in anything trekr has indexed; it may come from a gem, a DSL, or `method_missing`." |

A residue shows no signature and no doc. Showing the top candidate's would
make a guess look like an answer, which is the one failure this engine exists
to avoid.

**Why read when asked, not indexed.** The first design stored a bounded
summary per `def` row as a blob fact, with a VERSION bump and a cold re-index.
That was turned down before it landed. A hover needs the doc of one
definition, whose site the tree already has, and reading that one file costs
little. Measured on discourse and rails (median of five, page cache warm), a
hover whose definition file the session has not read yet costs 0.3–1.3 ms
more, and 0.2–0.45 ms more once it has. The worst case was `has_many`, in a
1,909-line file. Storing docs instead would grow every store, gem docs
included, for a fact no index-wide query reads. It would also cost every user
a re-index to ship. Reading also sees an unsaved buffer, which a stored fact
cannot.

The cost of reading is staleness. The index says line 15, and the file may
have moved since. The definition is taken at the indexed line if one of the
same name and kind is there. Otherwise it is taken as the only definition with
that name, kind and enclosing scope in the file as it is now. If neither
holds, no doc is shown. A comment attached to the wrong definition is worse
than none, so this refuses rather than guesses.

**What counts as the doc.** This is the contiguous `#` block directly above
the definition, or an `=begin`/`=end` block whose `=end` sits there.

- A blank line ends it.
- A Sorbet `sig`, one line or `do … end`, is stepped over.
- A bare `private` line is not stepped over: a comment above it heads a
  section.
- Dropped: magic comments, `rubocop:`/`standard:`/`steep:` directives, a
  shebang, RDoc directives (`:call-seq:` with its body) and rbs-inline's `#:`
  lines. Also dropped: RDoc's `#--`…`#++` hidden text, and headings
  (`= Active Record`) at the top of a class doc.
- `:nodoc:` and `:stopdoc:` mean no doc.

The summary is the first paragraph, capped at six lines and 400 bytes and
marked `…` when cut. The rest is one click away, at the linked definition.

Of YARD's tags, `@return` (rendered as **Returns** `Type` — description, and
omitted for `void`) and `@deprecated` (shown first) are kept. The signature
cannot say either. `@param` is dropped because the signature above already
shows the parameters with their defaults, so a list would repeat it at twice
the height. `@example`, `@see`, `@raise`, `@option` and directives (`@!…`) are
dropped as more than a glance.

RDoc and YARD inline markup becomes Markdown: `+x+`, `<tt>x</tt>` and `{X#y}`
become code, and `\Rails` loses its backslash. A stray `<` is escaped, or
`Array<String>` renders as an unknown HTML tag and vanishes.

**The signature** is the definition's own text after its name: the
parenthesized parameters (whitespace collapsed, comments dropped, capped), a
class's `< Parent`, or a constant's first line (`…` when it continues). This
text goes after the FQN the tree settled on: `Owner#name(…)`,
`Owner.name(…)`. A method carries no `def`: `def String#downcase` is not Ruby,
and the `#`/`.` already says it is a method. A macro-made method (`attr_reader`, `has_many`) has no parameter
text of its own, so its parameters come from the extracted facts, with `…` for
defaults the facts do not carry.

**Completion** shows the same doc and signature on `completionItem/resolve`,
for the one item chosen. Items carry `{root, owner, singleton}` or `{root,
fqn}` in `data` to find it again. The list carries none, since a list can be
hundreds of items. `workspace/symbol` shows none, because it fires per
keystroke over up to 200 rows.

**Not chosen.**

- **Docs as blob facts** (the brief as first written). See above: it grows the
  store and forces a re-index for a fact read one definition at a time.
- **A compact `@param` list.** It repeats the signature.
- **Keeping the rung in words** ("resolved via the constructor"). It is true,
  but a person reading a certain answer does not need it; `--def --json` and
  `--explain` keep it.
- **Reading every declaration site of a reopened class** for its doc.
  `ActiveSupport` has hundreds of sites. The first five are read, and the first
  with a doc wins.

**Reverses if** a batch consumer needs docs, such as a `doc` field in `--def
--json` for agents, or a search over doc text. A per-query file read is then the
wrong shape, and docs earn a place in the blob layer.

## DEC-053 — A `require` string is a link to the file it loads

**Decided.** In `--lsp`, the string literal of a `require`,
`require_relative`, `load` or `autoload` answers `definition`, `hover` and
`documentLink` with the file it names, opened at its top. The whole literal is
the origin — `originSelectionRange` on a `LocationLink` when the client
advertises `linkSupport`, plain `Location`s otherwise. Resolution is static
and lives in a pure module (`serve/require.rs`): given the requiring file, the
string, the ordered load-path directories and a file-exists test.

The report that started this: Cmd-clicking a `require_relative` path jumped to
references of a same-named thing, because definition answered nothing and
VS Code fell back.

**The rules.**

| call | where it looks | extension |
|---|---|---|
| `require_relative "a/b"` | the requiring file's directory | `.rb` appended unless written |
| `require`, `autoload` | `./…`/`../…`: the checkout root, Ruby's working directory statically; absolute: itself; otherwise the load path | `.rb`, then compiled |
| `load` | the load path, then the checkout root | as written |

The load path, in order: the checkout's `lib/`, `spec/`, `test/`; its path
gems' `lib/` (`Gemfile.lock` `PATH` sections — rails' `remote: .` holds a dozen
gems, so each named gem's directory is offered too); each bundled gem's `lib/`
(`Store::gems_used`); the stdlib of the Ruby those gems were installed into,
then its arch directory.

Per directory, `x.rb` then `x.so`/`x.bundle` before moving on — directory
outer, extension inner, as `rb_find_file_ext` does. A compiled extension that
comes first is Ruby's answer, so it is reported (the hover names it) and is no
definition: there is nothing to open, and the `.rb` further down is not what
runs. Several matches are all returned in path order: the definition is a peek
list, the hover lists them. The order among gems here is alphabetical, not
bundler's, which is why several are never collapsed into one.

**Why `spec/` and `test/`.** rspec-core puts `lib` and its default path on
`$LOAD_PATH` (`configuration.rb`: `directories = ['lib', default_path]`), and
railties' `test_command.rb` appends `test`. That is the only way `require
"rails_helper"` and `require "test_helper"` resolve at all. `app/*` is left
out: `load_defaults "7.1"` sets `add_autoload_paths_to_load_path = false`, and
requiring app code is the autoloader's job.

**Paths built at runtime.** Followed only when they are literals in disguise:
`File.expand_path("x", __dir__ | __FILE__ | File.dirname(__FILE__))`,
`File.join(__dir__, "a", "b")`, `File.expand_path(File.dirname(__FILE__)) +
"/x"`, `"#{__dir__}/x"`, `Rails.root.join("x")` with or without `.to_s`. Each
has one answer whoever runs it. Anything else — a variable, a method, any
other interpolation — answers nothing. Discourse has 1 467 top-level
`require`s; the `dirname … + "/x"` idiom is 130 of them (vendored test
suites), `expand_path(…, __FILE__)` 17, `Rails.root.join` 18. The span of a
built path is its literal part(s).

**The standard library** is found beside a gem the bundle resolved:
`<prefix>/lib/ruby/gems/<abi>/gems/<gem>` sits beside `<prefix>/lib/ruby/<abi>`
(rbenv, asdf, Homebrew, system), and rvm's `.rvm/gems/<ruby>[@set]/gems/<gem>`
beside `.rvm/rubies/<ruby>/lib/ruby/<its one ABI directory>`. That is the Ruby
the bundle was installed with, which is evidence; a `.ruby-version` lookup
would be a second guess. A bundle vendored into `vendor/bundle` names no Ruby
and gets no stdlib. `src/tree/core.rb` is no help here: it stubs core classes,
not files. On discourse, `require "open3"` lands on rvm's `open3.rb`, and
`require "json"` offers both the `json-2.19.9` gem and the stdlib copy.

**Where the click lands.** The whole string, not the path segment under the
cursor. Ruby resolves the whole string; a directory cannot be an LSP location;
and a per-segment answer would make the target depend on where in the string
the cursor happened to be, which is the surprise this fixes.

**`documentLink`** links only a string with exactly one non-native file behind
it. A link has one target, and picking one of several would be a guess made
silently; those stay with `definition`'s peek list. An unresolved string is
not underlined, so the underline itself says "this resolves". There is no
`documentLink/resolve` step: resolving eagerly costs too little to defer.

**Cost** (discourse, 308 load-path directories, release build, isolated store,
server-side, repeated requests):

| what | ms |
|---|---|
| building the load path, once per checkout | 8.6 |
| `documentLink`, `lib/onebox/engine.rb` (70 requires) | 0.37–0.56 |
| `documentLink`, `config/application.rb` (32) | 1.7–2.2 |
| `documentLink`, `lib/guardian.rb` (16) | 1.0 |
| first request on a file (read, parse, locate) | 7–16 |
| `definition` / `hover` on a require string | 0.1–0.26 |

What makes it cheap is listing each gem and stdlib directory once — they never
change — so a lookup stats only the directories that hold the path's first
component; the checkout's own few are stat'ed each time. The load path is kept
until `gems_used` changes. Before the gem list was read once per request and
the candidate names once per require (not once per directory), `guardian.rb`
took 5–8 ms.

**Not chosen.**

- **Linking the first of several matches.** A silent guess.
- **Segment-by-segment targets.** See above.
- **Each gem's gemspec `require_paths`.** `lib/` is what `--index` walks and
  nearly every gem's only entry; reading gemspecs is a Ruby-evaluation problem.
- **Bundler's load-path order**, from the lockfile's dependency graph. The
  honest alternative is listing every match, which is what is done.

**Reverses if** several-match answers turn out common and noisy in use — then
reconstructing bundler's order earns its keep and the first match can be the
answer.

## DEC-054 — A CLI query does not free its tree

**Decided.** Every CLI command that builds a tree holds it as
`ManuallyDrop<Tree>` (`OneShotTree`, built by `build_tree`), so the process
exits without freeing the namespace string by string. The store the command
opened is still closed normally, so `PRAGMA optimize` still runs; what is not
closed is the tree's own read-only loader connection.

**Why, measured.** Freeing the tree was timed directly (`drop(tree)` around
`--ancestors`), since wall time on the scaled corpora was too noisy to use:

| | tree build | its drop |
| --- | ---: | ---: |
| discourse | 170 ms | 16 ms |
| synthetic monorepo, 10× discourse | 0.8 s | 76 ms |
| synthetic monorepo, 30× discourse | 2.7 s | 216 ms |

It grows with the tree, ~8 % on top of building it, and buys nothing: the OS
takes the pages back in one go. Discourse, eleven interleaved rounds, medians:
`--ancestors` 210 → 197 ms, `--def` 227 → 197 ms, `--refs Topic#title`
384 → 361 ms. Output identical.

**Not applied to the LSP**, whose trees live for the session.

## DEC-055 — A no-op index does not load the known blobs

**Decided.** Before computing what to parse, `index_files` asks
`Store::map_unchanged` — the same map-key comparison `write` already made to
skip a rewrite (DEC-035) — and loads the set of known blob OIDs (DEC-046) only
when the map moved. The set stays loaded, once, for the gems after it.

**Why, measured.** On discourse the set is ~4 ms and this changes nothing
visible. It grows with every blob on the machine: on the 30× synthetic
monorepo (336k files, 335k blobs) it was 62–735 ms of a no-op. No-op
`--index` there, git's untracked cache and fsmonitor on, six interleaved
rounds: **503 → 419 ms** median.

**What the rest of that no-op is**, since it is now most of the answer to
"how long does nothing take at scale": `git status` 0.09 s (with fsmonitor;
1.2 s with only the untracked cache, 2.7 s with neither primed), `git
ls-files -s` 0.1 s, and ~0.3 s of trekr reading their output into the file
map. The last is O(files) and the next thing to look at; the untracked cache
and fsmonitor are the user's git config, and the changelog says so.

## DEC-056 — LSP references are bounded, evidence first, and say when they cut

**Decided.** `textDocument/references` keeps at most `referenceLimit`
references (an `initializationOptions` integer, default 1000). Confirmed
callers are kept ahead of possible ones. When anything was left out, one
`window/showMessage` says how much is shown, of how much, and names the
`trekr --refs` query that lists the rest. A client that sends a
`partialResultToken` gets `$/progress` batches. How much is read depends on
the question (the table in ARCHITECTURE's LSP section):

- **Owner known, not streamed:** read every file that calls the name and keep
  the best `limit`, in bounded memory. It stops early only once `limit`
  confirmed callers are in hand, since nothing later outranks them.
- **Receiver never resolved (a bare `to`, `call`, `save`):** read files page
  by page from the index and stop at `limit`. Here "confirmed" means a typed
  receiver that finds *some* method of that name, so a full scan to promote
  those says nothing about the method asked after.
- **Streamed:** read nearest the definition first and stop at `limit`, since a
  stream cannot take back what it sent. Each batch is ordered by evidence; the
  stream as a whole is not, and the message says "nearest the definition
  first" rather than "confirmed callers first".

**Why 1000.** Tiering 300 methods sampled from discourse's `app/`
(`--refs Owner#name`): the largest confirmed tier was 123, the 99th
percentile 42. The confirmed tier is what a cut must never lose, and 1000
leaves eight times the largest seen. The limit cut 14 of the 300 answers, all
dominated by possible sites; the largest was 32,686 possible and 0 confirmed.
At 500 it would cut 21, at 200 it would cut 28.

**Measured**, LSP over stdio, synthetic monorepos of 1×, 10× and 30× discourse
with distinct content. Before is one run; after is the median of three warm
runs:

| | 1× | 10× | 30× |
|---|---:|---:|---:|
| bare `to`, before | 1.1 s, 81,505 locations | 16 s, 815,050, peak RSS 6.4 GB | 65 s, 2,445,150, peak RSS 7.5 GB |
| bare `to`, after | 21 ms, 1,000 | 26 ms | 34 ms, RSS flat at 1.2 GB |
| `Topic#reload`, before | 0.35 s, 5,450 | 3.5 s, 54,302 | 19 s, 162,862 |
| `Topic#reload`, after (reads every file) | 0.31 s, 1,000 | 2.9 s | 9.8 s |
| streamed, first batch: bare `to` / `Topic#slug` / `Topic#reload` | 19 / 20 / 39 ms | 27 / 39 / 98 ms | 38 / 160 / 450 ms |

The streamed first batch for a method with a known owner is dominated by
`files_calling`, which lists every file before nearest-first can sort them.

**The page query's plan is pinned.** A bare name first ran through the same
paged listing unpinned, and it took 0.6 s at 10× and 2.6–9 s at 30× to read
128 files, against 8 ms for the same query in the `sqlite3` shell. The
bundled SQLite had different ideas: with `sqlite_stat4` saying `to` is
everywhere, it walked every file of the checkout and sorted their calls.
`INDEXED BY call_site_name` plus `CROSS JOIN` fix the order, and a unit test
reproduces the skew and checks the plan. `files_calling` has the same
exposure. Inside the server it took 34–56 s for `to` at 30×, against 3 s in
the shell; it is left as is here.

**Rejected.**
- *A time budget for the unstreamed full scan.* It bounds `Topic#reload` at
  30× but makes the answer depend on machine load. The full scan is
  cancellable, and its result does not depend on timing.
- *Confirmed tier only for a bare name.* That tier is not about the method
  asked after (above), and it would still need the full scan to find.
- *Streaming the constant path.* It is answered from the index without a
  reparse, so it is capped and answered whole.

**Known gap.** Neither VS Code (`vscode-languageclient` 9) nor Claude Code's
LSP tool sends a `partialResultToken` for references, so streaming serves no
client in use today. Claude Code shows no `window/showMessage` either, so an
agent gets exactly `referenceLimit` locations with no sign they were cut.

## DEC-057 — A load that doubles the store rebuilds the fact indexes by sorting

**Decided.** When an index is about to parse more blobs than the store already
knows — a first index, above all — `Store::write_bulk` drops the fact tables'
seven secondary indexes (`schema::BULK_INDEXES`), inserts the rows, and
rebuilds the indexes with `CREATE INDEX`, all inside the checkout's one
savepoint. The rebuild's sort spills to temporary files (`temp_store=FILE` for
its duration), not to memory. Gems are never bulk-loaded: they write inside
the bundle's shared transaction (DEC-041), and each is small.

**Why, measured.** A cold index grew much faster than the repo. On the
synthetic monorepo — discourse's Ruby replicated with every file and constant
distinct, so content addressing cannot flatter it — a cold index took 7.9 s at
1×, 56 s at 10× and 978 s at 30×. The single writer inserted every row into
two random-keyed indexes per fact table; once those outgrew the page cache,
each insert was a random read and write.

| cold `--index`, quiet machine | before | **after** |
| --- | ---: | ---: |
| discourse + gems, 5 interleaved rounds | 7.5 s | **6.3 s** |
| 10× monorepo, 2 interleaved rounds | 56.3 / 56.4 s | **39.9 / 40.3 s** |
| 30× monorepo, one run each | 978 s | **240 s** |
| 30× — database | 4.61 GB | 4.44 GB |
| 30× — private memory peak | 331 MB | 393 MB |

Both stores hash identically, 1,453 CLI queries are byte-identical with each
build answering from a store it indexed itself, and the widget_shop gold
report is identical on a store this path built.

**Why it is safe to drop an index.** The drop, the rows and the rebuild are one
savepoint. A reader in another process keeps its WAL snapshot — the old schema,
indexes included — until the commit, and after it sees the new indexes: two
LSP servers on discourse answered 2,645 requests while a 10× monorepo was
bulk-loaded into their store, with no errors and every answer unchanged. An
interrupted load rolls the drop back with everything else; a unit test panics
a bulk write mid-rows and requires the indexes and rows to be as before, and it
fails if the drop is moved outside the savepoint. A second test holds
`BULK_INDEXES` equal to `SCHEMA`.

**Tried and not taken.** At 10×, one run each: a 1 GB page cache made it
slower (104 vs 80 s) and held 1.4 GB. Committing every 20k files bounded the
WAL (0.36 GB) and was slower (120 s) — every commit rewrites the random index
pages it touched, DEC-041's finding again. Sorting in memory held 1.5 GB at
30×. Four sort threads (`PRAGMA threads`) were no faster than one and held
70 MB more.

**What it does not fix.** The WAL still grows to the size of the load (4.3 GB
at 30×), because the checkout is one transaction; a cold index needs about
twice the store's size in free disk, briefly. Bounding it means committing in
batches, which costs time (above) and gives up the checkout's all-or-nothing
write. And the write stays one thread: SQLite has one writer per database, so
a parallel write means a store split across database files — sharded by
checkout or by blob — which is a redesign, not a knob, and is recorded here as
the option if a first index at 30× still needs to fall well below minutes.

## DEC-058 — `files_calling` is pinned to the name's index

**Decided.** `Store::files_calling` reads `call_site INDEXED BY call_site_name
CROSS JOIN file`, filtered to the checkout, and deduplicates and sorts the
paths in Rust — the plan DEC-056 pinned for its paged sibling. A test runs
`ANALYZE` over a name that is everywhere and requires the plan to start from
`call_site_name` and to build no temp B-tree.

**Why, measured.** The references lane (DEC-056) found the bundled SQLite,
given statistics saying a name is common, choosing to walk every file of the
checkout and sort all their calls: 34–56 s for `to` at 30× inside the server.
The CLI's `--refs` reads the same list. On the 30× synthetic monorepo, `--refs
Array#to` (2.4 M calls of `to` in the store), alternating builds: 44 / 31 s
before, **25 / 21 s** after — noisy, and output identical. Where the planner
already chose well it changes nothing: `Topic#title` 6.2 s both ways,
`ActiveRecord::Persistence#save` 4.9 s both ways.

**Measured and not taken: a covering `call_site(name, blob_id)` index.** It
halves this query's SQL — `to` on discourse 73 → 37 ms, `title` 2.1 → 1.3 ms —
for 2 MB more index. But the SQL is a sliver of a references query, which
parses every file the list names (81k calls of `to` on discourse), and it
would cost a schema version, so every user a cold re-index. Worth folding into
the next schema change that happens for its own reasons.

## DEC-059 — A bare-name `--refs` matches its tierings by position, not by scan

**Decided.** `cmd_refs_by_name` tiers every call of the name and then attaches
each tiering to its row. It found each row's tiering with a linear scan of all
of them — quadratic in the name's call sites. It now builds a map keyed by
(path, line, col) once; the first tiering for a position still wins.

**Why, measured.** Harmless on a rare name, and the cost of a common one grows
with the square of the repo. `--refs NAME`, five interleaved rounds on rails,
four on the 30× synthetic monorepo, medians; output identical:

| | rows | before | after |
| --- | ---: | ---: | ---: |
| rails `save` | 716 | 188 ms | 188 ms |
| rails `new` | 13,736 | 672 ms | 595 ms |
| 30× `save` | 17,130 | 5.2 s | 5.1 s |
| 30× `each` | 129,690 | 17.9 s | 9.9 s |

Found by the unbounded-growth audit: every per-query structure that grows with
the repo was checked for work that grows faster than it.

## DEC-060 — The tree becomes a flat, interned, mmap'd snapshot

**Built** (DEC-065), as the shape below describes: a zero-copy layout mapped
by every query and session, methods still demand-loaded. The estimates held —
120 MB at 30× against 117 estimated; loading it, checksum included, 14 ms. At
30×, interleaved against the binary before it, outputs identical: `--def`
3.0 → 0.05 s and 1.1 GB → 34 MB private peak; an LSP's first answer
2.9 → 0.07–0.8 s; live heap per server 1011 → 490 MB, the rest being
completion's listing; three servers' footprint together 5.4 → 2.4 GB. The
full table is in ARCHITECTURE's Measurements.

**Recommended, for its own lane** (as written before it was built). At monorepo scale the cost every question
pays is the tree: each CLI query assembles the whole namespace from SQL, and
each LSP session holds it privately. Measured on the 30× synthetic monorepo
(discourse replicated with every blob and constant distinct):

| | 1× (discourse) | 30× |
| --- | ---: | ---: |
| names / declarations | 45k / 70k | 395k / 894k |
| tree build (`--ancestors`) | 170 ms | 3.5 s |
| — of which the declarations SQL | 68 ms | 2.1 s |
| — freeing it at exit (before DEC-054) | 16 ms | 216 ms |
| `--def`, private memory peak | 0.10 GB | 1.06 GB |
| LSP: tree / completion listing, live | 45 / 32 MB | 531 / ~490 MB |
| a snapshot of it, estimated | 7 MB | 117 MB (61 MB of it paths) |
| materialising it into today's structures (a clone) | 22–31 ms | 222–381 ms |

**Where assemble's time goes** (sampled, discourse): allocation and freeing
46 %, string work — `format!` in `qualify`, `rsplit_once` — 15 %, SipHash
11 %, copying 9 %. Interning names to `u32` and flat arrays attack all of
that, but not the 2.1 s of decoding rows at 30×. So interning alone is worth
perhaps 3.5 → 2.8 s at 30×; it is not the fix, it is the format the fix wants.

**The shape.** The assembled namespace — names interned to `u32`, entries in
flat arrays indexed by id, sites and mixins as index ranges, paths stored once
per root — written as a file and mapped read-only by every CLI query and LSP
session. A deserialising snapshot would already cut the 30× build ~10× (the
clone above is its floor); a zero-copy one makes the load a few page faults and
moves the tree from private memory to shared page cache, which is what cuts
1 GB per query and ~0.5 GB per server. Methods stay demand-loaded from SQL.

**Cross-process safety, as a requirement of the format:**
- **Immutable and content-keyed.** Named by a key over the format version, the
  core stub's hash, and each root's surface key in tree order (checkout plus
  every gem) — the inputs `Tree::build` reads. A file is never modified; a
  mapped file written in place gives torn reads or SIGBUS.
- **Written temp → fsync → atomic rename.** Two builders racing produce the
  same bytes under the same name, so the rename is idempotent.
- **Readers keep their mapping.** A replaced or unlinked file stays valid for
  whoever has it mapped; a reader switches when its stamp moves, as the LSP
  already does for its tree.
- **Header** with magic, format version, key and a checksum; any mismatch —
  including a Homebrew upgrade changing the format — is rebuilt, never read.
- **GC** in `--gc`: a snapshot no checkout's current key names is deleted.
- **Tests:** a concurrent writer and reader process; a truncated file; a
  version mismatch; and the invariant that decides it all — a loaded snapshot
  equals a fresh build — checked over the CLI differential and the gold set.

**Write cost.** A rebuild plus a ~117 MB write at 30×, paid by `--index` when
the key moves (so queries only read), or by the first query after.

**Not done now** because it replaces the tree's representation, which every
resolve path reads, and deserves a lane and its own DEC when built.

## DEC-061 — Storage audit: what the bytes are, and what did not clear the bar

**Where the bytes go.** discourse + rails + gems, 306 MB; the 30× store 4.6 GB:

| | 1× | 30× |
| --- | ---: | ---: |
| `call_site` rows | 135 MB | 2.18 GB |
| `call_site_name` | 47 MB | 794 MB |
| `call_site_blob` | 32 MB | 610 MB |
| `const_ref` + its two indexes | 43 MB | 560 MB |
| `def` + its two indexes | 38 MB | 342 MB |
| everything else | 11 MB | ~120 MB |

Call sites are 70–78 % of the store. A row's 45.6-byte payload is mostly
`nesting` (14.6 B, only 26.6k distinct values across 2.5 M rows), `name`
(7.3 B) and `recv` (6.2 B, one of six words).

**Measured and not taken:**
- **Page size 8 K / 16 K** (fresh stores, discourse + rails, two runs): size
  302–304 MB either way, tree query 170–175 ms, refs 314–330 ms. Nothing.
- **`synchronous=OFF`**: one-file reindex 164 → 138 ms (store-write 66 →
  34 ms; the rest is the WAL checkpoint's fsync). Turned down: a power loss can
  then corrupt the store silently, and a corrupt store answers wrongly — the
  accuracy constraint outranks 26 ms. A larger `wal_autocheckpoint` moved the
  cost out of the write and not out of the run (163 ms).
- **Dropping `call_site_blob` / `const_ref_blob`** (38 MB at 1×, 700 MB at
  30×): no read uses them — `EXPLAIN QUERY PLAN` over the real queries reads
  `def_blob`, `ancestry_blob`, `def_name`, `const_ref_name`, `call_site_name`,
  `file_blob` — but refreshing a blob and `--gc` delete through them, and
  without them each deleted blob is a scan of the table.
- **A covering `call_site(name, blob_id)`** — see DEC-058.

**The slimming that would clear it, sized for a schema lane:** interning
`nesting` (~30 MB at 1×) and `name` (~20 MB across table and index), `recv` as
an integer (~13 MB), and `call_site` as `WITHOUT ROWID` clustered by
`(blob_id, seq)` so the blob index disappears (32 MB, and 610 MB at 30×, where
random inserts fragmented it) — about 30 % of the store together. It touches
every query over these tables, so it waits for a schema change that happens
for its own reasons.

**Is SQLite a bottleneck anywhere left?** Only for bulk writes. A 30× first
index is 240 s after DEC-057, of which the single writer is most; a
purpose-built append-only fact log could approach the parse's own speed, and
per-shard databases would let the writer parallelise. Everything read on the
query path is small and indexed by name, and the one O(repo) read — the tree's
declarations, 2.1 s at 30× — is what a snapshot (DEC-060) replaces. So the
shape is hybrid: SQLite as the source of truth for facts, purpose-built mapped
snapshots for the hot, whole-namespace reads. A custom datastore would pay only
if a first index at that scale has to fall well below minutes.

## DEC-062 — The LSP's background index lowers its own priority, but not to the lowest I/O tier

**Decided.** The `--index` child DEC-039 spawns is marked `TREKR_BACKGROUND=1`
and, first thing, drops itself: `nice(10)`, and disk I/O to macOS
`IOPOL_UTILITY` / Linux best-effort level 7. It reads both back and logs them
(`index_priority`), so a refused request is visible rather than assumed. The
child lowers itself instead of the spawn using `pre_exec`, so a hand-run
`trekr --index` keeps full speed; rq's detached `--warm` does the same.

**Rejected: the lowest I/O tier** (macOS `IOPOL_THROTTLE`, which rq's warm
uses; Linux's idle class). Throttled I/O waits while anyone else's I/O is in
flight, and the index does its writes inside SQLite's write transaction — so
starving its I/O stretches the lock that a save's refresh, a CLI query's
refresh, and every CLI run's close-time `PRAGMA optimize` wait on. Measured
on a cold discourse index (store pre-loaded with mastodon, M-series, 8 cores,
machine shared with other load): with no foreground I/O all tiers ran
~7–8 s; against a synthetic fsync-heavy writer, median of 3, plain 11.3 s,
utility 13.1 s, throttle 23.6 s. Under the LSP with queries running, throttle
ran past 100 s in 2 of 5 runs (one unfinished at 300 s); utility's worst of 8
was 18 s.
rq can afford throttle because a warm is small; a cold monorepo index is not.

**What it bought, measured.** Five interleaved before/after pairs, an LSP
rooted at mastodon issuing definition, hover and references while its child
cold-indexed discourse, plus a CLI `--def` between each round. Medians
during the index, before → after: definition 1.2 → 1.7 ms, hover 0.4 →
0.7 ms, references 16 → 27 ms, CLI `--def` 0.87 → 1.5 s; index 7.2 → 10.9 s.
The quiet baselines drifted as much between runs (references 14 vs 21 ms),
so no foreground gain is resolvable on this machine: definition and hover
answer from the in-memory tree and barely contend, and the CLI's time is
not CPU or disk at all — it is SQLite's 5 s `busy_timeout`, waiting on the
index's write lock: with a write lock held by hand, every CLI command took
5.4–5.9 s, `--def` before answering (`refresh_for_query`) and `--ancestors`
after it (its answer at 0.19 s; the store's drop runs `PRAGMA optimize`).
DEC-066 found both waits were the `optimize`, and removed them. Kept
anyway: it is the cheap, conventional courtesy for work nobody is waiting on, and its cost is bounded by the non-starvable tier.

**Reverses if** a foreground regression is measured that traces to the lower
tier, or a quiet-machine measurement shows the index slowdown is larger than
the few seconds seen here. The larger lever for foreground latency during an
index is the write-lock wait, not priority.

## DEC-063 — Usage is counted, per day, in its own file

**Decided.** Every CLI command and every LSP operation adds one to a daily
counter keyed by surface, feature, flags, caller, outcome, latency bucket and
(for the LSP) whether it opened a session — `usage_daily` in
`trekr.usage.db` beside the store. `--usage` summarizes it; `--json` emits the
rows. It replaces `--usage`'s old summary of `lsp.log`, which saw only the LSP,
while agents mostly reach trekr through the CLI and its skill.

**Why counted, and why this coarse.** The point is evidence for keeping,
cutting or improving a feature — rq deleted its learning feature on exactly
this (rq DECISIONS D10: 349 searches, 346 from claude-code, none used
`--show`/`--open`). Those questions need which feature, which knob, who asked,
and whether it came back empty, not what was asked. So nothing that names code
is kept: no query, no path, no repository (a repo hash was considered and
left out — "how many checkouts" answers no keep/cut question). Latency is a
decade bucket, because one invocation's timing to the millisecond is noise.
The caller taxonomy is rq's, copied, so the two tools' tables compare; an LSP
session with no agent in its environment is labelled by its client's name.

**Why a separate file.** The store is a cache that a VERSION bump drops
(DEC-009) — 22 of them so far, several a month at times. Usage is the one thing
trekr holds that cannot be re-derived, and a history wiped at every extractor
fix could never compare a feature before and after a release. A file of its
own also keeps `--gc` and `--drop` away from it, and it follows `$TREKR_DB`, so
an isolated store (every e2e test) gets isolated counts. `$TREKR_USAGE` moves
it or turns it `off`. Rows older than 90 days are pruned on write; at a few
dozen distinct rows a day that bounds the file to a few hundred KB.

**Why after the answer.** The CLI counts in `run()` once the command has
printed, the LSP once the response is on the wire, as rq did after finding its
write's tail under load (rq DECISIONS D13). A process still exits after the
write, so it is not free for a caller that waits on exit: `--def` on a small
repo, release build, 300 interleaved runs each way — median 28.9 → 29.1 ms,
p90 34.1 → 36.1 ms. Within the run-to-run noise. The LSP keeps one connection
open, so its per-request cost is the upsert alone.

**Outcomes.** `hit`, `uncertain` (ambiguous, confidence below 0.5, or residue
with ranked guesses — an answer, but not the certainty the product sells),
`empty`, `not-indexed`, `cancelled`, `error:<kind>`. A handler notes what only
it knows — the cursor was snapped, the references were cut, the definition was
a `require` — through a per-thread note the dispatcher takes, rather than a
parameter threaded through every handler.

**Also fixed.** The log summary counted a hot-reload `resume` as neither a
session nor a reason to treat the next request as cold, so the successor's
first request — which rebuilds everything — was blended into the warm median.
Counting in the process gets it right by construction: a resumed process
starts with its first request cold and counts a `resume`, not a `session`.

**Not kept.** The old summary's history: `lsp.log` is still written and still
holds it, but `--usage` does not import it. **Reverses if** a question needs
what was asked rather than which feature asked it — that is the log's job, at
`TREKR_LOG_LEVEL=debug`, not this table's.

## DEC-064 — A variable is answered from its file, when asked; an ivar only from its class's chain

**Decided.** In `--lsp`, `definition`, `references`, `documentHighlight` (a new
capability) and `hover` on a local, parameter, `@ivar` or `@@cvar` answer from
the files themselves. `serve/vars.rs` is a pure walk of one file's Prism tree,
cached on the document per edit; `serve/variables.rs` puts it on the wire and,
for a member variable, reads the class's files through the tree. No blob fact,
no schema change, no re-index — the DEC-052/DEC-053 shape: the index names the
files, the question reads them.

The ask was "click an `@thing` that is being used and land where it was set;
best effort, or skip when it's too hard". Cmd-click on a variable answered
nothing, so VS Code fell back to its word-based references.

**Locals: flow, not the nearest line above.** Prism already decides which
identifiers are locals and how many block scopes up each lives (`depth`), so
scope is settled; what is left is which writes reach a read. The first design
was "the latest assignment above, in the same scope chain". That is wrong on
the two shapes a reader clicks most: `x = 1 if c` (the old value still reaches)
and `if … x = 1 else x = 2 end` (both do). So the walk carries the set of
writes that may have set each local:

| construct | rule |
|---|---|
| `x = v`, targets, parameters | replaces the set (the value is walked first, so `x = x + 1` reads the old one) |
| `if`/`unless`/`case`/`case…in`/`&&`/`||`/`rescue` modifier | each branch from the same state; results merged, the no-`else` path included |
| `x ||= v`, `x &&= v` | the old set plus this write |
| `x += v` | this write |
| `begin … rescue` | a rescue clause sees the entry state plus every write the body made, since it may have stopped after any |
| `while`/`until`/`for`/a block | may run again: a read at the top of the body sees the body's own later writes |
| `def`/`class`/`module` | start empty and give nothing back |

The loop rule needs the body's writes before the body is walked. Walking each
loop body twice doubles per nesting level, and a spec file is ten deep. So the
whole file is walked twice: the first pass records which writes each loop body
holds (occurrences are numbered in walk order, so a body is an index range),
and the second seeds each loop entry with those of an enclosing scope. Linear,
and 6 ms on discourse's largest file (8,706 lines, half of it the parse).
Carrying a nested block's own locals into that seed was the first version's
bug — `users_controller_spec.rb` took 50 ms, because the top `describe` seeded
every local in the file and each nested block cloned the lot.

Every binding form is a write with its own name in the hover: method and block
parameters of every shape (`|a, (b, c); d|` included), `in {x:}`, `=> x`,
`in [a, *rest]`, a regexp's named captures, `rescue => e`, `for x in`,
`a, b = …`. A write under the cursor is its own definition, which lets VS Code
offer references from there, as it does on a method's `def`.

**Ivars: the class's chain, or nothing.** An ivar belongs to an object, and the
object is decided where it is written: an instance method or a
`define_method` block writes the instance's; a class body, `def self.x` or
`class << self` writes the class object's. The instance's writes are looked
for in `Tree::ancestors` of the class — superclasses and included modules, as
far as the tree resolves them — and the class object's in that class alone
(class-level ivars are not inherited). `Tree::sites` names each owner's files,
reopenings included; gems and core are skipped, and at most 64 files are read.
A mention in those files counts when `Tree::scope_fqn` places its written
nesting on the chain and `self` agrees. No tree API was added.

Writes are `@x =`, `||=`/`&&=`/op-assigns, multi-assign and `rescue => @e`
targets, `attr_writer`/`attr_accessor` (symbols or strings; `attr_reader`
writes nothing), and `instance_variable_set(:@x, …)` with a literal name on
`self`. `initialize` first, then by file and line: the first is where a reader
looks, the rest is a peek list.

What is **not** searched: a subclass (a base class reading an ivar its
subclasses set), a module's includers (a concern reading an ivar the model
sets), and a receiver other than `self` (`controller.instance_variable_set`).
Each could be answered — `Tree::includers_of` exists — but the answer would be
"one of these classes, depending on the object", and a module mixed into many
classes makes that a list of guesses. Returning nothing leaves the editor's
own word matching in place, which is honest about being text. When the class
cannot be named at all (no checkout, a top-level ivar, a class not yet saved),
the file at hand is searched — it is certainly part of the answer.

**Class variables** follow the ivar rules, shared by the class and its
instances. **Globals** are skipped: a write can be in any file of the program,
and answering means scanning the checkout for a feature nobody asked for.

**Highlight** is the file's own mentions, writes marked as writes, matched for
an ivar by written nesting and `self` — no index needed, so it works in an
unindexed checkout.

**Measured** on discourse (release build, isolated store, a machine shared with
other work; 20–60 variable positions per file): median 0.1–0.8 ms for
definition, highlight, hover and references on a variable. The first ivar
question about a class reads its files, up to 11 ms (`TopicQuery`), beyond the
tree the session builds once for every operation. Spot-checked on `User` and
`ApplicationController`: `@readonly_mode` lands in `lib/read_only_mixin.rb`,
`@canonical_url` in `lib/canonical_url.rb`, `@import_mode` on its
`attr_accessor`; `@asset_preload_links`, set only through
`controller.instance_variable_set` in a helper, answers nothing.

**Not chosen.**

- **Variable facts in the index.** A local never crosses a file, and an ivar's
  answer is a few files the tree already names. Storing them would put millions
  of rows in every store (DEC-012 kept assignments out for the same reason) and
  cost a re-index, for an answer that must reflect the unsaved buffer anyway.
- **Every write of `@x` in the checkout.** One click on `@user` in a Rails app
  would list hundreds of unrelated classes' writes.
- **Includers of a module, subclasses of a class.** See above; an ambiguous
  owner answers nothing.
- **Globals.** See above.

**Reverses if** "nothing" on a concern's ivar turns out to be what people click
most — then a module with exactly one includer (`Tree::includers_of`) is a
determinate answer and earns its keep.
## DEC-065 — Tree snapshots live beside the store, one per checkout, keyed by their inputs

**Decided.** DEC-060's snapshot is persisted as `trekr.trees/<checkout>-<key>.tree`
next to the database, mapped read-only by every `Tree::build` whose key it
answers to, and built by the first query that finds none. The format is one
flat layout used both in memory and on disk, so a tree built in-process and a
tree mapped from a file answer through the same code (`Names::Frozen`), and a
fixture tree exercises the same path a real one does.

**The key** is SHA-1 over the format number, the crate version, the source
text of `tree/mod.rs`, `tree/snapshot.rs`, `tree/core.rb` and `store/mod.rs`,
the schema version, and each root's path and surface key in tree order. The
paths are in it because sites are absolute. The source text is in it because
the format number only says the *layout* is the same: a release — or a dev
build between two — that changes how the namespace is assembled would
otherwise read a snapshot an older assembly produced and answer from it with
nothing to say so. Hashing the source makes any edit to that code a rebuild,
which costs one assembly per checkout and cannot be forgotten, as bumping a
constant by hand can.

**Why one per checkout, retired on write.** The LSP's refresh-on-save moves a
checkout's surface key on every save; keeping every key's snapshot would leave
a 120 MB file per save at 30×. A new snapshot therefore removes the same
checkout's older ones (its name's prefix is the checkout's), and only its
own: another checkout's current snapshot is not this one's to judge. A process
still mapping a retired file is unaffected — an unlinked file stays valid for
whoever has it mapped — and moves to the new one when its key moves. Two
processes at different keys for one checkout can retire each other's file in
a window; the cost is a rebuild, never a wrong answer.

**Collected by `--gc`.** Retiring on write leaves two kinds of file: the
snapshot of a checkout whose key moved with no query since, and the snapshot
of a checkout that is gone. `--gc` recomputes every surviving checkout's
current key and removes any `.tree` none of them names, plus temporaries older
than an hour (a live writer takes seconds). A dry run treats the checkouts it
would collect as gone, so it reports what the real pass does.

**Why a checksum over every byte on open.** It is a tripwire for a torn or
rotted file, and it reads every page — which for a mapping means the page
cache's pages, not private memory. At 30× the whole load, checksum included,
is 14 ms against a 3.5 s assembly.

**Correctness, checked.** 1,470 CLI queries and 944 LSP requests on discourse
and widget_shop are byte-identical to 11dbbb6, each binary on its own store,
both on the pass where four concurrent queries raced to build each snapshot
and on the pass that read them; the widget_shop gold report over all 3,075
sites is identical. Unit tests cover a truncated file, a format
or key mismatch under the right name (each rebuilt and rewritten whole), a
moved key (new file, old one retired), racing builders (one file), and a
second *process* that rewrites the very file a reader has mapped and then
unlinks it while the reader keeps answering — which fails if a snapshot is
ever written in place.

**When it is built: the LSP's background index, else the first query.**
Measured at 10× and 30×, one edited definition per round, four interleaved
rounds, building in `--index` against building in the first query after it:

| | `--index` | first query | together |
|---|---:|---:|---:|
| 10×, lazy | 1.0–1.4 s | 1.0–1.3 s | ~2.2 s |
| 10×, in `--index` | 2.0–3.4 s | 0.02 s | ~2.4 s |
| 30×, lazy | 5.1–5.4 s | 10.4–12.7 s | ~16 s |
| 30×, in `--index` | 15.6–16.4 s | 0.05 s | ~16 s |

The cost is the same wherever it lands, so it lands where nobody waits: the
index the LSP spawns (already at low priority, DEC-062) builds it, taking the
assembly off the session's request thread, and an index a person or agent runs
stays as fast as it was — it may never be followed by a query, and a CLI query
refreshes the file it asks about, moving the key anyway. `--usage` counts the
operations that paid (`tree-built`), which is the evidence to revisit this
with. The 30× build after an index is 10–12 s, not the 3.5 s measured warm: the
index has pushed the declaration rows out of the page cache.

**The miss, profiled.** Past assembly, a 30× miss spent 0.56 s encoding,
0.22–0.66 s freeing the assembled map, and only 60–100 ms writing and syncing
120 MB. Interning through Fx rather than SipHash and writing sections straight
into the output took encoding to 0.37 s (best of four, interleaved); the map is
freed on its own thread. Same bytes.

**The LSP's stamp is the snapshot key.** A session stamped its tree with the
checkout's own surface key, so a bundle moving to another gem version — which
changes `Gemfile.lock` and no Ruby file in the checkout — kept the old
version's tree until the checkout's own files changed. The key already covers
every gem's surface key; computing it per request is three small queries and
a hash of the roots. An e2e case switches a lockfile between two vendored
versions and fails on the old stamp.

**Reverses if** the store moves to a network filesystem, where a mapping's
guarantees are weaker (as for DEC-051).

## DEC-066 — A query never waits on another process's write lock

**Decided.** Nothing on a query path waits for SQLite's write lock. The
close-time `PRAGMA optimize` runs with `busy_timeout` 0 and is skipped on
`SQLITE_BUSY`. A one-file refresh that meets a writer is not retried in place:
`--def` answers from the committed index and says so in `index.busy` (the file
it could not refresh), and the LSP keeps the saved path and retries every
250 ms until the refresh lands. Writers (`--index`, `--gc`, `--drop`) keep the
5 s timeout; waiting their turn is their job.

**Where the wait actually was.** DEC-062 blamed `refresh_for_query` for
`--def`'s wait. It was never the refresh. Its transaction reads before it
writes, and SQLite refuses a read transaction's upgrade at once instead of
calling the busy handler, so the refresh failed in microseconds. The whole
wait was `PRAGMA optimize` in `Store::drop`, and `--def` paid it *before*
answering only because its store drops at the end of a `match` arm, ahead of
the print. `optimize` takes the write lock as soon as two tables planned with
statistics are candidates (`nCheck == 2` in SQLite's pragma code), before it
has decided whether to analyze anything. So every read command waited out the
index to do nothing.

**How often `optimize` does anything.** A build with `optimize` in debug mode
(`0xffff`, which reports the `ANALYZE`s it would run) found none after
`--def`, `--refs`, `--ancestors`, bare, `--status`, `--gc --dry-run`, or a
no-op `--index` on a rails store. It re-analyzes a table only when the table
is 10× larger or smaller than its statistics say. `--index` already regathers
them at a tenth of growth (DEC-042), so the only case left is a 10× shrink by
`--gc`, and the next uncontended close still catches it. Skipping it under a
writer costs nothing: that writer is an index, and it analyzes for itself.

**The refresh was losing edits, silently.** The failed refresh was read as
"unchanged". `--def` then printed "the checkout moved; other files may lag",
which claims the file asked about is current when the answer came from its
old facts. The LSP dropped the save outright. A background index scans before
it writes, so the edit stayed missing until the next save. In four runs of
five saves each, under a held lock or during a background index, 1 of 20
saves landed before and 0 of 20 at once after. At the end of each run the
lock was released and the last save checked: before, it was missing in 3 of
4 runs; after, it had landed in all 4.

**Measured.** Release builds, rails store (3.3k app files + 74 gems), M-series,
8 cores, load average 7–17 from other work. Wall to exit, median of 3:

| with the store's write lock held | before | after |
|---|---:|---:|
| `--def` (answer printed at) | 5.47 s (5.46) | 0.092 s |
| `--def`, file edited | 5.43 s | 0.095 s, `index.busy` |
| `--refs` | 5.29 s (0.063) | 0.104 s |
| `--ancestors` | 5.36 s (0.092) | 0.095 s |
| `--status` | 5.20 s (0.015) | 0.010 s |

During a real cold discourse index (`TREKR_BACKGROUND=1`, queried 1.5 s in,
two rounds): before, 1.5–3.6 s, and `--status` 5.26 s once. The waits were
bounded by whenever the index happened to release the lock. After:
0.011–0.109 s. Quiet, no lock: 0.01–0.10 s either way, within noise. LSP
definition, hover and references took the same before and after, held lock
or background index (0.4–0.9 ms, 0.4–0.9 ms, 118–175 ms medians). The server
never waited, it lost the edit, and that is what changed. With nothing
locked, outputs are byte-identical to the previous build on 535 rails
queries (177 `--def`, 143 `--refs`, 107 `--ancestors`, 107 bare, `--status`),
stdout, stderr and exit code.

**The usage counter** (DEC-063) keeps its own 200 ms timeout. With
`trekr.usage.db` held, every command exits about 0.23 s late, before and
after. That timeout is the cap for a pathological holder: a real concurrent
writer holds the lock for one upsert.

**Rejected: `BEGIN IMMEDIATE` for the refresh.** It would take the lock even
when the file is unchanged, the common case, and turn a read into a write
that can be refused. The deferred transaction already fails fast.
**Rejected: a short wait (50–200 ms) before giving up.** An index holds the
lock for seconds (the app's write, then the whole bundle's gems in one
transaction, DEC-041), so a short wait almost never wins. It would only add
latency to the answers that were going to be stale anyway.

**Reverses if** SQLite starts invoking the busy handler on a read
transaction's upgrade. The lock tests in `store::lock_tests` pin that. Or if
statistics are found stale on a read path that `--index` does not cover.

## DEC-067 — Errors exit with sysexits codes, apart from every verdict; `2` means not indexed

**Decided.** Every error exits on a sysexits code, one per remedy: `64` usage,
`66` a path that does not exist or is outside any checkout, `69` git could not
be run, `70` a bug, `74` the store or a file could not be read or written.
Under `--json`/`--ndjson` an error is one `{"error", "kind", "code"}` object on
stdout, and the message is on stderr in every mode. That includes clap's own
parse errors, whose output mode is read off argv before clap parses it: `-j`,
`--json`, `-J`, `--ndjson`, alone or in a cluster of short flags, and nothing
after `--`. `Failure::exit_code` is the one mapping, and the JSON `code` is read
from it, so the object and the process cannot disagree. This is rq's D22,
ported, with the same `kind` names where they mean the same thing: `usage`,
`not_found`, `internal`, `database`.

| Exit | Meaning | `kind` |
|---|---|---|
| 0 | an answer | |
| 1 | nothing found — `status` says whether certainly (DEC-080) | |
| 2 | no answer yet: the checkout is not indexed (`status: not_indexed`) | |
| 64 `EX_USAGE` | the command line is wrong | `usage` |
| 66 `EX_NOINPUT` | a named path is missing, or in no checkout | `not_found`, `not_a_repo` |
| 69 `EX_UNAVAILABLE` | git could not be run | `git` |
| 70 `EX_SOFTWARE` | trekr failed at something that should always work | `internal` |
| 74 `EX_IOERR` | the store, or a file it reads, failed | `database`, `io` |

**Before this**, every error exited `2`, the code `not_indexed` also exits.
Under `--json` only `not_indexed` was structured, and every other failure was
plain text on stderr with nothing on stdout, so a JSON caller had nothing to
parse. A script branching on `2` could not tell "index this checkout, then
ask" from a typo'd flag or a file that does not exist. Clap exited `2` for a
bad flag as well. A caller that indexed and retried on `2` would do so for a
typo forever.

**What `2` means now.** It is kept, for one thing: `not_indexed`. That is
trekr's "no answer yet", the counterpart of rq's `warming`. The question was
never asked, so it is not `1`. Nothing about the call is wrong, so it is not an
error. There is one difference from rq. rq's `2` can be retried blindly,
because rq finishes its own warm-up. trekr's CLI never indexes on its own, so
the retry has to follow the `hint`. That is still one reaction to one code:
run `trekr --index`, then ask again. Retiring `2` would have put a setup step
on the error codes. That is the confusion DEC-035's `not_indexed` exists to
prevent: it reads "the tool failed" when the truth is "nobody has looked yet".

**How a failure gets its `kind`.** Where only the call site knows what a
failure means, it is tagged there. A `NotFound` reading a file the caller named
is their typo (`not_found`), so `--def` and `--symbols` tag it. A `NotFound`
elsewhere is trekr's own problem. `--index PATH` and `--drop PATH` check that
the path exists before asking git. Otherwise git runs in the nearest existing
parent and can answer for a checkout the caller never meant. git's failures
carry a type (`scan::GitError`), not a string, so "not a git repository" is
`not_a_repo` and anything else is `git`. What nobody tagged is classified from
the chain: SQLite is `database`, other I/O is `io`, and the rest is `internal`.
An unanticipated failure is a bug by definition. `--usage` counts each failure
under the same `kind` (DEC-063's `error:<kind>`). The earlier labels
(`store`, `not-a-repo`, `other`, `input`) stay in the rows already kept.

*Why two codes are shared.* The number is for the caller that cannot read JSON,
and it needs to know what to do next. `not_found` and `not_a_repo` both mean
"point it at the right path". `database` and `io` both mean "look at the disk
or `$TREKR_DB`". `kind` still tells them apart for a JSON caller.

*Reverses if:* a caller needs two errors under one code told apart without
JSON. Split that code; never reuse `1` or `2`.

## DEC-068 — `super` is a call of its method's name, looked up after the method's owner

**Decided.** The extractor records `super` (and bare `super`) as a call of the
enclosing method's name with receiver shape `super`, and the resolver answers
it by Ruby's rule: the first definition of that name *after* the method's owner
in the ancestors of the object running it. `--def` answers with
`resolved_via: super`, `--refs` tiers super sites, and `--dead` gives a method
reached only that way the tier `super-only`.

**Before this**, `super` was not a fact at all. `--def` on it snapped to
another name on the line and answered that one — `normalize(super(value))` in
rails answered `normalize` at confidence 1.0 — and a method reached only from
its overrides had no references, so `--dead` listed every method in a super
chain as unreferenced.

**Which classes are asked.** A class's method answers from the class's own
chain: a subclass only adds ancestors in front of its parent, so the part after
the class is the same for every object that can run the method. A module's
method answers once per class that mixes it in, and they must agree to be
`resolved`; disagreement is `ambiguous` with the other landings as candidates
(DEC-027), and no includer is `residue`. The includers are the *mixers* —
classes whose own `include`/`prepend` names the module, directly or through a
module — rather than `includers_of`'s every class with the module in its chain,
because a subclass's tail after the module is its parent's. The first cut used
`includers_of` and paid 190 ms on rails for the one `super` it met, since that
linearizes every class; resolving the mixin edges once costs a few
milliseconds.

**Only where the owner is the scope.** A `def obj.x`, and a `def` inside a
block — `Class.new do`, `class_eval do`, RSpec's `let` — land on whatever the
code runs against, so their `super` is not recorded and `--def` on it says so
instead of snapping. A class whose superclass is computed
(`DelegateClass(Base)`) records that expression as its parent, which resolves
to nothing and so is named in `unresolved_ancestors`; it used to get Ruby's
implicit `Object`, which sent `super` from its `initialize` to
`BasicObject#initialize` with confidence 1.0.

**Core had to say where `initialize` is.** The stub declared it only on
BasicObject, so `super` in an exception's `initialize` resolved confidently to
BasicObject. It now declares `initialize` on the 32 core classes Ruby defines
one for, read off Ruby 3.4's `instance_method(:initialize).owner`, and
`inherited` moved to Class, where Ruby has it. A `super` landing in core is
still only as right as the stub is complete (ARCHITECTURE, known gaps).

**`super-only` is its own tier** rather than `single-caller`, because a method
reached from an override is live exactly when the override is — neither
unused nor something to inline into its one caller. `super_from` names the
overrides so a caller can check them.

**Measured.** The widget_shop gold set, retraced with the tracer now keeping a
`super` site when the calling frame is the method it entered and the line holds
exactly one `super`: 3,243 sites, 168 of them `super`, both builds on their own
store, context pinned to widget_shop, every site scored. The 0.2.0 build
scored 0 of 128 super sites and put 38 more on another token; this one scores
**118 of 163 correct, 138 found, 0 confidently wrong** (1 ambiguous-wrong). The
gem floor went 1,502 → 1,621 correct with confidently-wrong unchanged at 93,
and no site's verdict got worse. Across every `super` in rails (1,804) and
discourse (625), `--def` answers: resolved 1,257 / 454, ambiguous 71 / 1, and
the rest residue, the largest bucket being a module no indexed class mixes in
(`composed_of` includes `Aggregations` inside a method) and a chain with
nothing after the owner.

**A `super` never lands on its own method** (amended before 0.2.1). The lookup
starts after the owner, so the method a `super` is written in is the one
definition it cannot reach. A chain that holds the owner twice is the one
exception, and the lookup sees it exactly. Where the lookup could not settle
(a module nothing mixes in, a mixer whose ancestors are not indexed), the
method's own `super` was tiered `possible` against itself, and `--def` offered
the method as its top candidate. On rails' `activerecord`, `activemodel` and
`actionpack` libs, 39 `super-only` candidates named no `super_from`, and 6
named only themselves. Such a `super` is now excluded (`different_owner`). A
`super` from an unplaced method names that method's owner in `super_from`.
One whose owner the index does not know at all counts as an ordinary call of
unknown origin, so `super-only` always says who reaches the method.

**An unseen ancestor hides only what could sit there** (amended before 0.3.0).
When nothing indexed after the owner matched, a chain with any unresolved
ancestor tiered the `super` `possible` against every same-named method, in any
class. On a store holding only activerecord, `ActiveRecord::Promise#
pretty_print` was `super-only` from `CollectionProxy` and `Core`, neither of
which has `Promise` as an ancestor: `CollectionProxy`'s one unresolved
ancestor is a module (`ActiveModel::ForbiddenAttributesProtection`). A module
can include another module but never a class, and a class enters a chain only
as a superclass. So a class target stays `possible` only when some chain
asked has a superclass line that stops short of `BasicObject`; otherwise the
site is excluded like any other `super` that lands elsewhere. A module target,
or a bare-name query, is unchanged. On that store six `super-only` candidates
become `unreferenced`, and each is `unreferenced` on a store holding all of
rails, which is the evidence the smaller store was missing.

**A mixin sent at runtime is a mixer** (amended after 0.5.0, DEC-097). The
amendment above had a hole: a class can take a module in without a line in
its body — `Engine.prepend(BootHook)` — so `BootHook#boot`'s `super` found no
mixer and was only a possible reference to `Engine#boot`. The direct fix,
parked as `correct-b2`, kept every class target `possible` whenever the
landings came through a module's includers. That restored this one site and,
measured when it was parked, brought back seven false results, because it let a class look hidden with no
evidence that anything hid it. Recording the sent mixin as an edge gives the
`super` a real landing instead: `Engine` prepends `BootHook`, so the `super`
confirms `Engine#boot`, and `could_hide` keeps its rule. An `include` sent the
same way puts the module *behind* the class, where its `super` cannot reach
the class's own method, and that is now what the chain says too.

## DEC-069 — A class built by a call and assigned to a constant is a class body

**Decided.** `X = Struct.new(…)`, `Data.define(…)`, `Class.new(Base)` and
`Module.new`, assigned to a constant, declare a class or module `X`: the parent
is its superclass edge (`Struct`, `Data`, `Base`, or a computed parent as
written), Struct's literal members become readers and writers and Data's
readers, declared by `Struct.new` / `Data.define`, and the block is visited as
the body. Before, `X` was a constant with no ancestors and the block's methods
landed on the enclosing scope — `--ancestors Sub` answered `[Sub]`, status
resolved.

**The liberty, stated.** A block is `class_eval`'d, so its methods and the
calls in it belong to `X` exactly as in a `class` body. Its *constants* do not:
Ruby's cref inside the block is the enclosing scope, so `FOO = 1` there defines
the outer `FOO`, and a constant read resolves without `X`'s ancestors. trekr
scopes both to `X`. Keeping a second, lexical nesting beside the owning one
would be exact, but a `class` written inside such a block then needs its
methods placed by one and its body resolved by the other — machinery for code
that working Ruby rarely writes, since a constant that resolves only through
`X` raises there. The divergence is recorded as a known gap, not hidden.

**Not modelled**: the same call unassigned (`klass = Class.new do`), whose
owner no constant names, and `class Foo < Struct.new(:a)`, whose members live
on an anonymous class between the two.

## DEC-070 — `define_method` with a literal name is extracted, and a residue reason states only what was checked

**Decided.** `define_method(:x) { … }` and `define_method("x") { … }` in a class
body define `x`, as the looped interpolated form already did; the block is
visited as that method's body (a bare call in it dispatches on the instance,
not the class, and a `super` in it looks up `x`); and handed a method object
rather than a block it is a declaration, with open arity.

Session 23 built the literal form and did not ship it because it moved no gold
site (BASELINE, "built, measured, not shipped"). That was a measurement of
reach, not of harm, and the cost of leaving it out was a confident wrong
*reason*: `--def` on `spin` said the method came from "a gem, a DSL, or
method_missing" when it was defined four lines up. The reason for a settled
type with no method now says what was checked — "nothing indexed in its
ancestors defines this name" — and names no cause, because the cause is
exactly what was not seen.

## DEC-071 — A local is typed by the writes its read can see; `ambiguous` means a known competitor, not a low number

**Decided.** The assignment rungs (`local:new`, `local:const`, `literal`,
`sig`, `finder`, …) vote only with the writes a local's read can see, from the
same flow analysis the LSP answers a local with (DEC-064). The winner is the
type most of them agree on, and the answer is `ambiguous` exactly when some
write gives it another type, with those types' landings as `candidates`.
`confidence` stays the share of reaching writes that agree, so a write that
cannot be typed at all — a parameter, a block parameter, `x = compute` — lowers
it without making the answer ambiguous.

**The reported bug was a vote, not a lookup.** rails'
`belongs_to_associations_test.rb:1042` resolved `post.author` on `Cpk::Post`
at 0.13, agreement 1/8. Constant resolution was right — `Post` there is
`::Post` either way. The ladder counted every `post =` in the file within the
same class and took the *first* one's type: `Cpk::Post.create!`, 975 lines
earlier in another test. It now answers `Post` at 1.0.

**Why no confidence floor.** The question was whether `status` should drop to
`ambiguous` below some confidence. On the widget_shop gold set, answers
resolved below 1.0 are 20 right of 21: `includer` 6 of 6 (four of them below
0.5), `receiver_name` 9 of 9 at 0.5, `local:new` 5 of 6 — and the wrong one is
an instance variable at 0.5, whose file-wide vote is the known weak spot. A
floor at DEC-063's 0.5 would demote four right answers and no wrong one. What
made 0.13 misleading was evidence that should never have been counted, and
removing it is what fixed that answer. So the two fields keep separate jobs:
`status` says whether a competitor is *known* (DEC-027), `confidence` how much
of the evidence agrees. `--usage` still counts anything below 0.5 as
`uncertain`, and the LSP's hover still words a guess as one.

**Measured.** Gold (widget_shop, 3,243 sites): three gem sites went `correct`
→ `residue-hit` and one `wrong` → `residue`. All four were a parameter
(`arel`, `condition`) or block parameter (`stmt`) typed by a same-named
assignment in another method — right three times by coincidence, wrong once,
and confident every time. The truth is still offered as a candidate for the
three. That is the behaviour this decision wants, and it is recorded as a cost
rather than hidden: a "same name elsewhere in the file" rung would win the three
back, and it would be a naming convention stated as a type.

Flow is worked out on first use from the source the facts now keep, so a query
that never types a local pays nothing; the analysis lives in `serve/vars.rs`,
now visible to `resolve/`.

## DEC-072 — A name declared with two superclasses is two classes

**Decided.** When a checkout declares one constant with superclasses that
resolve to different classes — `class Post < ActiveRecord::Base` in one file,
`Post = Struct.new(…)` or `class Post < Other` in another — the tree splits it.
Each superclass is a *variant*: its own entry holding that superclass, and the
mixins, extends and methods written nearest its declarations. The name itself
keeps every declaration site and its nested constants, and no ancestry: its
chain is `[Post]`, with the competing superclasses listed as `unresolved`.

A query picks the variant from the file asking. The receiver ladder, `super`,
the residue ranker and `--refs`' proximity all map a split name to the variant
**nearest the call site** — the one whose declaring files share the most
leading directories with it, the file itself nearest of all. Equally near
several, the one declared in the file named for it wins (`post.rb` for
`Post`): that is how an autoloader or `require "models/post"` reaches a class
from anywhere, where a class in `fake_models.rb` or a benchmark script is
reached only by what sits beside it. Still tied, the query gets the name: a
method every variant defines identically is the answer, otherwise `--def` is
`ambiguous` listing each variant's landing and `--refs` tiers the site
`possible`. `--ancestors Post` answers `ambiguous` with one chain per variant.

**Why.** Ruby raises "superclass mismatch" when both declarations load, so a
checkout holding both is holding two programs. rails keeps its test fakes
(`actionpack/test/lib/controller/fake_models.rb`) beside the models their
suites never load; discourse has a benchmark script's `User = Data.define`
beside `app/models/user.rb`. The old rule was that the first superclass edge
wins, since "a disagreement in the index is bad input rather than a case to
model". It is not bad input in a monorepo, and edges come in path order, so
the winner was whichever file sorted first, and every declaration's mixins
and methods went to it regardless. DEC-069 made it bite. `Post = Struct.new`
used to be a constant with no edge, and it became a superclass edge that
sorts before `activerecord/`. From then on, rails' `Post` inherited
`Struct` with `ActiveModel::Conversion` mixed in, and `Post.find_each` beside
the model's tests found nothing.

**It was already latent in 0.2.0.** rails declares 47 top-level names with
superclasses written differently. Some resolve to one class. Others do not:
`Cpk::Book`, `Reply < Topic` against `Reply < ActiveRecord::Base`,
`User < ApplicationRecord` against `User < ActiveRecord::Base`. The
first-sorting file won each of those. `Cpk::Book.all` in
`calculations_test.rb` was residue in both 0.2.0 and a411be2, because the
fake's `Struct` sorted first. It now resolves.

**Considered.**
- *Keep first-wins, and prefer the declaration with more sites or the one in
  the file named for the class.* One class still wins everywhere, so a
  program's fake is wrong in the one place it is right: `actionview`'s tests
  run against the `Struct`. Naming is kept as the tiebreak only.
- *Always `ambiguous`.* Honest, but 0.2.0 answered `Post.where` beside
  rails' models correctly by luck, and this would give that answer up
  everywhere. Proximity keeps it, and says why.
- *Merge.* That is the bug.

**Where it stops.**
- An `.rbi`'s superclass never splits a name. Tapioca writes the superclass
  it saw at runtime, and that differs from the source whenever the source
  computes it (concurrent-ruby's `class Map < Collection::MapImplementation`
  against the RBI's `MriMapBackend`). Splitting on it cost six widget_shop
  gold sites before it was excluded.
- A declaration with no superclass is a reopen, and it joins its nearest
  variant, or every variant tied for nearest. (*Amended by DEC-075:* not when
  its gem declares no variant.) A third program's plain
  `class User`, such as rails' `activemodel/test/models/user.rb`, is not a
  class of its own, so `@user.authenticate` in activemodel's tests tiers
  `possible` where the merge had excluded it by luck.
- Constant lookup inside a split class's body does not search the variant's
  ancestors, because the lexical scope is the name. A constant inherited from
  `ActiveRecord::Base` and read bare inside such a model falls through to the
  top level.

**Measured** (BASELINE, "Precision fixes before 0.2.1"). On rails `--refs
'ActiveRecord::Querying#where'` confirms 1,216 sites: 1,197 in 0.2.0, 860 at
a411be2. `lease_connection` confirms 1,024: 1,024 in 0.2.0, 972 at a411be2.
The widget_shop gold set moves no verdict against a411be2. Of the 520 CLI
differential positions, one answer changed, and it is a fix.

*Reverses if:* a checkout's programs can be read off something firmer than
paths. That could be a gemspec's `files`, a test helper's `$LOAD_PATH`, or
Zeitwerk's roots. Any of them would replace "nearest" with "reachable", and
the split would stay.

## DEC-073 — A method that provably does not exist has no references

**Decided.** `--refs 'Owner#name'` answers with no references and exits `1`
when the owner does not resolve (`status: residue`) or when its whole chain
was seen and defines no `name` (`status: no_such_method`). Text and JSON
agree: neither lists a site, and both carry the `reason` plus a `hint` naming
the bare-name query, `trekr --refs name`, which does list them.

**Before**, the unknown owner exited `1` in text but `0` under `--json`, with
every same-name call site in the checkout listed as `possible` (686 for
`Nopity#save` on rails). `no_such_method` listed the untyped sites and exited
`0`, while the bare `Owner#name` card exited `1`. Those sites belong to other
owners. The command's point is narrowing them, and offering them as possible
references to a method that is not there reads as evidence that it is. The
exit table (DEC-067) says `1` is a definitive nothing, and that is what these
answers are.

A chain with an unindexed ancestor stays `residue` and still lists its sites,
because the method may be defined there.

## DEC-074 — `--dead` weighs the checkout's evidence, not the store's

**Decided.** Every count `--dead` uses comes from the checkout that holds the
scope. `Store::written_calls`, the cheap pre-filter that clears a name written
as a call more than eight times, counts only that checkout's files.

**Before**, the pre-filter counted a name's calls in every blob in the store,
while the receiver-narrowed pass that decides a tier (`files_calling`) read only
the checkout. So the only thing the rest of the store could do was hide a
candidate, by name, whatever those calls' receivers were. The answer depended on
what else had been indexed. On rails, `--dead activerecord/lib activemodel/lib
actionpack/lib` gave 0.2.1 2,425 candidates on a store holding rails and its
gems, and 2,175 on one that also held two discourse checkouts. 72 `super-only`
became 33. The year-old discourse run moved by 34 in the same way. With this
change both stores give byte-identical output.

**Why the checkout, and not the checkout plus its gems.** Code in the checkout
reaches a gem's methods, but a gem reaches the checkout's methods only through
a hook it calls by convention: `perform`, `call`, a callback named by a symbol.
The name count was never evidence for those. A gem's `job.perform` is an
untyped call, and it only counted because another repo happened to write the
same name. The tier that exists for a method reached by convention is
`convention-only`, and the stated limits (templates, `send`) still apply. The
other direction, `--dead` inside a gem, asks which *apps* call it, and the answer
would be whichever apps are indexed. The checkout is the one scope whose answer
means the same thing on every machine.

*Reverses if:* a checkout's framework hooks need evidence from the framework.
That would be a named rule for the hook (a job's `perform`), not a store-wide
name count.

*Amended before 0.3.0:* **each scope is weighed against its own checkout.**
`--dead lib ../other-worktree/lib` took the first path's checkout as the
evidence for both, so a method called three times in the second worktree was
`single-caller` there when that path came second, and the answer turned on
argument order. The scopes are now grouped by checkout, each group is weighed
against its own, and the candidates are listed together. Refusing mixed
checkouts would also have been correct, but comparing two worktrees is a
reasonable thing to ask, and the grouping costs a loop. A path that does not
exist is `not_found` (exit 66), as for every other command that names a file.

## DEC-075 — A plain class in a gem with no variant is that gem's own class

**Decided.** When a name is split (DEC-072), a declaration with no superclass
that sits in a **program** where no variant is declared, outside that
program's `lib/`, becomes a class of its own. A program is a checkout root or a directory holding a `.gemspec`, and a
file belongs to the deepest one containing it. Every such declaration in one
program is the same class. A plain declaration in a program that does declare
a variant is a reopen, as before, and joins the variant nearest it.

**Why gemspecs.** A monorepo's gems are its programs: rails' `activemodel/`
tests run against activemodel, which never loads activerecord's `User <
ActiveRecord::Base`. The gemspec is the one marker such a checkout writes down
on purpose. It is already indexed (`.gemspec` is Ruby), and adding or removing
one changes the checkout's surface key, so the tree snapshot follows it. The
checkout root counts as a program, so a repository with one gemspec, or none,
behaves exactly as before.

**Before.** A superclass-less `class User` in `activemodel/test/models/user.rb`
was as near to activerecord's `User` as to railties' template (no shared
directory with either), so it joined both variants. Its `include
ActiveModel::SecurePassword` went into activerecord's model, and
`@user.authenticate` in activemodel's tests was `possible` against
`HttpAuthentication`'s three `authenticate` methods. On rails five of the 48
split names change: `User`, `Post`, `Person`, `Session`, `CallbacksTest`. In
each, a plain declaration in activemodel, actioncable, activejob, actionview or
activesupport's tests now stands alone, where it had been merged into two or
three other gems' classes. `--dead` gets the three `authenticate` methods back
as `single-caller`. DEC-072 had recorded exactly that as the cost of the gap.

**The risk, stated.** A gem that reopens another gem's split class to patch it
would now get its own class. That needs a split name *and* a cross-gem
monkeypatch of it, and neither rails nor discourse has one. A dependency read
from the gemspec would settle it, and would be the next step if one turns up.

*Amended:* it turned up before release, in the shape every Rails engine has. An
app splits `User` (`app/models/user.rb < ApplicationRecord` beside a test's
`User = Struct.new`), and an engine with its own gemspec adds a method from
`engines/billing/lib/billing/user_ext.rb`. As the engine's own class,
`User.new.plan` in the app's controller went to a model with no `plan`:
`--def` said the method does not exist, `--refs` excluded the call, and
`--dead` listed `plan` as `unreferenced`. So a plain declaration under a
program's `lib/` is a reopen again and joins the variant nearest it. `lib/` is
a gemspec's default `require_paths`, the part of a gem other programs load, so
a plain class there is the one kind that can be patching someone else's. Its
tests, benchmarks and fixtures are loaded only by the gem itself, and those
keep their own class. All five names this decision changed on rails are in
tests, and rails' `--dead activerecord/lib activemodel/lib actionpack/lib` is
byte-identical with and without the amendment.

*Amended after 0.3.0:* a program's own class is private to it. With a second
engine whose `test/user.rb` declares a plain `class User`, that file is the
shipping engine's own class, and the billing engine's `lib/` reopen joined
it: it is nearer than the app's model (both under `engines/`), so `plan` was
again unreachable from the app. A reopen or an edge now joins only variants
that are not another program's own class. The one class a program declares
for itself is loaded by that program alone, which is the premise that made it
a class of its own.

*Reverses if:* programs can be read off something firmer (DEC-072's
reverses-if: a test helper's `$LOAD_PATH`, Zeitwerk's roots). This rule is
then a special case of that one.

## DEC-076 — Every path in an answer is relative to a `root` beside it

**Decided.** In `--json` and `--ndjson`, every object with a `path` also has
`root`: the absolute root of the indexed checkout holding the file, the gem's
own root for a definition in a gem. `path` is relative to it. A path in no
indexed checkout keeps its absolute form with `root: null`, and Ruby core's
`<core>` stays as it is. `--dead`'s `file` is renamed `path` to match. Text
output writes a path inside the checkout being asked about relative to its
root, and every other path absolute with `$HOME` as `~`.

**Before**, three forms mixed within one answer. `--refs` printed its
`definition` as `~/…` absolute and its references relative. In JSON, sites
from the tree (definitions, `--def`, candidates, the card) were absolute,
references and bare `--refs` rows were relative to the checkout, a variable's
sites were the path as typed, relative to wherever the caller stood, and
`--dead`'s `file` was the argument as typed. A caller had to know which field
was which to open a file.

**Why relative plus `root`, not absolute everywhere.** It is rq's shape (rq
0.53 added `root` per result), so an agent using both joins the same two
fields. A relative path is also what a person pastes and what an answer
about a checkout usually wants to show. Per object rather than once per
answer, because one answer spans checkouts: a call in the app resolves to a
definition in a gem.

**How.** One pass over the finished JSON (`rooted`), run by `emit_json`,
`emit_rows` and so `report`, rewrites every `path` it finds against the roots
the store knows, choosing the deepest that holds it. A command states which
checkout it answers from once, where it checks the checkout is indexed
(`answering_in`). That is also the base for a path still written relative. The
next command gets this without writing anything, and `tests/cli_e2e.rs`
asserts it for `--def`, `--refs` (both forms), the card and `--dead`. The LSP
is unchanged: its locations are `file://` URIs by protocol.

*Reverses if:* a consumer needs absolute paths without joining. Then it gets
a flag, not a second shape.

**Revised (DEC-080):** Ruby core no longer stays `<core>/…` with `root:
null`. Its stubs are written beside the store, as the editor already wrote
them, and a core site is `path: "String.rb"` under that directory, so every
site in an answer opens the same way.

## DEC-077 — A receiver that is a call is typed by the call's declared return

**Decided.** The `other` receiver bucket gets a rung after all: a receiver that
is a literal is its class, and one that is a call is typed by what that call
returns. The previous call's own receiver climbs the ladder; its method's `sig`
names the class (`chain`), an identity method passes the receiver's type
through, and `Foo.new` is a Foo. With no receiver type, every definition of the
name is asked (`chain:name`): if all that declare a return agree, that is the
type, those that declare none make the answer `ambiguous` at declaring /
definitions, and declarations that disagree leave it untyped. Several `sig`s on
one method are overloads, told apart by the positional parameters each names
and whether it types the block `T.proc…` or `NilClass`.

**Why this reverses DEC-020.** DEC-020 declined chains because the type would
have to come from return types that did not exist, and said it reverses if a
new type source appears. Ruby core's return types are that source: RBS
documents them, `script/core_sigs.rb` writes them into the core stub, and core
methods are exactly where untyped Ruby's chains end — `x.to_s.strip`,
`name.gsub(a, b).downcase`, `list.map { … }.join`. Measured against main
(cfba0f1), each build on a store it indexed itself:

| | main | this |
| --- | ---: | ---: |
| gold, gem floor correct | 1,618 | **1,630** |
| gold, confidently wrong | 92 | **92** |
| gold, app code correct | 33 | 34 |
| rails `--refs String#strip`, confirmed | 3 | 109 |
| rails `--refs String#gsub`, confirmed | 10 | 57 |
| rails `--refs ActiveRecord::Querying#where` | 1,216 / 531 / 97 | unchanged |

Twenty gold verdicts moved and none became confidently wrong: 13 residues to
correct (ActiveSupport's `x.to_s.singularize` and friends at 0.18, the rest
resolved), four to right-owner-wrong-site (a `Set` or `Hash` method landing on
core's stub or another gem's reopening rather than the file the trace saw), one
residue to ambiguous-wrong, and two app declarations offered to declarations.
The CLI differential (520 positions) changed 11 answers: 7 fixed, 3 neutral, 1
named below.

**The named cost.** A literal receiver resolves through the tree as a typed
local always has, so `{ … }.to_json` in discourse now resolves to the json
gem's `GeneratorMethods#to_json`. At runtime ActiveSupport prepends its encoder
to Hash from a loop the extractor cannot read, so that answer is confidently
wrong in a Rails app, as `h = {}; h.to_json` already was. It is one of 520.

**What was turned down on the way.**

- *`self` as the owner class.* RBS writes `-> self` for `each { }` and friends;
  mapping it to the owner makes a Struct subclass's `each { }.to_h` look up
  Struct's. The identity rule already carries the receiver's own type.
- *Abstract returns.* `Numeric#+` "returns a Numeric", and an Integer then
  finds `to_s` in the wrong class; a class core subclasses gets no `sig`
  (Enumerator excepted — only `lazy` makes its subclass). Nor `Class`:
  `record.class.find` is not `Class#find`.
- *Excluding on a `chain:name` guess.* The first cut let an `ambiguous`
  receiver exclude a reference, which would hide `def strip` in the checkout
  behind every `x.to_s.strip`. A guess now confirms or leaves a site
  `possible`, and must answer the call it types, as `receiver_name` must.
  Extending that to every `ambiguous` rung cost six correct `where`
  exclusions on rails (`relation.where`, typed by name), so it is scoped.
- *A floor on agreement.* `step[:end_time].nil?` resolves `ambiguous` at 0.00
  because one of ~150 `[]` definitions declares Array. A threshold would be a
  tuning knob with no measurement behind it; the confidence already says it.

Fixed on the way, because a chain exposed it: `has_many :x, class_name: "Y"`
typed its reader as a Y, which made `firm.clients_of_firm.where` an instance
method lookup on Client and excluded four `where` references.

*Reverses if:* a corpus shows `chain:name` picks wrong more often than its
confidence says. In the gold set its eleven picks have the right owner ten
times.

**Amended before release: three ways a declared return overstated.**

- *A sig spoke for calls RBS did not type.* The generator wrote a per-count
  `sig` for the one count RBS gave a class and skipped the others, so
  `Array#first` carried `params(count: T.untyped).returns(Array)` alone. A lone
  `sig` holds for every call, and `["a"].first.size` resolved to `Array#size`
  at 1.0. A block state with any shape RBS cannot type now gets no `sig`, and
  neither does a method whose blockless calls are untyped, since only
  `block: NilClass` confines a `sig` to its state. 19 stubs lost theirs
  (`first`/`last`/`min`/`max`/`pop`/`shift`/`sample`, `Integer#pow`,
  `Process.clock_gettime`, `Marshal.dump`, `Process.fork`, `gsub!`). Refusing
  a lone `sig` whose parameter count differs from the call's was turned down:
  it would change what an ordinary Sorbet `sig` means in user code.
- *A return type was looked up from the owner.* `sig { returns(Item) }` inside
  `module Shop` found `::Item`, because resolution started at the method's
  owner and skipped the scopes around it. A `sig` now resolves in the method's
  lexical nesting, as Ruby does. An association's class resolves from the
  model's *name*, as Rails' `compute_type` does, so a compact
  `class Admin::Note` with `belongs_to :user` finds `Admin::User` where a
  `sig` in the same class would not. These are two rules because two
  evaluators read the names.
- *A class method voted on an instance's return.* Core's `Dir.[]` is the only
  `[]` declaring a return, so every `h[:a][:b]` typed `h[:a]` as an Array and
  confirmed an `Array#[]` reference. An untyped receiver is taken to be an
  instance, and only instance methods vote.

Measured on rails, each build on a store it indexed itself:

| `--refs` confirmed / possible / excluded | 0.2.1 | before | after |
| --- | ---: | ---: | ---: |
| `Array#[]` | 165 / 7,982 / 2,676 | 604 / 7,472 / 2,747 | 214 / 7,862 / 2,747 |
| `Array#first` | 41 / 2,056 / 536 | 221 / 1,860 / 552 | 151 / 1,930 / 552 |
| `String#sub` | 4 / 78 / 1 | 17 / 63 / 3 | 17 / 65 / 1 |
| `Hash#[]` | 407 / 7,886 / 2,530 | 414 / 7,823 / 2,586 | 414 / 7,841 / 2,568 |

The 49 `Array#[]` sites confirmed beyond 0.2.1 follow a `split`, `to_a`,
`map` or `&`. The exclusions beyond it are the chain working
(`Table.new(:users)[:id]` is Arel's, `Thread.current[:x]` is Thread's). The
gold set is unmoved: every verdict matches before and after.

One cost of the third, named: `projects.delete(1).size` now confirms
`String#size`. `File.delete` used to disagree with `String#delete` and leave
the call untyped; without it, String is the only declaring vote and the pick
is `ambiguous`, which confirms as decided above.

**Amended after 0.3.0.**

- *A qualified return kept only its last segment.* `returns(Stripe::Customer)`
  was stored as `Customer`, which the method's nesting then resolved to the
  `Billing::Customer` beside it at 1.0, and `--dead` called
  `Stripe::Customer#email` unreferenced; `::Item` found the nested `Item`. The
  path is stored as written, `::` included, and resolved as Ruby resolves a
  qualified constant: the head through the nesting, the rest by descent.
  `T::Array` and its kin stay `Array`, since `T` is Sorbet's namespace, not a
  scope. An extractor change, so store v33.
- *Only instance methods voted, so a class receiver took an instance's
  answer.* `self.class.build.spin` was typed by `Kit#build` as a Gadget at
  1.0 when `Builder.build` returns a Widget; `model.class.build` and a
  parameter `factory.build` the same. A class method still never makes the
  answer (`Dir.[]` alone must not type `h[:a]`), but one declaring another
  class objects to it and leaves the call untyped. Nothing is added to the
  vote count, so confidence elsewhere is unmoved.
- *The pruning above dropped sigs that were exact.* A block state RBS types at
  only some counts lost every sig, though a per-count sig beside a
  `block: NilClass` one is an overload covering just its count and state. The
  generator now writes the typed counts when a blockless sig confines the set:
  `max_by(n) { }` and `min_by(n) { }` are Arrays and `gsub!(pattern)` is an
  Enumerator again. `first(n)`, `last(n)`, `min(n)` and the rest take no
  block parameter to confine them with, so they stay unsaid.

## DEC-078 — Core is served one file per owner, and its stubs read as signatures

**Decided.** `core.rb` stays the one source, and is served as one file per
top-level class or module (`<core>/String.rb`, written beside the database as
`core/String.rb`). Each `def` is multi-line, its parameters named from the
method's rdoc call-seq with RBS's arity, so the first line is the signature.

**Why.** VS Code's peek list shows a location's file name and its line. Every
core candidate pointed into one `core.rb` and read `def downcase(*args); end`
beside `def downcase; end`, with nothing saying whose each was. Now it reads
`String.rb  def downcase(*options)` and `Symbol.rb  def downcase(*options)`,
and go-to-definition opens a file whose name is the owner. Hover already named
the owner and is unchanged. `--def --json` reports the per-owner path, with `root: null` as core has
had since DEC-076, so a script sees the same owner an editor does.

**How it stays one source.** Splitting happens in the tree layer at build time
(`tree/corelib.rs`): a block takes the comment above it, top-level code goes to
`Object.rb`, and a class may be declared only once. Each file is extracted as
served, so sites carry that file's own lines and nothing maps between
numberings. The files are written only when a location must be opened, and
only when they differ.

*Turned down:* generating per-owner source files in the repository (a hundred
`include_str!`s, and two places to edit), and a virtual URI scheme (the editor
would need the extension to serve it, and an agent reading `--json` could not
open it).

*Superseded in part by DEC-240:* `core.rb` is gone. The one source is now
the app's Ruby's own rbs gem, read at index time into a stub stored with
its stdlib; the per-owner serving, the multi-line `def`s and `Object.rb` are
as above, under a directory per Ruby's signatures (`core/rbs-3.8.0-<key>/`).

## DEC-079 — A schema change is one immediate transaction, and every write re-checks it

**Decided.** `Store::init` sets `busy_timeout` before any other statement,
retries the switch to WAL, and rebuilds a store of another version inside one
`BEGIN IMMEDIATE` that re-reads `user_version` under the lock. An index
`write` and a `batch` are immediate transactions; a one-file refresh stays
deferred so that it never waits (DEC-066). Every write reads `user_version`
inside its own transaction and refuses a store another binary has rebuilt
since this connection opened it; the LSP stops refreshing and logs
`refresh_refused`. A blob row another writer inserted first is kept
(`ON CONFLICT DO NOTHING`) rather than replaced, and a rebuild that dropped an
older index is recorded in `upgrade`, so "not indexed" can say why.

**Why.** Four races, each reproduced with two real processes on one store:

- *Old store, two openers.* Both read the old version, then dropped and
  created table by table with no transaction. One failed with
  `table checkout already exists`; the store could keep 5 of its 12 indexes,
  or `file` rows pointing at blob ids from a different generation, which a
  later index reused, so one repository answered with another's facts. Every
  run of two concurrent `--index` on a copied v29 store corrupted it (27 of
  27); none do now.
- *Current store, shared blobs.* Two indexes that both found a blob new both
  inserted it. `INSERT OR REPLACE` deleted the first row, whose id `file`
  rows already referenced, so the second failed on the foreign key (8 of 10
  runs). Facts are a pure function of the bytes, so the first row is the answer.
- *Fresh store.* The switch into WAL takes an exclusive lock without asking
  the busy handler, so two processes creating the store failed one of them
  (10 of 20). It is retried with jitter for as long as the busy timeout.
- *Deferred read-then-write.* A deferred transaction that has read cannot wait
  for the write lock: SQLite returns `SQLITE_BUSY` at once, since waiting could
  deadlock. `batch` (a bundle's gems) read before writing, as the migration
  would have. Immediate takes the lock, or waits for it, before reading. rq
  found the same bug.

Tree snapshots already had the schema version in their key. That did not help
after a corruption, because the key is a function of what the store says, and
a correct reindex reproduces the key of the wrong tree. `--drop` now removes
the checkout's snapshots, so drop-then-index repairs both.

**Not fixed, and cannot be.** A 0.2.1 LSP still running after a newer trekr
rebuilds the store writes old-format facts into it, because 0.2.1 has no
check. That applies only to builds before this one; restarting the editor
clears it.

**Amended after 0.3.0: it could be fixed, and restarting did not clear it.**
A blob row is keyed by content and never rewritten, so the old facts outlived
the editor, every reindex and `--drop`; only a rebuild removed them. The
schema now gives `blob` a column the old writers do not name, `written_by`,
`NOT NULL` with no default. 0.2's `INSERT OR REPLACE` and 0.3's
`ON CONFLICT DO NOTHING` both abort on it rather than skip, so the old
writer's savepoint fails and writes nothing. Reproduced with a 0.2.1 LSP
saving a file after a new `--index` had rebuilt the store: the edited blob's
0.2.1 facts were kept by 0.3.0 and are re-read by this build. Any later
schema change keeps the protection only if it changes the shape the same way;
`check_schema` covers writers from 0.3.0 on.

**Reverses if** migrations ever become real, rather than drop-and-rebuild:
the immediate transaction then has to span a migration that may take much
longer than the busy timeout, and waiting openers need progress instead of a
5 s wait.

## DEC-080 — The CLI's contract: one name per concept, and `1` means "found nothing"

**Decided.** The command line's public API is its flags, its output shape and
its exit codes, and each follows one rule rather than whatever the command
that introduced it chose.

*Exit `1` is "looked, found nothing"*, not "certainly absent". `--def` on an
untyped receiver, a method whose owner has an unindexed ancestor, and a
constant nobody declared all exit `1` with `status: residue`; `status:
no_such_method` is the certain one. The answer's `status` and `reason` say
which, so the code and the message never disagree: the message hedges exactly
when the status is residue. `2` stays reserved for "not indexed" (DEC-067),
which running `--index` fixes. An unindexed ancestor is usually a gem that is
not installed or a module built at runtime, which `--index` does not fix, so
reusing `2` would send a retry loop around forever.

*A mistake in the command line is `64`*, including one clap cannot see: a
directory where a file is asked for, a position with line or column `0`.
Settings that make a command have nothing to say are not mistakes:
`TREKR_USAGE=off` makes `--usage` exit `1`, with nothing recorded.

*One name per concept in JSON.* A field means the same thing in every
command that reports it, and a command that reports the fact uses that name:

| Field | Means |
|---|---|
| `query` | the input, exactly as typed — never a path trekr rewrote |
| `fqn` | the constant it resolved to |
| `name` | the name at a position (`--def`), or a definition's own name (rows) |
| `definition` | where the thing asked about is defined: an array of sites, always present, `[]` when unknown |
| `candidates[].site` | one ranked guess's location, when there is no `definition` |
| `receiver`, `receiver_text`, `receiver_type` | a call's receiver shape, its source text, its resolved type |
| `unresolved_ancestors` | ancestors that could not be seen, top level and per variant |
| `path`, `root`, `line`, `col` | every located object has all four (DEC-076) |
| `repo` | a checkout's identity (`--index`, `--drop`, `--status`, `not_indexed`) |
| `ruby` | a checkout's Ruby: at a checkout's level in `--index` and `--status`, an object (`version`, `root`, `how`); inside `gems` and `gems.stdlib`, the same choice as a sentence (DEC-292) |
| `answer`, `rows` | `--ndjson`'s last line of a row set: the rest of the `--json` answer, and how many rows came before it (DEC-290) |

**Before**, each command named a fact as it was written: `--ancestors` said
`name` and `unresolved` where the card said `query` and
`unresolved_ancestors`, and the card's own `variants[]` said `unresolved`.
Bare `--refs` rows said `recv`/`recv_text`, the storage column names, beside
`Owner#m` rows that said `receiver`. `--def` said `sites` where the card and
`--refs` said `definition`, and left it out of a residue, so a caller read two
fields to learn where to go. `--symbols` rows had no `path`, `--dead` rows no
`col`, and `--def`'s `query` was the canonical absolute path for a variable
but the typed path otherwise. The renames were clean breaks, pre-1.0, with
the migration in the changelog.

*Why `definition` over `sites`.* It says what the locations are. `--refs` has
`references` beside it, which are sites too; `sites` would not say which.

**Reverses if** a caller needs "certainly absent" and "not seen" told apart by
the number alone. Then residue gets a code of its own above `2`, never `2`.
A new field that repeats a concept above under another name is the drift
this closes; name it from the table.

## DEC-081 — A call on `self` is a possible reference to a subclass's override

**Decided.** A call whose receiver is `self`, written or implicit, is typed as
the class it is written in. When `--refs` asks about a method whose owner
inherits from that class (or includes that module), and Ruby's lookup from the
class lands elsewhere or nowhere, the site is `possible` rather than excluded:
`self` runs as any subclass, and the subclass's method is the one that runs.
The template-method pattern, and a hook the base class never defines at all,
are the same shape.

```ruby
class Base; def run; setup; end; def setup; end; end
class Child < Base; def setup; end; end
```

**Before.** `Base#run`'s `setup` was excluded as a reference to `Child#setup`
(`different_owner`), and `--dead` called `Child#setup` `unreferenced` at clear
confidence. On rails that was each adapter's `configure_connection`, every
`validate_each` (22 validators), `private_url` in each storage service, and
337 candidates in all that are overrides a base or an included module calls on
`self`: `--dead .` moves them from `unreferenced`, `convention-only` or
`super-only` to referenced. Across the 40-method `--refs` differential, 29
sites move from excluded to possible and none from confirmed; each checked is
real dispatch (`Enumerable#index_by`'s `size` reaches `Array#size`,
`DatabaseStatements`' `execute` each adapter's).

**Not confirmed**, because which class runs depends on the object, and the
base's own method stays the confirmed landing. A call on an explicit instance
(`Base.new.setup`) is not `self` and is tiered as before.

*Reverses if:* `self`'s type is ever narrowed per call path (it is not: nothing
here follows a method to its callers), which would make "any subclass" too
wide.

**Amended: `--def` says so too.** A call on `self` whose method a subclass
overrides — or a module that a class inheriting the receiver mixes in ahead of
it — is answered `ambiguous`: the receiver's own method is still the answer
and its site, and each override is a named competitor ("a subclass overrides
it, and `self` may be one"), confidence one in the landings. It was
`resolved` at 1, and accord's `nested_schema` on a `Fields::Array` was
confidently wrong. The overrides are found from the name's definitions, not
the receiver's descendants: `ActiveRecord::Persistence` has thousands, and
the first cut, walking them, cost a cold `--def` on rails 0.4 s; this one
moves a warm `--def` there from 21 to 23 ms (median of 7). `create_or_update` in `Persistence#save` now names
`Timestamp#create_or_update`, which is what every model runs.

**Amended: a module's includer counts** (made for DEC-103). In a module, `self` is
whatever includes it, and a module often calls what it expects the includer
to get from another module: actionpack's `AbstractController::Caching::
Fragments` calls `instrument_name`, which `ActionController::Caching`
provides to the same controllers. The rule asked only whether the target's
owner had the calling module in its own chain, which held there by accident,
because the concern's `included do` include was read as the concern's own;
read as the includer's (DEC-103), three methods rails' controllers call this
way went `single-caller` → `unreferenced`. Such a site is now `possible` when some
class that has the calling module in its chain also has the target's owner
("`self` is what includes this module, and one that does has this"); a
target no includer has stays excluded. *Only ahead of the module's own
landing* (DEC-122).

Measured on its own, before DEC-103: rails `--refs`, 52 sites excluded →
possible across 11 of the 40 queries (28 of them `AbstractAdapter#execute`
from the adapters' `SchemaStatements` modules), none from or to confirmed.
`--dead` on rails 2,364 → 2,273 candidates, 69 of them `unreferenced`
dropping out or becoming `single-caller`; on the activerecord-only store 1,661
→ 1,597. Sampled, each is one adapter module calling what another module of
the same adapter defines (`write_query?`, `quoted_binary`,
`exec_rollback_to_savepoint`). No gold verdict or click moved.

## DEC-082 — A constant receiver is the class or module the constant names

**Decided.** `Foo.bar` types its receiver by resolving `Foo` and then
following what it is bound to: `Short = Router` makes `Short.go` a call on
Router, and a constant bound to anything that is not a namespace
(`NAMES = %w[a b]`, `ENV = …`) leaves the receiver untyped.

**Before.** The receiver was typed as a class named after the constant. For a
value that class defines nothing, so every call on it was excluded as
`no_such_method`; for an alias it hid the aliased module, so `Short.go` was
excluded from `Router.go`. On rails, across the 40-method `--refs`
differential, 149 sites move from excluded to possible and none from
confirmed: `Hash#fetch` on `DAYS_INTO_WEEK`, `HTTP_STATUS_CODES` and their
kind; `Array#size` on test fixtures' arrays; `Array#first` on `ARGV`.
`Array#join`'s `no_such_method` exclusions go from 24 to 14.

**The named cost.** 43 of those sites are `ENV.fetch`, which is not
`Hash#fetch`. Core writes `ENV = nil`, so the old answer excluded them for the
wrong reason, and they are now `possible`.

**Not done: typing a value by its literal.** It needs the extractor to record
the literal's class on the constant and the tree to carry it, and it is in
PLAN's backlog.

## DEC-083 — An editor miss is logged with its position and token, locally

**Decided.** When an LSP definition or hover comes back empty or unsure (the
`empty` and `uncertain` outcomes of DEC-063), the server writes one `miss`
line to `lsp.log`: file, 1-based line and byte column (pasteable into `--def`),
the token under the cursor, the outcome, and the engine's one-line reason
(`residue; receiver local_variable: …`, `no name at this position`).
`trekr --usage --misses` reads them back from the log's tail; `--json` gives
one object per miss.

**Why.** `--usage` said 30 of 72 definition requests in a day missed, and
nobody could say which: the counts keep nothing that names code (DEC-063), and
the log's `request` line had the file and line but not the column, the token
or the verdict. A miss rate without its positions cannot be fixed.

**Why the log, and not the usage file.** `lsp.log` already holds the file and
line of every request, stays on the machine, and is silenced by
`TREKR_LOG=off`; a miss line adds a column and a token to what it already
names. The usage file's promise — no code, no paths — stays intact. A ring
buffer of its own was considered and is not needed: `--misses` reads only the
last 8 MB of the log, which at a month of daily use is all of it.

**Off the hot path.** The handler notes its reason in a thread-local (one
`String` on a miss, nothing on a hit); the token is read, and the line written,
after the response is on the wire — as the usage count is.

**Also.** A hover on a residue or ambiguous call now counts as `uncertain`, as
a definition does. It counted `hit`, because the hover card is never empty,
which made hover look more certain than definition at the same position.

## DEC-084 — A block RSpec runs has an example group for its `self`

**Decided.** The extractor recognises RSpec's block DSL and says what each
block runs as. `describe`/`context`/`shared_examples` and their kin open an
example group — at `RSpec.describe` anywhere outside a method, at a bare
`describe` at the top of a file, and inside another group — and `it`,
`before`, `let`, `subject` and the rest open an example, a method body on an
instance of the group. A group is pushed onto the nesting as a segment of its
own, `(group AccordTypesDecimal)`, named as RSpec names the class. An
implicit call inside one is typed `RSpec::Core::ExampleGroup` — the class in
a group's body, an instance in an example — and looked up in whatever the
index holds of rspec-core, so `let`, `before`, `described_class` and
`is_expected` land on rspec-core's own definitions.

**Why a segment, and not a class.** A group is an anonymous class that only
its file can reopen, and RSpec's own names collide across files: 111 of
discourse's top-level descriptions are shared by two files or more ("Core
features" by 33). A class per group would need the file in its name, and
facts are a function of a blob's bytes, never its path. A segment needs no
name outside its file. The tree reads past it — a constant or class written
inside a group is the file's, as Ruby has it — so it changes nothing that
does not look for it.

**What was not modelled from the source.** Which block runs as what is
rspec-core's `module_exec` and `instance_exec`, behind
`define_example_group_method` and friends; no reading of the gem gets from
`describe` to "this block is a class body". The DSL's names are the one thing
stated here, as `belongs_to` and `attr_reader` are. Everything the block then
calls is found in the gem.

**Honest without rspec.** When the index holds no `RSpec::Core::ExampleGroup`,
the answer is residue saying so — "the call runs on an RSpec example group,
and rspec-core is not indexed" — rather than that the file does not
determine the receiver.

**The named cost.** A bare top-level `describe` is read as RSpec's. Minitest's
spec DSL is the same syntax, and rails writes it in four arel tests; with no
rspec-core indexed those answer residue as above, and a checkout bundling both
would type such a file's calls as RSpec's. Telling them apart needs the file's
name (RSpec loads `*_spec.rb`), which the blob layer does not have.

*Reverses if:* a checkout with both frameworks writes Minitest specs at the
top level often enough to show up in its gold set.

**Measured**, with DEC-085 to DEC-088 (BASELINE, "RSpec"): of 11,859 replayed
clicks in spec files, 67.0% missed and now 28.0%; library files 25.3% and
25.1%. A spec call site's gold answer is correct 77% of the time in
graph_weaver (was 12%), 56% in accord (1.5%) and 53% in polyid (7.2%), and
no gold set's confidently wrong count rose.

**A group's own methods.** `let(:x)`, `let!(:x)`, `subject(:x)`, `subject`
and a `def` in a group's body define a method on that group. They are facts
of the file alone: the store and the tree never see them, and a call in a
group asks its file for one before it asks rspec-core. A group sees its own
and those of the groups around it — a nested group is a subclass — and the
innermost definition wins, as the subclass's does; a sibling's is out of
reach, which is why a repeated description is numbered, `WhenValid_2`, as
RSpec numbers it. A `let` is a declaration (defined via `let`): the method
that runs is the one `let` generates in rspec-core, and the block is what it
calls. `subject(:x)` is written at the symbol, and its `subject` at the block,
so a click on the word `subject` still asks what the macro is. A `let` that a
shared context or an includer defines in another file is not found. Only
what the group's body writes is the group's: a `def` in a `Class.new` block
inside a `let` is that class's, and keeps the file's nesting as before.

**A block handed elsewhere.** A block keeps its caller's `self` unless the
method it is handed to runs it as another object, and trekr cannot see that
in general. Inside an example it vouches only for a block handed to RSpec's
own methods (found on the example group — `expect { }`, `travel_to`), to a
class core declares, with a method core defines or nobody indexed does
(`Dir.mktmpdir`), or to a method on a value (`items.each`); not for
`instance_eval` and its kin, `Class.new`/`Struct.new`/`Module.new`, a spec's
own helper, or a constant's method in the checkout or a gem. A call in one of
those is residue saying so. In graph_weaver, accord and polyid's gold sets the
example group had added exactly two confidently wrong answers, both of that
shape — `boolean` in `schema { boolean(:flag) }`, which the helper
`class_eval`s into a schema, and `output` in `GraphWeaver.graph :money do`, a
DSL's body — and the rule removes both. It costs one declaration, a `let`
called in a block handed to a spec's helper that yields, which is offered as
a candidate instead. Stubbing `Dir.mktmpdir` in core was tried and made four
calls to it confidently wrong: its source is the standard library's
`tmpdir.rb`, a real file the stub stood in front of.

## DEC-085 — A class method that hands its parameter to `define_method` is a macro

**Decided.** Inside a class method (`def self.m(name, …)`), a call that
passes the method's first parameter as the first argument of
`define_method`, `define_singleton_method`, or another such method of the
same scope makes `m` a macro. A later call `m :x` in that scope's body
declares a method `x`, written at the symbol, `defined_via` `m`.
`define_method` inside `(class << self; self; end).module_exec` or
`singleton_class.class_eval` defines a class method.

**Why.** rspec-core writes `it`, `describe` and `context` this way:
`define_example_method :it` calls `idempotently_define_singleton_method(name)`,
which calls `define_method(name, &definition)` on the singleton class. Nothing
read it, so `it` answered residue in every spec even once its block's `self`
was known (DEC-084): 622 of the 20,290 replayed clicks, `describe` and
`context` 272 more. Read this way, the answer is the line that names the
method, as for any macro, and a declaration, since the body that runs is the
macro's block.

**The limits, deliberately.** One file, and the macro defined before it is
called, which is Ruby's own order for a class body. A name the method builds
(`define_method("#{name}_label")`) is not its parameter and declares nothing:
a name half-guessed is worse than none. A macro inherited from a superclass
or a `ClassMethods` module is not followed, since its definition is another
blob's fact.

## DEC-086 — A block evaluated in a class defines that class's methods

**Decided.** The block of `X.class_eval`, `class_exec`, `module_eval` or
`module_exec` with no arguments is a body of `X`: a `def` in it is `X`'s
method and a mixin in it is `X`'s ancestor. `X` is the constant the call is
sent to, or, for a local that is a parameter of the enclosing method, the
constant that parameter defaults to (`def enable(host = ::Host);
host.module_exec do def greet`).

**Before.** The `def` was recorded on the scope around the block.
rspec-expectations defines `expect` as `syntax_host.module_exec do def expect
… end end` with `syntax_host=::RSpec::Matchers`, and rspec-mocks defines
`allow`, `receive` and `expect_any_instance_of` the same way: each landed on
the `Syntax` module that writes them, which no spec calls them on. Its
`minitest_integration.rb` has `Minitest::Test.class_eval do include
::RSpec::Matchers; def expect` at the top of the file: that `expect` was a
top-level method and the include an edge on nothing.

**The guess, named.** A parameter's default is what the block runs against
only when a caller passes nothing. rspec's own callers pass nothing; a
caller that passes another host defines the method there as well, and trekr
sees only the default. A bare, single-segment constant inside another scope
(`module A; String.class_eval do`) is left as it was, because only a lookup
can say whether it is `A::String` or `::String`, and the blob layer does not
look up.

## DEC-087 — RSpec's runtime wiring is a stub, read only when rspec-core is indexed

**Decided.** `src/tree/rspec.rb` states, as Ruby, the part of RSpec that is
built when a suite boots and that no reading of the gems can follow:

- `RSpec.describe`, `context`, `shared_examples` and their kin, which
  `RSpec::Core::DSL.expose_example_group_alias` makes with `define_method` on
  a name held in a variable;
- `ExampleGroup` including `RSpec::Core::MockingAdapters::RSpec` and then
  `RSpec::Matchers`, which `Configuration#configure_mock_framework` and
  `#configure_expectation_framework` do by `include`-ing a variable;
- the return types of `expect` (a `ValueExpectationTarget`, or a
  `BlockExpectationTarget` given a block) and `is_expected`, which
  `ExpectationTarget.for` decides at runtime and no signature states.

It is served beside core as `RSpec.rb`, and the tree reads it only when the
index declares `RSpec::Core::ExampleGroup`: without rspec-core it says
nothing, and DEC-084's residue says what is missing. It declares no class or
module, since every one it names is the gems' and a site in the stub would
be another place each is written; its edges come last, as RSpec's includes
run after the class body, and its methods sit after core's and before the
index's, so a method the gems define themselves — `expect`, since DEC-086 —
wins over the stub's. Its methods are declarations (`defined_via: rspec`):
RSpec makes them, and the stub only says so.

**A declaration's return type types the real method.** `expect`'s answer is
rspec-expectations' own `def`, which declares nothing, so the stub's `sig`
would never be read. A method with no return type now takes one from a
declaration of the same method on the same owner — the stub, or an `.rbi` —
as Sorbet reads an `.rbi`'s `sig` for the method it describes. That is what
types `expect(x).to` and `is_expected.to` as `ValueExpectationTarget#to`.

**Why a stub, not inference.** Each of these is a value flowing through a
variable into `define_method`, `include` or `new` — `ExpectationTarget.for`
returns one of two classes by an `if` — and following values is the thing
DEC-020 declined. Four facts, written down where anyone can read them, cost
less than an inference that would be right about these four and a guess
everywhere else.

**Before.** `RSpec.describe` found nothing on RSpec, fell through to Kernel,
and answered minitest's `Kernel#describe` (bundled through activesupport) at
confidence 1: the first line of every spec, and 7 of polyid's 13 confidently
wrong gold sites. `eq`, `be`, `raise_error`, `double` and `receive` were not
in ExampleGroup's ancestors, and `.to` after `expect` was untyped.

## DEC-088 — `RSpec.configure`'s `include` mixes a module into every example group

**Decided.** Inside `RSpec.configure do |config|`, `config.include X` is an
`include` edge on `RSpec::Core::ExampleGroup` and `config.extend X` an
`extend` one, so a spec's helper calls find `X`'s methods.

**Why.** It is how a suite brings in its helpers — FactoryBot's `create`,
`travel_to`, a gem's own `stub_*` — and the calls were residue once a spec's
`self` was known: "nothing indexed in its ancestors defines this name".
The source states the module and the receiver outright, so this is a reading,
not a guess.

**The over-reach, named.** A metadata filter (`config.include Helpers, type:
:model`) includes the module only into the groups that match, and the filter
is not read: every group is taken to have it. The runtime also includes it
into each group rather than into `ExampleGroup`, which matters only when two
modules define the same name.

**Honest without rspec, still.** The edge creates an `ExampleGroup` entry
even when rspec-core is not indexed, so DEC-084's "rspec-core is not
indexed" now asks whether anything *declares* the class, not whether the
name exists.

## DEC-089 — A rescued variable is an instance of what was rescued

**Decided.** `rescue WidgetError => e` is a write to `e` of a WidgetError,
typed `local:rescue`; a bare `rescue => e` of a StandardError. Several
classes rescued together are one write with a type each, so the answer is
`ambiguous` among them, confidence one in their number.

**Why.** The flow analysis already knew the reference as a write (DEC-064's
`bound by rescue`), and nothing gave it a type, so `e.message` and
`e.detail` answered that the receiver's type was not determined, in every
`rescue` clause. The class is written on the same line.

**Not done.** `rescue *ERRORS => e` names no class, so its write counts
against the answer rather than typing it.

## DEC-090 — A predicate matcher is the predicate it calls on the subject

**Decided.** An implicit `be_xxx` or `have_xxx` call in an example, for
which the group has no method of that name and whose ancestors hold
`RSpec::Matchers` with its `method_missing`, answers with the method RSpec's `method_missing` sends:
`be_empty` and `be_an_empty` → `empty?` (BePredicate), `have_key` →
`has_key?` (Has), and for `be_`, the present tense (`be_exist` → `exists?`)
when the subject has no `exist?`, as `BePredicate#predicate_method_name`
does. The receiver is the expectation's subject — `x` in `expect(x).to`,
`.not_to`, `.to_not`, or `x.should` — typed by the same ladder as any
receiver; `resolved_via` is `predicate_matcher`.

**Why.** It was residue in every spec, "nothing indexed in its ancestors
defines this name": 38 of the replayed spec clicks, and the answer a reader
wants is the predicate, since that is the code that runs and decides the
expectation.

**Honest where the source does not say.** `is_expected`, a bare `should`, and
a matcher not handed to an expectation (`all(be_empty)`, `.and be_empty`)
have a subject the file does not type, so the answer is residue whose reason
names the rule — "`be_empty` is RSpec's predicate matcher, which calls
`empty?` on the expectation's subject: …" — with the predicate's definitions
as candidates. A matcher RSpec, a gem or the spec defines (`be_within`,
`have_attributes`) is found first and is itself.

**Only where RSpec's `method_missing` answers.** ExampleGroup defines a
`method_missing` of its own, which comes first and hands every name that is
not a group method on with `super`; the rule asks for RSpec::Matchers' in
the chain, not the nearest — the first build asked for the nearest, and
fired in no real spec. Without RSpec::Matchers' in the chain —
a checkout that indexes no rspec-expectations — `be_nil` could be a real
matcher the index cannot see, and reading it as `nil?` would be a guess. The
matcher's call is resolve-time only: nothing is stored, so `--refs
Widget#empty?` does not count `be_empty`.

## DEC-091 — A custom matcher is a method its DSL call declares

**Decided.** `RSpec::Matchers.define :name`, `define_negated_matcher :name, …`
and `alias_matcher :name, …`, outside a method, declare an instance method
`name` on `RSpec::Matchers`, written at the symbol, `defined_via`
`RSpec::Matchers.define` (or the call used). The same calls — and `matcher`,
`define`'s alias — written bare in a group's body declare a method of that
group, as `let` does (DEC-084). The RSpec stub gains `ExampleGroup`'s `extend
RSpec::Matchers::DSL`, which rspec-expectations does with `RSpec.configure {
|c| c.extend self }`, so a click on `matcher` itself lands on the DSL.

**Why.** Each is `define_method(name)` in `RSpec::Matchers::DSL`, on the
module or group it is sent to, with the name a parameter — the macro shape
DEC-085 reads within one file, but here the call is in the spec's support
file and the definer in the gem. The receiver is written out, so this reads
the call rather than inferring anything. Before, a spec's own matchers were
residue, and a `have_error` or `be_a_twirp_response` was read as a predicate
matcher (DEC-090), which RSpec never reaches for a name that exists.

**The block is the matcher's.** `define :x do match { … } end` runs its
block as the body of a `RSpec::Matchers::DSL::Matcher`, so a call in it is
not the example's: a bare `define` or `matcher` joins `instance_eval` and
`Class.new` among the calls whose block DEC-084 does not vouch for. Found
before it shipped — with the stub's extend in place, `match` in such a block
answered RSpec::Matchers' `match` matcher, confidently wrong.

**Store v35**, since a support file's facts now hold the declaration.

## DEC-092 — A top-level shared group is a module its includers include

**Decided.** `shared_context "raw http server" do … end`,
`shared_examples` and `shared_examples_for`, with a literal name and written
outside any group (bare at the top of a file, or on `RSpec` anywhere outside
a method), make a module, `RSpec::SharedExampleGroups::RawHttpServer` — the
name as RSpec's `base_name_for` writes it, declared at the call. The `let`s,
`subject`s and `def`s its body writes are that module's methods, and stored.
`include_context "raw http server"` and `include_examples` in a group's body
include the module into that group; `it_behaves_like` and
`it_should_behave_like` into the nested group they make, which their block
customizes. A call in a group finds, innermost group first, the group's own
definitions (DEC-084), then the shared groups it includes, the last included
first. Inside the shared body, its own module comes last.

**Why a module, when DEC-084 turned down a class per group.** That was
because a group's name is not unique — 111 of discourse's top-level
descriptions are shared by two files — and facts cannot carry the file that
would tell them apart. A top-level shared group is different in kind: RSpec
registers it globally by its name, and an `include_context` elsewhere finds
it by that name alone, so the name is already the key the module needs. It
is also what RSpec does: `SharedExampleGroupModule` is a `Module`, and
`include_context` includes it. The include is file-local, like a group's
own methods, and is not stored.

**The limits.** A shared group written inside a group is scoped to it in
RSpec, and stays a group segment of that file, as before. A name that is not
a literal, or an include by metadata (`shared_context "x", :db` picked up by
`describe …, :db`), is not followed. Two names that `base_name` writes the
same (`"raw http server"` and `:raw_http_server`) are one module. A shared
group's body calling what its includer defines — `shared_examples` reading a
`let` it expects the includer to supply — is still residue, since which
includer is not the file's to say.

**Store** unchanged at v35, since DEC-091 moved it in the same release.

## DEC-093 — A symbol that names a method answers with the method

**Decided.** A bare symbol argument (DEC-037 already records each as a
`symbol`-shaped call) answers with a method in two cases, each a rule rather
than a reading of intent:

- **Reflective calls.** The first symbol of `send`, `public_send`, `__send__`,
  `method`, `public_method` and `respond_to?` names a method of their
  receiver — `self` when there is none — and the receiver is typed by the
  ordinary ladder: `widget.public_send(:x)`, `Widget.new.send(:x)`,
  `Widget.send(:x)` (a class method).
- **Class-level calls.** A symbol handed to an implicit-receiver call in a
  class or module body, outside any method and any block but a concern's
  `included`, names a method of the class's instances: `before_action :x`,
  `after_save :x`, `validate :x`, `alias_method :new, :old`, `private :x`,
  `helper_method :x`. `private_class_method` and `public_class_method` name
  class methods, and in `class << self` every one does.

`resolved_via` is `symbol`; a symbol whose method is not found is residue
whose reason says the symbol names a method of the receiver.

**Why generic, and not a list of Rails macros.** The same reason DEC-037
recorded every symbol: an app's own DSL is unknowable, and the answer is
checked — a symbol answers only when a method of that name is found in the
receiver's ancestors, so a symbol that names something else (`:string` in
`attribute :price, :string`) stays residue. The conservatism is in the
lookup, not in guessing which macros take method names.

**The exception, named.** `define_callbacks`, `define_model_callbacks`,
`set_callback` and `skip_callback` take the name of a callback *chain*, and
`define_model_callbacks :save` sits in the same class as `def save`: reading
the symbol as that method would be confidently wrong about a real class. They
are excluded. Anything else whose symbol names a chain, a state or a route
that happens to share a method's name is the rule's known cost.

**Not done.** A symbol in a hash value (`if: :ready?`, `with: :handler`) or
an array (`only: [:show]`) is still not recorded (DEC-037). `&:sym` is
DEC-094. Nothing new is stored, so `--refs` still counts a symbol as a
`possible` reference.

## DEC-094 — `&:name` is a call of `name` on each element

**Decided.** A block argument that is a literal symbol, `items.map(&:name)`,
is recorded as a call named `name` at the symbol, shaped `symbol` like the
arguments DEC-037 records, standing for a call on each element the block is
handed. The element's class is known when the receiver is a literal array
whose elements are all literals of one class; otherwise the answer is
residue — "the symbol names a method of what it is sent to: the receiver's
type is not determined" — with the name's definitions as candidates.

**Why.** Nothing recorded it, so a click on `empty?` in
`name.split("_").reject(&:empty?)` found no name at all, and `--refs` and
`--dead` never saw the call: 197 of them in the lib directories of the 13
dogfood repositories.
Recording it as a symbol, not an ordinary call, keeps it a `possible`
reference, which is what it is while the elements are untyped.

**Not done.** Element types. `String#split` returns an `Array` in core's
stubs, not an `Array[String]`, and no rung carries what a collection holds,
so `split(…).reject(&:empty?)` stays residue. That is the lead: a typed
element would answer this and every block parameter of `each` and `map`
with it.

## DEC-095 — A file that writes Minitest's expectations is not RSpec's

**Decided.** A bare `describe` at the top of a file opens no RSpec group
(DEC-084) when the file's bytes contain `.must_`, `.wont_`, `minitest/spec`
or `Minitest::Spec`. `RSpec.describe` is RSpec's wherever it is written.

**Why this tell.** DEC-084 named the cost: Minitest's spec DSL is RSpec's
syntax, and rails writes it at the top of four arel tests. With rspec-core in
the bundle, their `it` answered rspec-core's `it` at confidence 1. The file's
name would tell them apart and the blob layer does not have it, and neither
do the files' requires — rails' say only `require_relative "../helper"`. What
they do write is Minitest's expectations, `_(dolly.wheres).must_equal …`,
which no RSpec spec calls: Minitest defines `must_*` and `wont_*` on every
object, RSpec defines neither.

**A byte search, deliberately.** The tell is read before the visit, since the
group opens at the file's first line and the expectations come later; a
second parse to find them would cost more than the four files are worth. A
comment or string that happens to contain `.must_` turns an RSpec file's bare
`describe` off, which leaves it residue, as before DEC-084 — not wrong.
*Superseded by DEC-123*: the tell is read from the code.

## DEC-096 — A `let` is typed by what its block returns

**Decided.** A `let`, `let!`, `subject` or `subject!` whose block ends in an
expression of a shape an assignment is typed from (`X.new`, a constant, a
literal, a call whose `sig` names its return, a finder) types the calls made
on it, as `x = X.new` types `x`: `widget.save`, `held = widget; held.save`,
and the subject of `expect(widget)`. `described_class` in the block is the
constant the innermost group that names one describes, so
`described_class.new` is an instance of it. `is_expected` and a bare `should`
expect the `subject`. The rung is `let`.

**Why.** A spec's receivers are its `let`s: of the 2,676 definition clicks
in spec files that still missed after DEC-090 to DEC-093, 575 are calls on a
`let` or `subject` by name. And a predicate matcher (DEC-090) can say which
predicate runs only when its subject has a type, which in a spec is mostly
a `let`'s.

**Nested groups override a `let`, and a hook sees the override.** A `before`,
another `let`, or a method in a group runs in every group nested in it, and
there `widget` is the nested group's `let` when it has one. So in those, each
nested override is a competing write: the answer is `ambiguous`, confidence
one in their number, naming the other types. An example's own block (`it`)
runs only in its group, and is exempt. Only a hook's call is checked this
way; `held = widget` in a hook reads the innermost `let` alone.

**Not done.** The implicit subject — `described_class.new` when no `subject`
is written — is not modelled, so `is_expected` without a `subject` stays
untyped (done since, DEC-114). A `let` that a shared group defines is stored without its value, so
a call on it from an includer is untyped. FactoryBot's `create(:widget)` names
no class.

## DEC-097 — A mixin sent to a constant is an edge, when it runs as its file loads

**Decided.** `X.include(M)`, `X.prepend(M)`, `X.extend(M)`, and `X.send(:include,
M)` (or `__send__`) are ancestry edges on `X`, as the same line in `X`'s body
would be. `send(:include, M)` on `self` is `include M`. `self` as the argument,
in a module's body, is the module (`Object.prepend(self)`). `X` is a constant
as written, looked up by the tree where the call is written: the lexical
scopes, then the top level. A receiver the tree does not hold has no chain for
the edge to join, and the edge is dropped rather than inventing one.

**Why.** It is how a gem patches another gem's class and how Rails' own core
extensions are applied: `Range.prepend(ActiveSupport::CompareWithRange)`,
`Integer.include(ActiveSupport::NumericWithFormat)`, polyid's
`ActiveRecord::Relation.prepend(PolyId::Relation)`. Until now each was an
ordinary call, so the patched method was invisible from the class, a `super`
in the patch had no class to land in (DEC-068's amendment), and the patch's
methods were `--dead` candidates.

**Only what runs when its file loads.** A sent mixin under a conditional
(`if`, `unless`, `case`, `&&`, `||`), inside a method, or inside a block
handed to a call is not recorded. The first cut recorded every one, and accord's gold set lost four correct answers
to it: sorbet-runtime writes `if defined?(::RSpec::Core::MemoizedHelpers::
ClassMethods) … ::RSpec::…::ClassMethods.prepend(MemoizedHelpers)`, which
runs only when rspec-core loaded first, and in accord's suite it had not, so
`let` went to sorbet's wrapper where Ruby ran rspec-core's. widget_shop's lost
two to activerecord's encryption `install_support`, a method that includes its
query overrides into every model only when deterministic encryption is
configured. A block runs when its receiver decides, after whatever ran
first; a Railtie's `initializer do` runs at boot, but widget_shop's trace
had a mailer call `action_methods` before the initializer's `on_load` included
`AbstractController::UrlFor` (DEC-098). Only a block that says what it runs
as and when — a class body's `class_eval`, an `on_load` hook as its class
loads — counts as loading. Body mixins (`include M if cond`) keep being
recorded as before: they are rare, and the rule is about the shape optional
integrations are written in.

**Why look the receiver up rather than place it** as DEC-086 places
`X.class_eval do`. Placing is what a declaration does, and the receiver is a
reference: `Cart.include(Pricing)` inside `module Shop` means `Shop::Cart` only
if that exists, and `::Cart` otherwise. The lookup reads lexical scopes and
the top level only, since no chain is complete while edges are being
attached; a receiver that is found only through an ancestor is missed.

**Measured** (BASELINE, "Mixins sent at runtime"): no gold set's confidently
wrong count moved; accord's gem floor gained a correct answer and a `super`;
the click replay's "module never mixed in" bucket went 151 → 138; the rails
`--refs` differential moved one site (a `super` in `BigDecimal`'s patch now
lands on `BigDecimal#to_s`, excluded from `TimeWithZone#to_s`); rails
`--dead` moved six tiers, each toward referenced.

**Not done.** `X.singleton_class.prepend(M)` (network_resiliency's adapters),
a receiver held in a variable (`base.extend(ClassMethods)` in a
`self.included` hook, `[Hash, Array].each { |k| k.prepend(M) }`).

## DEC-098 — An `on_load` block mixes into the classes that run its hook

**Decided.** `ActiveSupport.run_load_hooks(:name, Base)` is an edge of
relation `load_hooks`: `Base` runs `name`'s hooks. `Base` is a constant, looked
up as a sent mixin's receiver is (DEC-097), or `self` in a class body. A mixin
in `ActiveSupport.on_load(:name) { … }` — `include`, `extend`, `prepend`,
bare or on `self`, and mixins sent from the block — has an owner starting
`(on_load name)`, and the tree attaches it to every class that runs `name`.
The map from hook to class is read from the index, not a table:
`:action_controller` is `ActionController::Base` and `ActionController::API`
because both write `run_load_hooks(:action_controller, self)`, and a gem's own
hook works the same way. A hook no indexed class runs attaches nothing.

**Why.** It is the one way Rails lets a gem reach a framework class it must
not load early, so it is how most model extensions arrive: polyid's
`ActiveSupport.on_load(:active_record) { include PolyId::Model }`. Its
`User.id_for` found nothing (25 empty clicks in the replay), and `User.find`
and `User.find_by` went to ActiveRecord's own at confidence 1 instead of
the override — seven of polyid's confidently wrong gold answers, and the
whole of that number.

**Ordered after the class body.** The hook runs when the class's file
finishes, and a mixin sent to a class runs once the class exists. Both are
attached after every body's edges, whichever file sorts first, because the
order of a class's mixins is the order of its ancestors: polyid's
`ClassMethods#find` has to come before `ActiveRecord::Core::ClassMethods#find`
in `User`'s singleton chain, and `lib/polyid.rb` sorts before
`lib/active_record/base.rb` in no store at all.

**Only a hook registered as its file loads.** An `on_load` inside a block, a
method or a conditional runs when that code does. widget_shop's trace shows
the cost of ignoring that: actionmailer's Railtie registers `on_load(:action_
mailer) { include AbstractController::UrlFor }` inside an `initializer`, and
a mailer called `action_methods` before it ran, so recording it made one
correct gold answer confidently wrong. Such a block is still known to be a
hook, and its mixins are no longer credited to the class it is written in, as
every block's are: activestorage's Engine used to include
`ActiveStorage::Attached::Model` that way.

**Why an edge, not a table of its own.** DEC-002 keeps one table for "this
scope gains that ancestor", and a hook is what decides which scope gains it.
The row needs a scope, a name and a place, which is an edge's shape; the
relation says it is not an ancestor itself, and the tree consumes it before
attaching the rest.

**With `yield: true`** the block is called with the class as its argument
instead of evaluated in it, so its `self` is the caller's and it is not read
as a hook.

**Not done.** A `def` in an `on_load` block (it stays on the lexical scope,
as in any block), `on_load(:x) { |base| base.include M }`, and a call's
receiver inside the block, which is still the block's residue.

**Measured** (BASELINE, "Runtime ancestry"): polyid's gold set, spec sites
confidently wrong 7 → 0, correct 281 → 302; library sites correct 29 → 35.
No other gold set moved. Clicks: definition misses 5,940 → 5,890 ("known
type, method not found" 284 → 256, "module never mixed in" 138 → 116). rails
`--refs`: one site excluded → possible, activestorage's `reload`, whose
module no longer has the Engine as a false mixer. `--dead` on rails: one
candidate fewer; on the activerecord-only store, none moved.

## DEC-099 — A concern's `included do` extends and defines on the includer

**Decided.** Inside `included do … end` of a module that extends
`ActiveSupport::Concern`, `extend M` is an `include M` on the concern's
`ClassMethods`, and `def self.x` (or a `def` in `class << self`) is
`ClassMethods#x`. When the concern writes no `ClassMethods`, one is declared
at the block's `do`, `defined_via: included`, as DEC-033's routing of
class-level macros already does.

**Why.** The block is `class_eval`'d into each includer, so its `extend` and
its singleton `def`s land on the includer's singleton class, which is where
Concern puts `ClassMethods`. The extractor read them as the concern's own:
ActiveModel::API's `extend ActiveModel::Naming` and ActiveRecord::Core's
`def self.strict_loading_violation!` reached no model. Routing through
`ClassMethods` says the same thing with machinery the tree already has.

**Declared at `do`.** The first cut declared the module at `included`, like
the macro routing, and a click on `included` then answered `ClassMethods`:
two of widget_shop's gold sites (the `included` in activesupport's
`callbacks.rb` and actionpack's `caching.rb`) went correct → column-mismatch.
`do` is no name a click lands on.

**Not done.** `include` and `prepend` in the block still go to the concern
itself: `include` differs from Ruby only in where the module sits relative to
the concern, and `prepend` on the includer has no edge that says "ahead of
whoever includes me". Neither was measured to matter; a `prepend` in an
`included` block was not found in rails, discourse, mastodon or the installed
gems. The classic `def self.included(base); base.extend(ClassMethods); end`
is a receiver in a variable and stays unread (DEC-097).

**Measured** (BASELINE, "Runtime ancestry"): widget_shop's gem floor, correct
1,521 → 1,525 and confidently wrong 21 → 19 (`configurations` and
`preventing_writes?`, both `def self.` in an `included` block). No other gold
set, `--refs` query or click moved. rails `--dead` renames five candidates'
owners to `ActiveRecord::Core::ClassMethods`, two of them unreferenced →
single-caller.

## DEC-110 — `enum` declares every method ActiveRecord::Enum generates

**Decided.** An `enum` declares, at the attribute: its reader, typed
`String`, its writer, and the mapping's class method (`statuses`); and at
each member: `x?`, `x!`, and the scopes `x` and `not_x`. A member's name is
ActiveRecord::Enum's own rule, `"#{prefix}#{label}#{suffix}"` with a run of
ASCII punctuation or space in the label made `_`: `prefix: true` takes the
attribute's name, a symbol or string is used as written, `false` is none.
`scopes: false` drops the scopes, `instance_methods: false` the predicates
and bangs. Both spellings are read: Rails 7's `enum :status, {…}, prefix:
true` (or `enum :status, active: 0, prefix: true`, whose members are the
keywords that are not options), and Rails 6's `enum status: {…}, kind: […],
_prefix: true`, where each keyword but the options is an enum and the options
apply to all of them. Each is `defined_via: enum`, a declaration.

**Why.** Before, a `prefix:` or `suffix:` refused every member, although the
name it makes is a rule; `not_x` was not declared; and a Rails 6 call
declaring two enums read only the first. The attribute itself was the
schema's, where there was a schema, and typed from the column: `role.upcase`
was sent to `Integer`, where Rails' enum reader returns the member's name.

**A model's declaration wins over the schema's.** The schema declares an
attribute on the model by convention (DEC-022), and the model's `enum` (or
`attribute`) redefines it, as Rails' attribute API does. When one owner holds
both, the lookup takes the model's. Before, whichever file sorted last won,
and `db/` sorts after `app/`.

**The limits.** A computed affix spells nothing, so its members are left out
rather than guessed. Members computed at runtime (`enum status:
Status.values`) declare only the attribute's three methods, as before.

## DEC-111 — The common method-making Rails macros are declarations, picked by count

**Decided.** The macro table (`src/extract/macros.rs`) gains the macros the
dogfood corpora write most that it did not know, and fixes two it misread:

- `has_secure_password [:attr]` (default `:password`): `attr`, `attr=`,
  `attr_confirmation`, `attr_challenge` and their writers,
  `authenticate_attr`, `attr_salt`, `authenticate` when the attribute is
  `password`, and the reset token's `attr_reset_token`,
  `attr_reset_token_expires_in` and class methods `find_by_attr_reset_token`
  and `!` — unless `reset_token: false`. With no argument written, the
  methods sit just past the macro's name, so a click on the name still
  answers the macro.
- `has_secure_token [:attr]` (default `:token`): `regenerate_attr`.
- `has_one_attached :x`: `x`, returning `ActiveStorage::Attached::One`, `x=`,
  `x_attachment`, `x_blob` and their writers, class method
  `with_attached_x`; `has_many_attached` the same in the plural, returning
  `ActiveStorage::Attached::Many`.
- `accepts_nested_attributes_for :x`: `x_attributes=`.
- `store :s, accessors: [...]` and `store_accessor :s, *keys`: each key's
  accessor pair and the six dirty methods ActiveRecord::Store writes for it;
  `prefix:`/`suffix:` rename by Rails' rule. `store_accessor` used to take `s`
  itself for a key.
- `alias_attribute :new, :old` declares `new` only (it declared `old` too),
  and with `attribute :x` gains `x?` and the dirty methods below.
- `class_attribute` and the `mattr`/`cattr` family read their
  `instance_accessor:`, `instance_reader:`, `instance_writer:` and (for
  `class_attribute`) `instance_predicate:` options, and `thread_mattr_*` and
  `thread_cattr_*` are the `mattr` family's.
- `belongs_to :x` adds `x_changed?` and `x_previously_changed?`
  (ActiveRecord 7.1), and it and `has_one` add `reset_x`.
- A schema column (DEC-022), an `attribute` and an `alias_attribute` gain
  six dirty-tracking methods: `x_changed?`, `x_was`,
  `x_previously_changed?`, `x_before_last_save`, `saved_change_to_x?` and
  `will_save_change_to_x?`.

**Why these, by count.** Declarations in discourse, mastodon and rails:
`scope` 1,139, `attribute` 965, `enum` 191, `class_attribute` 168,
`cattr_accessor`/`mattr_accessor` 176, `store`/`store_accessor` 88,
`accepts_nested_attributes_for` 51, `alias_attribute` 52,
`has_one_attached`/`has_many_attached` 33, `delegate_missing_to` 10,
`has_secure_token` 9, `has_secure_password` 5. `scope`, `attribute`,
`class_attribute` and the `mattr` family were already in the table.
`normalizes` and `generates_token_for` make no method a caller names.

**Dirty tracking, reversing DEC-022's cutoff.** DEC-022 left the family out
as "a dozen names per column for a fraction of the calls", and DEC-022's
revisit counted 50 of 270 declined attribute sites as dirty tracking. Counted
in discourse's and mastodon's `app` and `lib`: `x_changed?` 107,
`saved_change_to_x?` 79, `will_save_change_to_x?` 61, `x_previously_changed?`
15, `x_was` 10, `x_before_last_save` 7; `x_change`, `x_in_database`,
`x_change_to_be_saved`, `saved_change_to_x` and `restore_x!` none at all. The
six that code calls are declared, the rest not (a store accessor keeps the
six Rails writes for it by hand, `x_change` among them). The rails store grows by
13,614 definition rows (88,292 → 101,906) and 2.7% on disk (65.3 → 67.1 MB).

**Not done.** The dirty methods' owner is the model, where Rails puts them in
`GeneratedAttributeMethods`, a module included when the class is made: a
concern the model includes later that overrides `x_changed?` wins at runtime
and loses here, as it already did for a column's reader. `store`'s own
attribute (`settings`) is the column's, left to the schema.

## DEC-112 — `delegate_missing_to` is followed when the lookup fails

**Decided.** `delegate_missing_to :target` in a class body declares
`method_missing` and `respond_to_missing?` (`defined_via:
delegate_missing_to`), carrying the target's name. When Ruby's lookup on a
typed receiver finds nothing and the first `method_missing` in its chain is
one of these, the call is looked up on the class the target's reader
returns — a `belongs_to`'s, a `sig`'d method's — and answers with that
method, `resolved_via: delegate_missing_to`, as sure as the receiver was.
`--refs` tiers such a site against that answer: confirmed when it lands on
the queried method, excluded (different owner) when it lands elsewhere.

**Honest where the target is not typed.** An `attr_reader` target — the usual
presenter, `delegate_missing_to :account; attr_reader :account` — has no type,
and the answer is residue saying so: "Presenter hands a name it lacks to
`account` (delegate_missing_to), whose type is not determined", with the
name's definitions as candidates; `--refs` counts it possible. A typed target
that lacks the name too is residue naming the type. Before, all three were
"nothing indexed in its ancestors defines this name", and `--refs` excluded
them as `no_such_method` — a claim the class's own `method_missing`
contradicts.

**Only this `method_missing`.** Any other in the chain could also answer a
name, and `--refs` still excludes a call on a class whose `method_missing` is
hand-written: ActiveModel::AttributeMethods defines one, so every model
would turn each `no_such_method` exclusion into `possible`, for a
`method_missing` that answers attribute patterns alone. That is DEC-021's
known weakness, and not this lane's to reopen.

## DEC-113 — A helper's block may call the helper's own group

**Decided.** DEC-084 vouches for a block in an example only when the method
it is handed to is RSpec's, core's or a value's, since a spec's own helper
may run it as another object. One call in such a block is now admitted: a
call whose name is a method of the same group the helper is — both of one
shared group's module, or both written in one group's body in the file —
when the helper's own call runs on the example and is not one of Ruby's
evaluating methods (`instance_eval`, `Class.new` and kin). It answers as a
call on the example does, with the group's method.

**Why this is sound enough.** If the helper did run the block as some
object O, the call would be sent to O, and the spec passes, so O answers the
name. The only definition the index holds of that name, in reach, is the
group's own; O answering it by other means — a DSL's `method_missing`, an
unindexed class that happens to share the helper group's name — is the
coincidence this rule bets against. The bet is narrow on purpose: a `let` of
the includer, an RSpec matcher or anything else in the block is still not
vouched for, because nothing ties it to the helper. DEC-084's two
confidently wrong answers do not qualify: `boolean` in `schema { … }` is
Accord's DSL, not a sibling of `schema`, and `output` in `GraphWeaver.graph`
is a constant's method.

graph_weaver's `serving { |socket| socket.write(http_response(500, "…")) }`
is the case: both are methods of the shared context `"raw http server"`, and
`serving` hands its block to a thread that calls it, so `http_response` runs
on the example. testbed 053's `assemble { part }` now resolves as well:
`assemble` does `instance_eval` — on the example itself, so `part` is the
example's there too.

**`--refs` asks the group first.** A call site in a spec that names a group
member — a `let`, a group's `def`, a shared group's method — is tiered by
that member, as `--def` answers it: confirmed when it is the queried method,
excluded (different owner) when the group defines its own. Before, the
receiver was typed `ExampleGroup`, which does not define the name, and a
shared group's callers were `possible` at best.

**Not done.** Reading the helper's body to see whether it evaluates its
block, which would admit every call in the block, not just its siblings.
The helper is usually another file's (a `spec/support` shared context), so
that is a stored fact per method, and the shared-group case did not need it.

## DEC-114 — The implicit subject is the described class's instance

**Decided.** When no group in reach writes a `subject` — neither the call's
group nor one around it, nor a shared group it includes — `subject`, and so
`is_expected` and a bare `should`, is typed as RSpec's
`MemoizedHelpers#subject` makes it: an instance of the class the innermost
group describes (`described_class.new`), or the module itself when a module
is described. The rung is `implicit_subject`. A group that describes a
string inherits its parent's class, as `described_class` does; a spec that
describes no constant at all has a string for a subject, which is not
modelled and stays untyped.

**Why.** DEC-096 left it as the named gap: `it { is_expected.to be_valid }`
under `describe Widget do` is the idiom RSpec's own documentation leads with,
and its predicate matcher could not say which predicate ran. The class is
written on the group's first line; nothing is inferred.

**Which group describes it** is a resolve-time fact of the file (each group
that describes a constant, by its nesting), like the shared groups a group
includes: never stored.

## DEC-115 — A bare top-level `describe` is sent from `main` to `RSpec`

**Decided.** A group method called bare at the top of a spec — `describe`,
`context`, `shared_examples`, `shared_context` and their kin, where DEC-084
opens a group (so not in a Minitest spec, DEC-095) — has `main` for its
receiver, and `main`'s method sends the call on to `RSpec`: the answer is
the stub's `RSpec.describe`, `resolved_via: main`, a declaration by RSpec.
Only when the index knows `RSpec`; otherwise residue as before.

**Why.** rspec-core's `expose_dsl_globally`, on unless a suite turns it off,
defines each group method on `main`'s singleton class and on `Module` as
`::RSpec.__send__(name, …)`. accord writes every spec that way, so the first
line of each answered residue ("the receiver's type is not determined by
this file"). The call is the same one `RSpec.describe` makes, one hop later,
so it answers with the same line; `main` itself is not modelled — a top-level
call is still untyped in general (`require_relative`, DOGFOOD).

**A shared group's module moves to its block's opening.** DEC-092 declared
`RSpec::SharedExampleGroups::X` at the call, so a click on `shared_examples`
answered the module — and, being no constant written there, "no indexed
constant by that name". It is declared at the `do`, as DEC-099 put a
concern's `ClassMethods`, and the call answers as itself.

## DEC-116 — A `scope`'s body runs on the model's relation

**Decided.** The lambda (`->`, `lambda`, `proc`) a `scope` is given is its
body, and ActiveRecord runs it with `instance_exec` on the model's relation.
An implicit call in it has `ActiveRecord::Relation` for its receiver,
`resolved_via: scope`, when the index knows that class. A name the relation
does not define goes to the model's class methods — another scope, a `def
self.` — as `ActiveRecord::Delegation` hands it there, answered with the
model's method. `--refs` tiers such calls by the same answers.

**Why.** It was typed as the class body around it, where `where` and
`order` are `ActiveRecord::Querying`'s one-line delegations to `all`: a
confident answer that is not the code that runs. That was the only kind of
confidently wrong answer in widget_shop's app code besides a mailer stub,
and both in the macro fixture's (`order`, `where`).

**Measured.** widget_shop's app code, confidently wrong 2 → 1
(`where` in `scope :affordable`, now correct); the macro fixture's 2 → 0
(`order`, `where`). rails `--refs`: 30 `where` calls in scope bodies move
from `ActiveRecord::Querying#where` (confirmed → excluded) to
`ActiveRecord::QueryMethods#where` (excluded → confirmed); nothing else
moved, and no click moved.

**Not done.** What a scope returns is not typed: a `Relation` receiver would
send `Widget.active.recent` to `Relation`, which lacks `recent` — it is
`Widget`'s, reached through the relation's delegation, and the relation's
type does not say which model it is for. Rails names that class
(`Widget::ActiveRecord_Relation`) only at runtime.

## DEC-100 — A loop over a literal list of classes sends its mixin to each

**Decided.** In `[Hash, Array].each do |klass| … end` (or `reverse_each`),
the block parameter is each of the list's constants in turn, and a mixin sent
to it — `klass.include(M)`, `klass.send(:prepend, M)` — is one edge per
constant, looked up where the call is written as DEC-097's are. A constant
this file assigns a literal list (`KINDS = [Symbol, Float].freeze`) iterates
the same way. Every element must be a constant as written, or the list is not
read at all: half a list looks like a whole one. The iteration's block runs
as its file loads, so it is not a block handed to an arbitrary call; a loop
inside a method or a mixin under a conditional is still not recorded.

**Why.** ActiveSupport's `core_ext/object/json.rb` ends with `[Enumerable,
Object, Array, FalseClass, Float, Hash, Integer, NilClass, String,
TrueClass].reverse_each do |klass| klass.include(ActiveSupport::
ToJsonWithActiveSupportEncoder) end`. That is how every Rails app's
`{ … }.to_json` runs ActiveSupport's encoder, and DEC-097 read none of it: the
block is handed to a call. DEC-077 named the cost when literals were typed —
discourse's `{ … }.to_json` resolved to the json gem's `GeneratorMethods`,
confidently. On rails, where the json gem's methods are not indexed on Hash,
the same site was residue.

**Not done.** `%w[Hash Array]` holds strings, which become classes only
through `const_get` or `constantize`; no such loop sends a mixin in rails or
the installed gems, and it is not read. Neither is a list another file
assigns, which is that blob's fact, nor a list built by a call
(`descendants.each`), whose elements are not stated anywhere.

**Measured** (BASELINE, "Mixins through a variable"): discourse's
`{ … }.to_json` in `post_alerter.rb` went from `JSON::GeneratorMethods`,
resolved at 1.0, to ActiveSupport's encoder; rails' in
`request_forgery_protection.rb` from residue to the same. No gold verdict,
`--refs` tier, `--dead` tier or replayed click moved: the loop is the only
one of its kind in either corpus.

## DEC-101 — A mixin into a singleton class is the class's own

**Decided.** `include M` inside `class << self` or `class << X`, and
`singleton_class.include(M)` sent to `self` or a constant, are `extend M` on
that class. `prepend M` in the same places is a new relation,
`singleton_prepend`: the tree walks `M`'s chain ahead of the class's own
singleton methods, the last prepended first, then the class's `def self.`s,
then what it extends. An `extend` there reaches the singleton class's own
singleton, which nothing asks for, and is not recorded. A sent one keeps
DEC-097's load-time rule.

**Why.** It is how a gem wraps another gem's class methods:
network_resiliency's adapters `singleton_class.prepend` their instrumentation,
and rails writes `singleton_class.prepend` or `.include` 23 times.
`X.singleton_class.prepend(M)` was an ordinary call, so `X.connect` answered
`X`'s own method, confidently, where Ruby runs the wrapper. Worse,
`class << self; include Naming; end` was read as an `include` on the class
itself: its methods were found on instances, which have none of them.

**Not stored as extends.** An extended module sits behind the class's own
singleton methods, and a prepended one ahead of them, so the difference is
the answer. Both are kept in the snapshot's extends section with a kind
beside each target, as mixins keep prepend and include (snapshot format 2).

**Not done.** A `super` in a module prepended to a singleton class is still
residue — "no class the index knows mixes it in" — because DEC-068's mixers
are the classes whose *instances* have the module. `X.singleton_class.
extend(M)`, and a singleton class held in a variable, are not read.

**Measured** (BASELINE, "Mixins through a variable"): no gold verdict moved.
One click replayed was fixed, berater's `Berater.test_mode`, which
`Berater.singleton_class.prepend Berater::TestMode` defines; one rails
`--refs` site moved excluded → confirmed, `ActiveJob::Callbacks.
run_callbacks`, which the module has through `class << self; include
ActiveSupport::Callbacks`. `--dead` did not move.

## DEC-102 — A module's `included` hook mixes into its includer

**Decided.** In `def self.included(base)` of a module — or `extended`,
`prepended`, and the same written in `class << self` — a mixin sent to
`base` (`base.extend(M)`, `base.include(M)`, `base.send(:include, M)`,
`base.singleton_class.prepend(M)`) is an edge on whatever mixes the module in
that way, and `base.class_eval do … end` is a body of it: its `include`,
`prepend` and `extend` are that scope's, and its `def self.x` and class-level
macros go to the module's `ClassMethods`, as DEC-099 routes a concern's, with
a `ClassMethods` declared at the `do` when the module writes none. The owner
is a `(mixed include)` segment before the module's nesting, and the tree,
once every body's edges are attached, gives each scope whose own mixins name
the module the hook's edges: an `include` inserted right after the module,
where Ruby puts it, an `extend` among the scope's extends. What a hook adds
can have a hook of its own; each scope applies each module's once.

**Why.** It is the classic idiom, from before ActiveSupport::Concern and
everywhere in older gems: 715 `def self.included(` across the installed gems,
242 `base.extend(`, 218 `base.class_eval`. `Widget.track` found nothing on a
class whose module gave it `ClassMethods` this way, and a `super` in a module
the hook includes had no class to land in.

**Load time, by definition.** DEC-097 records nothing inside a method, and
the hook is the one method whose body runs exactly when its module is mixed
in. Its sends count; one under a conditional (`base.extend(X) if
base.respond_to?(:x)`), or in a block other than `base.class_eval`, does
not.

**Only the direct mixer.** Ruby calls the hook with the scope that wrote
`include Tracking`. When that is a module, the module gains the hook's
includes, and so do its own includers through its chain; the hook's
`extend`s reach the module's singleton and stop there. That is what the tree
does. A concern is the exception Concern makes it — its `ClassMethods` reach
the final includer through dependencies — and keeps DEC-099's routing.

**The approximation.** A plain `def x` in `base.class_eval` stays on the
module, which sits right behind the includer in its chain: the site is the
one that runs, the owner is one step off, and an includer's own `x` wins
where Ruby's redefinition would lose.

**Measured** (BASELINE, "Mixins through a variable"): polyid's gem floor,
confidently wrong 8 → 5 and correct 143 → 146 (rspec-core's `attr_accessor`
in `Example`); no other gold verdict moved. Clicks: definition misses 5,637 →
5,586, 51 of them `expect` and `is_expected` in rspec-twirp's specs. rails
`--refs ActiveSupport::Callbacks#run_callbacks`: seven sites excluded →
confirmed, in classes that `extend ActiveModel::Callbacks`, whose `extended`
hook `class_eval`s `include ActiveSupport::Callbacks` into them. `--dead`
did not move.

## DEC-103 — A concern's `included do` includes into the includer

**Decided.** `include M` and `prepend M` inside `included do … end` of a
module that extends `ActiveSupport::Concern` are `(mixed include)` edges
(DEC-102): each class whose own `include` names the concern gets them, an
`include` right after the concern and a `prepend` ahead of the class. The
concern's `extend` keeps DEC-099's route through `ClassMethods`.

**Why.** DEC-099 left both on the concern itself, measured to matter
nowhere. For `include` that is close: the module sits behind the concern
instead of ahead of it. For `prepend` it is the opposite of Ruby: the module
sat behind the includer, so the includer's own method beat the one Ruby
puts in front of it, and a `super` in the prepended module answered the
concern's method where an includer runs its own. DEC-102's edges give both
their real place for nothing further.

**A concern that includes the concern** gets the edges itself, since its
`include` names the concern, and passes them on to its own includer through
its chain, which is where Concern's dependencies put them. The order differs
from Ruby's in one way: a module the inner concern prepends sits ahead of the
outer concern rather than ahead of the final class, so a method the class
defines itself would win where Ruby's prepend beats it.

**Measured** (BASELINE, "Mixins through a variable"): no gold verdict,
`--refs` tier or click moved. rails `--dead` loses one candidate,
`PrimaryKey::ClassMethods#dangerous_attribute_method?`: `included do include
PrimaryKey` now reaches `ActiveRecord::Base`, and railties' generator calls
it there. Without DEC-081's amendment the same change had made three
controller methods `unreferenced`.

## DEC-104 — An `on_load` block's parameter and `def`s are the hooked class's

**Decided.** Two leftovers of DEC-098:

- **The block's parameter is the class.** `ActiveSupport.on_load(:x) do
  |base| base.include(M) end` sends the mixin to every class that runs the
  hook, as a bare `include` in the block does. With `yield: true` the
  parameter is the only way the block reaches the class, and it counts the
  same; a bare `include` in such a block is still not the class's, since
  its `self` is the caller's.
- **A `def` defines on the class.** In a block registered as its file loads
  (not `yield: true`), a `def` is a method of a module `on_load(:x)`,
  declared at the block's `do`, which each class running the hook prepends.
  The hook runs after the class body, so its `def` replaces the class's own
  method of that name; prepending says the same thing to a lookup. The
  module is what `--def` names as the owner, and it shows in `--ancestors`.

**Why a module, not the class.** The classes that run a hook are known only
once the tree reads every `run_load_hooks` (DEC-098); a method's owner is
settled from its own row. A module the hook prepends puts the method where
the tree already carries hook edges, and needs nothing new below the
extractor. The name is how the source writes it.

**The approximation.** A `super` in such a `def` would, in Ruby, skip the
replaced method; here it would reach it. It is not recorded, as for any
`def` in a block. A `def self.x` in the block stays where it was.

**How much there is.** In rails, discourse, mastodon and the installed gems,
a `def` directly in an `on_load` block that runs as its file loads is
railties' `test_help.rb`'s two `before_setup`s; the rest (27, actiontext's
engine) sit in an `initializer` and are not read. The parameter form was
found twice, neither sending a mixin. Both are in because DEC-098 named them.

**Measured:** no gold verdict, `--refs` tier, `--dead` tier or click moved.

## DEC-105 — A concern's `ClassMethods` are its includer's, not its own

**Decided.** The singleton chain of a module that extends
`ActiveSupport::Concern` no longer holds its own `ClassMethods`, nor those of
the concerns it includes: Concern extends them into the first includer that
is not a concern, and defers them past one that is. A class, or a plain
module, that includes the concern keeps them as before. A call on `self`
written in the concern's own body — `Receiver.via == "self"` — still walks
the old chain, because the code there that reaches for the class side is
`included do`, which runs on the includer and finds them.

**Why.** `Api.build` resolved to `Api::ClassMethods#build` at confidence 1,
and Ruby raises NoMethodError: the tree's rule for Concern ("every module in
the chain that is a concern contributes its `ClassMethods`") was written for
classes and applied to the concern's own singleton too.

**The one exception, and why it is not wider.** A `self` call in the
concern's module body, or in its `def self.x`, would raise in Ruby as
`Api.build` does, and keeps the old answer. Telling it from `included do`
needs the call to say which block it is in, which the blob layer does not
record; such code crashes when it runs, so it is not what a gold trace or a
reader meets.

**Measured:** no gold verdict, `--refs` tier, `--dead` tier or click moved.
Without the `self` exception, rails `--dead` gained six candidates and moved
three more toward unreferenced — `ActiveRecord::Core::ClassMethods#
connection_class_for_self`, `ActiveModel::AttributeMethods::ClassMethods#
attribute_method_prefix` and the like, which each concern calls in its own
`included do` — so a `self` call keeps the includer's view.

## DEC-120 — A call on `described_class` is a call on the described class

**Decided.** `described_class.x`, called in a group or example, is typed as
the class or module the innermost group that names a constant describes, on
its singleton side — the constant itself, as RSpec's `described_class`
returns it — and `described_class.new.x` as its instance, by the chain's
`new` rule. The rung is `described_class`. A `let(:described_class)` in
reach is the `let`, and a group that describes only strings has nothing to
describe, so a call there stays residue.

**Before**, DEC-096 read `described_class` only as a `let`'s value.
Written as a receiver, it was a chain whose previous call is rspec-core's
`ExampleGroup#described_class`, which declares no return, so the call was
residue — `described_class.blocked?` in mastodon's `domain_block_spec.rb`
offered five `blocked?`s ranked by arity. It is the most common receiver in
a model spec.

**Where it lives.** The resolver, from the file's `described` groups that
DEC-114 already records, not the extractor. Recording the call as if
`DomainBlock.blocked?` were written would have been one line, but it would
have claimed `const` for something the source does not write, and hidden the
rule from `--explain`.

**Measured** (BASELINE, "A first-time Rails user"): 24 gold spec sites
residue → correct across graph_weaver, accord and polyid, 60 spec clicks
that missed now answer, and no confidently wrong count, `--refs` tier or
`--dead` tier on rails moved.

## DEC-121 — An override is reached through what it overrides

**Decided.** `--dead` asks of each candidate what a `super` written in it
would reach — DEC-068's lookup, from the method's owner, or from each class
that mixes in its module. Each landing is a method this one overrides, and
whoever calls that one on an instance of this class runs this one. A
candidate with no reference that overrides something is tier `override`,
confidence `lower`, and its reason names the method it overrides; one in
another tier gets `overrides X` in its `caveat`, which grades it `lower`.
Every row carries `overrides`.

**Before**, `DatabaseViewRecord#readonly?` in mastodon, which ActiveRecord's
`Persistence` asks of every record it saves, was `unreferenced` with clear
confidence, as were each Arel visitor's `visit_X` (dispatched by name from
`ToSql`), `extended` hooks and `init_with`. On rails, 71 candidates move
`unreferenced` → `override` and 254 more get the caveat; on activerecord
alone 39 and 146; on mastodon 4 and 10 (BASELINE, "A first-time Rails
user").

**Why a tier and not only a caveat.** `unreferenced` means nothing was
found, and something was: a definition the method replaces, whose callers
may be anywhere, most often in a gem the checkout's evidence does not cover
(DEC-074). A caveat on `unreferenced` would still sort it with the
candidates that truly have nothing. Where a reference was found, the tier
already says so and the override is one more way in, so it lowers the
grade and nothing more.

**Not counted twice.** DEC-081 already makes a `self` call in the ancestor a
`possible` reference to the override, so such a method is not a candidate
at all, or is `single-caller`; the new rule reads no reference and changes
no count. `super-only` (DEC-068) is the other direction: a method that
overrides' `super` reaches. A method can be both, and keeps `super-only`
with the caveat.

**Measured and not taken.** The finding that prompted this named
`AccountSummary`'s `readonly?`, which is `def self.readonly?`, a class
method: ActiveRecord defines no class-level `readonly?` and nothing in
mastodon or its 304 installed gems calls one, so it stays `unreferenced`,
correctly. `--dead`'s text showed the bare name, which hid the `self.`; the
next change names the method as Ruby's documentation does. A `def` that
shadows the writer Rails generates for a schema column (`def x=(value)`)
also overrides something, in a generated module trekr models on the class;
only one of mastodon's unreferenced writers is such a column, so it was left.

## DEC-122 — A module's `self` call reaches an includer's method only ahead of its own landing

**Decided.** DEC-081's amendment makes a `self` call in a module a
`possible` reference to a method another of its includer's modules defines.
That now holds only when some includer has the target *ahead of* the
owner the call's lookup from the module found, or has it anywhere when the
lookup found nothing. Behind the landing, Ruby finds the landing first and
the target is shadowed; the site is excluded (`different_owner`).

**Before**, the rule asked only whether an includer had the target in its
chain at all. `class Widget; include Formatting; include Labeled; end` puts
`Labeled` first, so `Labeled#show`'s `label` runs `Labeled#label` — which
`--def` answered — while `--refs Formatting#label` counted the call and
`--dead` called the method `single-caller`. The three disagreed about one
call.

**The stricter rule was measured and turned down**: counting the site only
when an includer's first landing *is* the target reverts 36 real rails
sites of the shape the amendment was made for, where the includer reaches
the target through a module the call's own lookup never sees.

**Measured** (BASELINE, "A first-time Rails user"): 2 of rails' 40 `--refs`
queries move one site each possible → excluded (`valid?` from
`ActiveRecord::Validations`, `execute` from PostgreSQL's
`DatabaseStatements`); rails `--dead` gains 4 candidates and moves one
`single-caller` to `super-only`, activerecord alone gains 1 — identical,
candidate for candidate, to the build the hunt that found it validated.

## DEC-123 — Minitest's tell is a call, not a string

**Decided.** DEC-095's tell — `.must_`, `.wont_`, `minitest/spec`,
`Minitest::Spec` — counts only as code: a call named `must_*` or `wont_*`
with a receiver, a `require "minitest/spec"`, or the constant path. The
byte search stays, as the gate: a file holding none of the words is not
walked. One that does is walked over the tree the extractor already parsed,
so there is no second parse, which is what DEC-095 turned the AST down for.

**Before**, accord's `spec/accord/instrumentation_spec.rb` asserts on the
event name `"accord.parse.must_be_positive"`, which holds `.must_`: the
file's bare `describe` opened no group, and every call in it was residue.
DEC-095 called this outcome "not wrong", and it is not; it is also a whole
file of answers thrown away for a word in a string.

**Measured** (BASELINE, "A first-time Rails user"): accord's gold set, 3
spec sites residue → correct; nothing else moved.

## DEC-124 — A shared group's name, where it is included, is the group

**Decided.** The literal handed to `include_context`, `include_examples`,
`it_behaves_like` or `it_should_behave_like` in a group is recorded,
file-local like DEC-114's described groups, with the module DEC-092 names
from it. A position inside it answers as a reference to that module:
`--def` and the editor's definition go to the `shared_examples` or
`shared_context` block that makes it, hover shows its card. A name nothing
indexed defines is residue saying no top-level shared group by that name
is indexed.

**Before**, a string holds no name, so the position snapped to the nearest
one on the line, the includer method, and answered rspec-core's
`it_behaves_like`: a confident answer to a question nobody asked. The
string is the whole point of the line, and RSpec's key for the group.

**Snapping, said where it is read.** The snap was disclosed by a stderr
note, which a pipe or an agent's tool call drops, and `--help` and the
README said nothing of it. The text answer now carries a `snapped_to` line
under the answer, as JSON always did, and both documents say a column on
no name snaps.

**Not stored**, and so not a `--refs` answer: `--refs
RSpec::SharedExampleGroups::X` does not list the includes. That would need
a constant reference the blob layer keeps, whose span is the string rather
than the constant's written tail; nothing has asked for it.

## DEC-125 — `--status` answers about the checkout you are in

**Decided.** `--status` shows the checkout containing the working
directory, with the gems its bundle resolves counted on its row (`gems:
{count, indexed, files}`), and counts every other checkout (`others: {repos,
gems}`). Outside any checkout it lists the repos, each with its gems
counted. `--all` lists every checkout, gems included, each with `kind`.
JSON and text show the same rows. An empty store carries `reason`: the
upgrade that dropped the index (the wording `not_indexed` uses), or that
nothing has been indexed.

**Before**, `--status` listed every checkout in the store. Gems are
checkouts, indexed once per machine and shared (DEC-029), so a first-time
user on mastodon got 304 gem rows and had to scroll to find the app. The
number they wanted — did my gems get indexed — was nowhere. And after an
upgrade, `--status --json` was `checkouts: []`, exit 1, with none of the
explanation `--def` and `--refs` give, so a store emptied by a format change
read as one never used.

**Why the default JSON changed too**, rather than only the text. The house
rule is one answer in two encodings; a JSON that listed everything while
the text summarized would make `--status --json` a different command. The
one script that read the whole list (`script/absent.py`, mapping paths to
checkouts) passes `--all`.

*Reverses if:* a consumer needs the whole list often enough that `--all` is
the common case.

## DEC-126 — A name defined nowhere is its own residue

**Decided.** When no indexed definition anywhere — the checkout, its gems,
Ruby core — carries a call's name, the residue says that: "nothing trekr
indexed defines this name anywhere … a gem may generate it at runtime
(Devise's `authenticate_user!` is one), or define it in a gem that is not
installed". It takes the place of both "the receiver's type is known, and
nothing indexed in its ancestors defines this name" and "the receiver's
type is not determined by this file", each of which is still said when the
name does exist somewhere.

**Why.** The two are different findings with different next steps. A
known receiver whose ancestors lack a name that other classes define is a
question about this class's chain — a mixin not seen, a method on another
object. A name defined nowhere is a question about what was indexed: a
method a gem writes with `class_eval` of a string or `define_method` with a
computed name, or a gem not installed. Devise's `authenticate_user!` is
generated per mapping, and a first-time user read the ancestors wording as
trekr saying their controller lacked it.

**Measured** (BASELINE, "A first-time Rails user"): of the 21,154 replayed
clicks, 443 misses are now the new bucket (`script/clicks.py`'s "defined
nowhere indexed"), drawn from "symbol argument" (244), "chained receiver"
(85), "known type, method not found" (54) and the rest; no gold verdict
moved, since a residue's reason is not scored.

## DEC-127 — In a module, `self` is what mixes it in, for completion and for the words

**Decided.** Completion of a bare word in a module — its methods, its
`included do` block — adds the methods of the classes that mix it in
(`mixers_of`, at most four; past that the list is marked incomplete), on
the side the call runs on, after the module's own. And a residue for a
call in a module says which of three things happened: no indexed class
mixes the module in; the includers were asked for an instance method and
none has it; or, in an `included do` block, they were asked for a *class*
method and none has one. The module is named. Hover shows the resolver's
words instead of its own, and a name defined nowhere says so first
(DEC-126).

**Before**, a bare word in `included do` completed from the module alone,
though the block runs on the including class, and hover on a call there
said "no class that includes it defines `stamp!`" when the includer did,
as an instance method: the lookup had been on the class side, which is
right — Ruby would raise — and the words hid it.

**Why `mixers_of`, and four.** A module's includers are every class with
it in its chain, which for `ActiveRecord::Persistence` is every model; the
classes whose own `include` names it are the ones a reader has in mind,
and their subclasses add nothing a bare word in the module can rely on.
Four covers a concern shared by a few models without turning the list into
the union of an app.

**Not done: `on_load` blocks.** A call directly in `ActiveSupport.on_load
(:active_record) do`, or in a `def` there, is residue — "a block handed to
a method that may run it on another object" — because the resolver does
not type it, though DEC-098 and DEC-104 know which classes run the hook.
Completion follows the resolver, so it offers Object's methods there.
Typing those calls as the hooked classes' is its own change, with the
cost question every includer-wide lookup carries (`on_load(:active_record)`
is in every model's chain). *DEC-214 does.*

**Measured** (BASELINE, "A first-time Rails user"): no gold verdict moved;
of the click misses, 47 of the 145 "module never mixed in" were names
defined nowhere and are now that bucket.

## DEC-130 — A scope that makes methods from unnamed names is marked, and no answer calls its method absent

**Decided.** The extractor marks a scope that defines methods whose names its
source does not state: `define_method` on `self` whose name no literal or
literal loop spells (in a class body, or in a class method, where `self` is
the class), and `class_eval`/`module_eval` handed a string. The mark is an
`ancestry` row of relation `dynamic`, target the method that does it. It is no
ancestor: the tree's assembly drops it, and a query reads the marks only when
an answer is about to say a method is not there.

Then, where nothing indexed in a chain defines the name and a scope in that
chain is marked:

- the card (`trekr Owner#name`) and `--refs Owner#name` answer `residue`, not
  `no_such_method`, with a reason naming the scope, its maker and where
  (`Widget defines methods its source does not name (class_eval,
  lib/widget.rb:12), which may include it`). The card still exits `1`, as
  every residue that found nothing does (DEC-080); `--refs` lists the call
  sites instead of none (DEC-073's exit holds only for the certain answer).
- a call site whose receiver is the queried owner, or inherits from it, is
  `possible` ("the receiver's class defines methods its source does not
  name") where it was excluded as `no_such_method`. A receiver that is some
  other class stays excluded: its marker would define *its* methods. A bare
  name's sites are possible whenever their receiver's own chain is marked.
- `--def`'s residue reason names the marker as well, ahead of DEC-126's
  "defined nowhere": the marked scope is the likelier maker than a gem.

**Why.** A first-time user of 0.7.0 asked for `Faraday::Connection#get` and
was told "has no method get in its ancestors", exit 1, and `--refs
Flipper::Adapters::Wrapper#enable` said `no_such_method`. Both are real
methods: faraday writes its verbs with `class_eval <<-RUBY … def #{method}`
over a list another file assigns, flipper with `METHODS.each { |m|
define_method(m) }`. `--def` on a call already said residue; the card and
`--refs` claimed the certain answer DEC-073 reserves for a chain that was all
seen. It was all seen — but not all of it could be read.

**`method_missing` is not a marker here.** The card already hedges on a
hand-written `method_missing` in the chain, and `--refs` keeps excluding
those call sites, as DEC-112 decided for ActiveModel's.

**Either side.** A mark covers the scope's instance and class methods alike,
since one string can define both. It errs toward the hedge.

## DEC-131 — A loop over a constant's literal names defines each, and so does its bare variable

**Decided.** The iteration whose block is read once per name (session 17's
`[:before, :after].each do |callback|`) also runs over a constant this file
assigns a list every element of which is a literal name — `%i[…]`, `%w[…]`,
`[:a, "b"]`, `.freeze`d or not — looked up lexically among the scopes around
the `each` (`METHODS` in `Wrapper` is `Wrapper`'s, not another class's in
the same file), and over `reverse_each`. Inside it, `define_method(var)`
names each value, as `define_method("#{var}_x")` already did.

**Why.** flipper's `Adapters::Wrapper` defines all eleven adapter methods as
`METHODS.each do |method| define_method(method) do … end end` with
`METHODS = [:import, …].freeze` five lines up, and `--refs
Flipper::Adapters::Wrapper#enable` answered `no_such_method`. DEC-100 already
read a constant this file assigns a list of *classes*; a list of names is the
same fact.

**Not done.** A list another file assigns is that blob's fact: faraday's
`METHODS_WITH_QUERY` is written in `methods.rb` and iterated in
`connection.rb`. Such a loop's `define_method` marks its scope (DEC-130)
instead. Neither is a list built by a call (`attribute_names.each`).

## DEC-132 — A `class_eval` string is read as code, in its scope

**Decided.** `class_eval` or `module_eval` on `self`, handed a string or a
heredoc, has the string read as Ruby written in the scope: its `def`s are
the scope's methods (`defined_via` the evaluator, a definition: the body is
there), its calls are calls, its mixins edges. The string is rendered from
the file's own bytes, each interpolation replaced, and every position a
node of it reports maps back to the file — a literal byte to itself, a
substituted value to the `#{` it replaced — so `--def` inside the heredoc
lands where the text is.

- **Only a simple interpolation.** Each must be a local, or a local through
  `to_s`, `to_sym`, `upcase`, `downcase` or `capitalize`. Anything else, or
  a rendering that does not parse cleanly, leaves the string unread and
  marks the scope `class_eval string` (DEC-130), and `--dead` gives each
  method in that file the caveat `class_eval string`.
- **The code around the values is read once.** It is rendered with a
  stand-in no Ruby name uses, and whatever mentions the stand-in — a `def`
  it names, a call it names or is sent to, a constant, an assignment — is
  dropped. The rest does not depend on the value, so a loop over three verbs
  contributes each call in the string once, as it is written once.
- **A `def` the value names is read once per value**, when the one local is
  a literal loop's variable (DEC-131's loops). Otherwise its names are not
  stated: the scope is marked `class_eval` and the answer hedges (DEC-130),
  while the string's calls still count.

**Why.** faraday's `Connection` defines `get`, `head`, `delete`, `trace`,
`post`, `put` and `patch` this way, and each body calls `run_request`: with
the strings unread, `--dead lib/faraday/connection.rb` called `run_request`
single-caller where it has three, and `Connection#get` was "no such
method". rails writes 82 string `class_eval`s and `module_eval`s (71 of
them heredocs), most inside methods, and every one before this was a string.

**Not done.** The verbs' list is `Faraday::METHODS_WITH_QUERY`, assigned in
`methods.rb`: another blob's fact, so `get` stays a hedge rather than an
answer. Reading it would take storing a constant's literal value and
expanding templated rows in the tree. A string sent to another receiver
(`Foo.class_eval "…"`), `instance_eval` with a string, and `eval` are not
read, nor a string evaluated inside such a string, which marks the scope.

## DEC-133 — `X.new` makes what a custom `new` says it makes

**Decided.** Wherever `X.new` types a value — `x = X.new` (`local:new`), a
chain `X.new.y`, a `let` of `described_class.new`, and the implicit subject
(DEC-114) — the class side of `X` is looked up for `new` first. `Class#new`
(core) makes an `X`, as before. A `new` of `X`'s own, or of a module its
class side has (`extend self`, a `ClassMethods`), that says it returns
something else makes that: its `sig`, or — recorded by the extractor as its
return — a last expression `Other.new(…)`, resolved from where the `new` is
written. One that names a class the index cannot place makes nothing
known. One that says nothing still makes an `X`.

**Why.** flipper's `Flipper` is a module that `extend self`s and defines
`def new(adapter, options = {}) DSL.new(adapter, options) end`. Its specs
write `let(:flipper) { described_class.new(adapter) }`, and trekr typed
`flipper` as a `Flipper` and answered `flipper[:search]` with `Flipper.[]`
at confidence 1: confidently wrong. Ruby runs `Flipper::DSL#[]` — an alias
of `feature`, whose body is the answer now.

**A `new` that says nothing is an `X`, not residue.** The first cut made
it residue, as the rule was first written, and measured worse: accord's
gold set lost 35 correct answers, widget_shop's gem floor 21, and 529 rails
`--refs` sites went from excluded to possible. Two `new`s did it.
ActiveRecord's `Inheritance::ClassMethods#new`, every model's, ends in an
`if` whose branches are a subclass's `new` (STI) and `super`. And sorbet's
gem-generator tracer, which `Class.prepend`s a `new` that wraps `super`, is
read as an edge on every class (DEC-097 cannot tell that it runs only under
the tracer). Both make an `X`. Only positive evidence of another class
moves the answer.

**Not done.** A `new` whose last expression is an `if`, a cached
instance or a factory's call types nothing new. `private_class_method :new`
is not read, as before.

## DEC-134 — With no `Gemfile.lock`, the declared dependencies at their highest installed versions

**Decided.** A checkout with no `Gemfile.lock` resolves its gems from what
it declares: every `*.gemspec` at its root (`add_dependency`,
`add_runtime_dependency`, `add_development_dependency`) and its `Gemfile`
(`gem`), read with Prism. Each name's requirements, merged across files,
pick the highest installed release that meets all of them (a prerelease
only when no release does), searched in the roots a lockfile's gems are.
Each installed gem's runtime dependencies follow, from the gemspec rubygems
wrote in `specifications/` or failing that the one it shipped, until
nothing new is named. Left out: a `gem` from `path:`, `git:` or `github:`,
one for `platforms:`, and the checkout's own gemspecs. (Amended by DEC-293:
a `git:` or `github:` gem is its checkout, or said.) A requirement that
interpolates (`"~> #{ENV['V'] || '1.4'}"`) is any version. `--index` says
which list it used (`gems.resolved_from`: `lockfile` or `declared`), and a
name nothing installed meets is reported missing with its requirement.

**Why.** Most gems commit no lockfile, and trekr indexed no gem for them:
a first-time gem author's specs had no rspec-core, so `describe`,
`it_should_behave_like`, shared examples and every matcher answered
residue ("rspec-core is not indexed"). flipper and faraday both. The gemspec
and Gemfile say which gems, less exactly than a lockfile; the versions
installed are the ones `bundle install` would most likely have locked, and
the ones the author's specs run against.

**Measured.** flipper: 90 gems resolved (89 indexed, 3,407 files, 7.5 s
cold), 14 not installed — optional adapters (`dalli`, `mongo`) and dev
tooling; `it_should_behave_like` in `redis_cache_spec.rb` now answers
rspec-core's `define_nested_shared_group_method` declaration. faraday: 38
resolved, 7 not installed (rubocop at `~> 0.90`).

**Not done.** Git gems (`bundler/gems/`) and `eval_gemfile`. The choice is
per name, not a solver: two requirements from different dependents that no
single installed version meets both leave the first one's pick.

## DEC-135 — `--context` names the checkout for a name query too; no `-C`

**Decided.** `--context DIR` applies to every query: a position (as
before), a name (`trekr Widget#save`, `trekr Widget`), `--refs` and
`--ancestors`. For a name it is the checkout asked about, where it was
always the current directory's. `DIR` must exist (`66` otherwise) and be in
a checkout; for `--ancestors`, as for a query from inside a gem, a gem
answers from the app that resolves it. It still means nothing to `--index`,
`--dead`, `--symbols` or `--drop`, which each name their own path, and says
so (`64`).

**Why.** A first-time user could only aim a name query by `cd`. `-C DIR`,
git's spelling, was considered and turned down: trekr already had one flag
for "answer as if asked from this checkout", and DEC-080 is one name per
concept. `--context` for a position pins the checkout a gem's position is
answered from; for a name there is no path to take it from at all, so the
flag says the same thing where it was missing.

## DEC-136 — Only a model's scope runs on a relation, and the relation prefers the model's class method to Kernel's

**Decided.** DEC-116 types a `scope`'s body as `ActiveRecord::Relation` only
where the scope is written in a class that inherits `ActiveRecord::Base`, or
in a module such a class includes (a concern's `included do`). A name split
by its superclasses (DEC-072) is the variant the file declares: rails' test
`Post` is one. Elsewhere —
Mongoid, ActiveHash, a class with its own `scope` — the body is typed as the
class it is written in, as before DEC-116. And on the relation, a name
whose lookup lands in core (`Kernel#display`, `#format`) answers the
model's own class method of that name when it has one.

**Why.** The 0.7.0 hunt: the check was `in_scope && RELATION is known`, so
any class's scope in a checkout that had ActiveRecord answered
`Relation#where` at 1.0 — a Mongoid document's, a plain class's. Mongoid
runs a scope's body with `instance_exec` on the class, and a plain class
runs it wherever it calls it. For the second half, ActiveRecord's
`Scoping::Named` generates a relation method for every model class method
Kernel also responds to and `Relation` does not define (`generate_relation_
method(name) if Kernel.respond_to?(name) && !Relation.method_defined?
(name)`), so `scope :shown, -> { display }` runs the model's `display`.

## DEC-137 — `x.class` is `x`'s class

**Decided.** In a chain, `.class` with no arguments on a receiver typed as
an instance of `X` is `X` itself: the next call is looked up on `X`'s class
side. On `self` it keeps `self`'s view, so a concern's `self.class` walks
its includer's class methods (DEC-105), and a call through it counts a
subclass's override as DEC-081's does.

**Only on a class.** In a module, `self.class` is whichever class includes
it, and that class's own class method is the one that runs: typed as the
module's side, activerecord's `Quoting#quote_table_name` (`self.class.
quote_table_name`) answered `Quoting::ClassMethods` at 1.0 where the traced
adapter's override ran — three confidently wrong answers in widget_shop's
gem floor, measured before it was kept to classes.

**Why.** `self.class.statuses` answered residue: `Kernel#class` returns
`Class`, which has no `statuses`. testbed 040 pinned `self.class.build` as a
split vote between two `build`s; it now answers `Builder.build`'s declared
`Widget`, which is what runs unless a subclass overrides `build`.

## DEC-138 — What Rails generates sits behind the class and the modules it includes

**Decided.** An instance method declared by a macro Rails writes into a
module the model includes as it is made — a schema column, `attribute`,
`alias_attribute`, `enum`, `belongs_to`/`has_one`/`has_many`/HABTM,
`has_one_attached`/`has_many_attached`, `accepts_nested_attributes_for`,
`has_secure_password`, `store`/`store_accessor` — is held while the lookup
walks the class and the modules it includes, and answers only if none of
them defines the name, before the superclass is reached. Among the held
ones, a model's declaration still beats the column's (DEC-110).

**Why.** Rails includes `GeneratedAttributeMethods` and
`GeneratedAssociationMethods` in `inherited`, before the class body runs, and
an enum's and a store's methods go into modules of their own, so every
module the body includes later, and the class itself, come first in the
chain. Two silent wrong answers from the 0.7.0 hunt: a hand-written `def
status` written above `enum :status` lost to the enum, because the lookup
took the last-written definition; and an `enum` in a concern's `included
do` lost to the schema's column, whose declaration sat on the class and the
enum's on the concern behind it — `status.even?` went to `Integer#even?` at
1.0, where the enum's reader returns a String.

**Not done.** A class method is looked up as before: `scope` defines on the
class itself, and order decides there. `has_secure_token` and `delegate`
define on the class too, so they keep last-written-wins.

## DEC-139 — An index waits for another writer as long as a writer takes

**Decided.** `--index`, `--drop` and `--gc` wait up to ten minutes for
another process's write lock; every other command keeps the 5 s handler,
and a query still never waits (DEC-066).

**Why.** Under load, two concurrent `--index` runs exited 74 "database is
locked": one bundle's gems are one immediate transaction (DEC-041), and a
cold one holds the lock far longer than 5 s. Waiting its turn was already a
writer's job (DEC-066); 5 s was a query's number.

## DEC-140 — A declared type is a bound: a call typed as an ancestor may reach a subclass's method

**Decided.** A receiver's type is either the class the object was made as —
`X.new`, a literal, a constant, `described_class` — or a type it conforms
to: a `sig`'s parameter or return, a finder's result (STI hands back a
subclass), a `rescue`'s class, the naming rung. The second is an upper
bound. When `--refs` asks about a method whose owner inherits from a bound
and defines the name itself, the site is `possible` ("the receiver is typed
as an ancestor, and may be the subclass that defines this") where it was
excluded: `different_owner` when the ancestor has its own, `no_such_method`
when it has none. It is DEC-081's rule for `self`, on every rung whose type
is a bound. A local or a `let` is a bound when a write that types it is one;
`self.new` keeps `self`'s. The site stays `confirmed` for the ancestor's own
method, and an owner that is an ancestor of the bound, with a closer
override between, is still excluded.

```ruby
sig { params(context: ::Lib::Context).returns(T::Boolean) }
def self.authorized?(context) = context.admin?  # App::Context < Lib::Context overrides admin?
```

**Why.** Reported: `--refs 'App::Context#admin?'` excluded that call, with
`Lib::Context` known only from its `.rbi`, though an `App::Context` handed
in runs its own `admin?` — the answer that lets someone delete or change a
contract because trekr said nothing reaches it. `--dead` did worse for a
method only the subclass defines, reached that way: `unreferenced`, clear.

**Only a bound.** The report asked for `X.new` too. `Lib::Context.new.admin?`
never runs a subclass's method, so a constructed, literal or constant type
stays exact, and a rung that cannot say is taken as exact, which rules
nothing back in. A custom `new` that declares another class (DEC-133) is
taken as exact for the same reason.

**No floor at `Object`.** Considered: requiring the bound to be something
narrower than `Object`, `BasicObject` or `Kernel`, which every class
inherits. Turned down: a receiver bounded by `Object` narrows nothing, which
is what an untyped receiver is, and those are `possible` already. Excluding
it would be the one claim — "provably not this method" — the type cannot
support. Measured, the `Object` sites are the naming rung reading a variable
called `object` (74 of the rails moves below, 62 of graph_weaver's), which
was excluding every `inspect`, `to_s` and `respond_to?` a class overrides.

**Measured** (BASELINE, "A declared type is a bound"): every gold verdict and
all 21,154 clicks unchanged; rails' 40 `--refs` queries unchanged; a sweep
of the 3,478 rails methods defined in a class with a superclass and named
by another owner moves 1,007 sites excluded → possible across 122 queries,
none from or to confirmed. Most are core bounds the naming and `sig` rungs
give: `Hash` (a `hash` variable, 422; `HashWithIndifferentAccess#[]` goes
8,337 → 8,457 possible), `String` (`SafeBuffer#+`, 180), a `rescue`'s
`Exception` (109). Sampled, each is dispatch that can happen:
`column.has_default?` on each adapter's `Column`, `ex.set_query` on a
`rescue StatementInvalid => ex` reaching `MismatchedForeignKey`,
`registration.matches?` reaching `DecorationRegistration`, `entry.value`
reaching `Cache::Coder::LazyEntry`. graph_weaver's sweep of all 2,090 of its
methods moves 281 sites across 82 queries, 121 of them a `node` typed
`Codegen::Node` reaching each node class's override. `--dead`: rails 2,281 → 2,279
candidates, activerecord alone 1,613 → 1,611, each an `override` whose
caller is now counted.

**Not done.** `--def` still answers the bound's own method `resolved`;
DEC-081's amendment made a `self` call with overrides `ambiguous`, and doing
so for a bound is its own decision, with gold verdicts to move. A module a
subclass of the bound includes is not counted, as DEC-081's amendment counts
a module's includers. *DEC-213 counts it.*

*Reverses if:* a guard — `case node when Scalar`, `is_a?` — ever narrows a
receiver per site, which would tighten the bound there.

## DEC-170 — `--status` answers `not_indexed` for a checkout nobody indexed

**Decided.** `--status` asks about one checkout — the one the working
directory is in, or `--context DIR`'s — found as a query finds it: its git
repository, else the indexed gem the path is in. When that checkout is not
in the store, the answer is the one a query from there gives: `status:
not_indexed`, `repo`, `reason`, `hint`, exit `2`. Beside it, `checkouts: []`,
and `others`/`totals` summarize everything else. Outside any checkout
(without `--context`) nothing changed: the repos are listed, exit `0`.
`--context` naming a path outside every checkout is `not_a_repo`, 66, as for
a query; `--all` and `--context` conflict.

**Before**, a checkout indexed nowhere fell through to the outside-any-
checkout listing, so `--status --json` showed *another* checkout as
`checkouts[0]` and exited `0`, while `--refs` from the same directory said
`not_indexed`, exit `2` (hunt 2). The same fall-through hid a second bug:
the lookup matched the working directory as a path *inside* a root, so run
from a checkout's own top directory — the usual place — `--status` found no
checkout there and listed every repo too. A script checking "is this checkout
indexed" got yes, about the wrong repo. That is the silent-wrong DEC-125 set
out to end: `--status` is about the checkout you are in, and a row that is
not that checkout cannot be the first thing it shows. An empty store in a
checkout was exit `1`; it is `2` now, for the same reason — `1` is
"looked, found nothing" (DEC-080), and nobody has looked.

**Why `checkouts: []` rather than leaving it out.** A consumer that reads
`checkouts[0]` must get nothing, not a key error it papers over; and the
field keeps one meaning — rows for the checkout asked about.

*Reverses if:* a caller needs `--status` to mean "is anything indexed at
all". That is `--status --all`'s exit code.

## DEC-171 — A writer waiting for the lock says so

**Decided.** A writer — `--index`, `--drop`, `--gc` — that finds another
process holding the write lock prints "waiting for another trekr writer to
finish with DB" on stderr once it has waited a second, then "still waiting
(Ns)" every 10 s when stderr is a terminal and every minute when it is not.
Nothing reaches stdout until the command's own answer, in every mode, so
`--json | jq` is unchanged. The wait is still DEC-139's ten minutes; the
busy handler is SQLite's `busy_timeout` backoff reimplemented so it can
speak, and a query keeps the silent 5 s.

**Before**, a queued `--index` sat silent for up to ten minutes, on a
terminal too (hunt 7): indistinguishable from a hang, and the likeliest
reaction — Ctrl-C and run it again — queues it again.

**Why stderr under `--json` too.** An agent reading a JSON run sees stderr
as the only sign of life, and it is where DEC-067 already puts every
message; the rule that matters is that stdout carries only the answer.
**Why a slower cadence off a terminal:** a CI log or an agent's transcript
wants to know it is waiting, not a line every 10 s for ten minutes.

**No pid.** SQLite does not say who holds a lock, and the holder need not
be trekr at all (an LSP's refresh, or `sqlite3` in a shell). A pid file
written by each writer would name the queued writers as readily as the one
holding the lock, since every writer registers before it waits; a
wrong pid is worse than none, since the obvious use of one is `kill`.

*Reverses if:* the store gains a record of the lock holder that is right —
then the notice names it.

## DEC-172 — The naming rung never types a receiver as a class every object is

**Decided.** The naming rung (`from_receiver_name`) maps a variable's name
to the class of that name. It no longer fires when that class is one every
object already is: `Object`, or anything in `Object`'s ancestor chain —
`Kernel`, `BasicObject`, and a module mixed into `Object`. Every other
class still counts, from core or not: `hash` is still `Hash`, `set` still
`Set`.

**Before**, a variable called `object` was typed `Object`, so
`object.inspect` answered `Kernel#inspect` by `receiver_name` and was a
*confirmed* reference to it, and `--refs` for a module's `inspect` (which
DEC-140's bound does not reach) excluded the site. Found by the downcast
lane. The name is a word, not a type: every object answers `inspect`, so
the "corroboration" that the class answers the call is no corroboration at
all.

**Considered: only classes app or gem code defines.** The rule the report
suggested — skip every class Ruby core defines — also covers `data` →
`Data`. Measured, it cost a right answer: money's `set.add` inside
`each_with_object(Set.new) { |k, set| … }` went right-owner → residue, and it
turned 164 rails sites confirmed → possible and 2,036 excluded → possible
across 228 of the sweep's queries, most of them `hash[…]` read as `Hash#[]`,
which is what those sites are. It is also not what it says: a core class an
app reopens (ActiveSupport reopens `Object` and `Hash`) has app sites too.
`data` → `Data` stays; it is `ambiguous`, never `resolved`, since `Data`
shares every method it has with other classes.

**Measured** (BASELINE, "A name every object answers to"): every gold
verdict unchanged, on the sites where a receiver is spelled like a core
class (545 across the four gold sets and widget_shop — TracePoint cannot see
a C method, so the gold holds none of the `Kernel` calls this moves);
rails' 40 `--refs` queries: 3 sites excluded → possible, all
`ActiveRecord::Core#inspect`; with a sweep of 275 more — `inspect`, `to_s`,
`respond_to?`, `==`, `hash`, `to_h` — 15 call sites move in all, 88
excluded → possible and 15 confirmed → possible (`Kernel#inspect`,
`Kernel#respond_to?`); graph_weaver's sweep of 684: 14 call sites, 203
excluded → possible, 11 confirmed → possible. Nothing moved to confirmed or
to excluded. The click replay over the 13 repositories: empty and unsure
unchanged in every repo (5,094 definition misses either way); 10 misses
move from "typed, with competitors" and "known type, method not found" to
"untyped local or parameter", which is what they are.

*Reverses if:* a codebase names variables after `Object`'s mixins on
purpose — then the chain test narrows to `Object`, `Kernel`, `BasicObject`.

## DEC-150 — A git gem is found at its locked revision, in its own gemspec's directory

**Decided.** A lockfile's `GIT` section is found where bundler checks it
out: `bundler/gems/<repo>-<revision[0,12]>/` beside each `gems/` directory
searched, the repo name being the remote's basename less `.git`
(`Bundler::Source::Git#base_name`, `#shortref_for_path`). Within the
checkout, each gem the section names is the directory holding
`<name>.gemspec`, searched as bundler globs, `{,*,*/*}.gemspec`, shallowest
first; a section naming one gem takes the checkout's one gemspec whatever
it is called. `BUNDLE_PATH` is searched before anything else — the app's
`.bundle/config` (or `$BUNDLE_APP_CONFIG`'s), the environment, then
`~/.bundle/config`, as bundler ranks them, under `ruby/*/`. A `PATH` gem
inside the checkout is part of it, as before. `--index` counts git gems
(`gems.from_git`) and in-checkout path gems (`gems.from_path`); a git or
path gem it did not index goes in `gems.unlocated` with why — a checkout
not where bundler would have put it (naming that path), no gemspec by the
gem's name in it, a path outside the checkout — never under `missing`,
which stays "not installed".

**Why.** An app pinning Rails from git (`gem "rails", github: …`) had 59
gems reported "not installed", and everything from Rails answered from
Sorbet's RBI or as residue: the checkout is named for the repository, not
the gem, and a monorepo's gems are its subdirectories. The old lookup
matched `bundler/gems/<gem name>-<hex>`, which finds a single-gem repo
named like its gem and nothing else, and took whichever revision `read_dir`
listed first — graph_weaver has two on this machine.

**Measured.** No change for a lockfile without `GIT`: the 13 dogfood gems,
widget_shop, flipper and discourse locate the same gems before and after.
mastodon's `webpush` (a git gem) resolves to the same directory, now by its
revision. rails' three git gems move from "not installed" to "git source,
checkout not found at ~/.rvm/gems/ruby-3.4.9/bundler/gems/httpclient-d57cc6d5ffee"
— true: they are not installed here.

**A git gem's clone is a gem, not a checkout.** Bundler's checkout has a
`.git`, so git's toplevel for a file in it is the clone — never indexed, and
for a monorepo not even the gem — and a position there answered "never
indexed" (C5). The store's deepest checkout containing the path is now asked
first, by `--def`/`--refs`, `--index` and the LSP alike: when it is a gem,
the answer comes from the app that bundles it (DEC-029), and `--index` on it
says how to refresh it, as for any gem. When it is a repo, or there is none,
git decides as before, so a submodule an outer repo contains is still its own.
The LSP's hover labels a gem by the same test instead of a parent directory
named `gems`.

**Not asking bundler.** `bundle list --paths` would be the fallback; it
needs the project's Ruby and a resolvable bundle, which DEC-016 turned down
as the product's first edge, and with the checkout named from the lockfile
alone there is nothing left for it to find. `bundle config local.<gem>`
overrides (a git gem served from a working copy) and a `glob:` other than
the default are not read.

**Path gems outside the checkout are reported, not indexed.** A gem's
identity rests on a directory whose name pins its bytes (DEC-017) — a
version, a revision. `path: "../shared"` pins nothing and is usually a
repository someone edits; indexing it as a gem would skip it once seen and
stamp it `kind = 'gem'`, turning that repository's own `--index` into "is a
gem". It needs a third kind of checkout, not a gem with an exception.

## DEC-151 — Without a lockfile, every requirement on a gem binds its pick, and one trekr cannot read is said

Amends DEC-134.

**Decided.** A name's requirements — the checkout's own and every picked
gem's runtime dependencies — are intersected before a version is picked,
and the picks are revised until they hold still (capped at eight rounds).
A requirement is read when it is a literal, or a constant or local the
file bound to one before the call, or a block parameter over a literal
list (`%w[a b].each { |g| s.add_dependency g }`), and an environment
lookup (`ENV["V"] || "7.1"`, `ENV.fetch("V", "7.1")`, interpolated or not)
reads as its default — what runs when nothing is set, the same rule as a
conditional's. Anything else — `version` read from a file, a lookup with
no default — binds nothing and is listed in `gems.unread` with its source.
A pick is also looked one step ahead: among the versions that meet a
name's requirements, one whose own runtime requirement on a name the
checkout pins leaves that name no installed version is passed over, if
another is not. A name declared in both
branches of a conditional takes the branch that runs when nothing is set:
`else`, or an `unless` body. `gems.picked` lists every gem found as `name
version`, and without a lockfile the text says the picks.

**Why.** Three silent wrong picks from the 0.8.0 hunt. A transitive `>=
5.1` from activesupport's gemspec picked minitest 6.0.6 past the gemspec's
own `~> 5.25`, in either declaration order: the first requirement to reach
a name was the only one, so `json "< 3"` also lost to activesupport's `json
>= 0` and gave 3.0.2. `JREQ = "~> 2.9.0"` and a version read from
`VERSION` were each "any", so the newest installed was taken with nothing
said. And a Gemfile's `if ENV["MODERN"] … else …` merged both branches'
`nokogiri` pins into one requirement nothing meets, reporting a gem that
is installed as not installed.

**Measured.** The hunt's repros: minitest 5.26.0 (`!= 5.27.0` binding),
json 2.21.2 under `>= 2.9, < 3`, json 2.9.1 under a `~> 2.9.0` constant and
under a loop, rake 13.3.1 under `ENV.fetch("RAKE_VERSION", "~> 13.3.0")`;
`activesupport` beside `version` listed as unread. flipper, the one
dogfood checkout without a lockfile: rails `"~> #{ENV['RAILS_VERSION'] ||
'7.1'}"` was any and picked 8.1.4, whose `activerecord = 8.1.4` the
gemspecs' `< 8` cannot meet; it now reads `~> 7.1` and picks 7.2.3.1, the
activerecord the gemspecs allow. Its `sqlite3 ~> 1.4.1` default is not
installed and says so, where 2.9.6 was taken as any: 90 found → 89.
Without the lookahead the intersection alone reported activerecord and
activesupport "not installed" — both are.

**Not done.** Still per name, not a solver: two requirements no single
installed version meets leave the name missing with both written, rather
than backtracking into a parent's other version.

## DEC-152 — Without a lockfile, gems are picked from one Ruby's, and `--index` says which

Amends DEC-134.

**Decided.** A checkout with no `Gemfile.lock` resolves against the gem
directories of one Ruby: the project's own (`BUNDLE_PATH`,
`vendor/bundle`, `.bundle`), then the first of — the version
`.ruby-version` or the Gemfile's literal `ruby "x"` names, matched to an
rvm, rbenv or asdf install of it or to the directories for its ABI
(`~/.gem/ruby/3.4.0`, Homebrew's); `$GEM_HOME`/`$GEM_PATH`; the `ruby` on
`$PATH`, resolved to its prefix, with the other directories for its ABI.
With none of them, every Ruby, as before, and that is said. `--index` has
`gems.ruby`, the choice in words, and the text names it. A lockfile still
searches every Ruby: it names exact versions, and any copy of one is the
same bytes. (Amended by DEC-291: the checkout's Ruby's first, and
a gem found only in another's is said.)

**Why.** "The highest installed" depends on which Ruby: the hunt's gem
resolved `rspec-core ~> 3.12.0` to 3.12.3 from Homebrew's Ruby 3.3 while
everything else came from rvm's 3.4.9, where 3.12.2 is the newest. Its
specs never ran against 3.12.3.

**Measured.** The hunt's repro now picks rspec-core 3.12.2 and
rspec-support 3.12.1 from rvm's 3.4.9; flipper resolves entirely from
`$GEM_HOME`, the Ruby rvm made current. No lockfile checkout changes.

**Not done.** rbenv's shims are scripts, not a prefix, so with rbenv and
no `.ruby-version` the `$PATH` step finds nothing and every Ruby is
searched — said as such. `ruby file: ".ruby-version"` in a Gemfile is not
followed, and neither is `.tool-versions`.

## DEC-153 — A default gem says its code is the stdlib; indexing it waits on a checkout of part of a directory

**Decided.** A gem located at an empty `gems/<name>-<version>/` with a
spec in `specifications/default/` is a default gem at the version its Ruby
ships, and its code is that Ruby's stdlib, `lib/ruby/<abi>/`. It is listed
in `gems.unlocated` — "default gem, its code is Ruby's stdlib in …, not
indexed" — rather than counted found with nothing read.

**Why.** json 2.9.1 in a lockfile or picked without one reported "1
resolved, 0 newly indexed (0 files)" and answered nothing, with no reason;
58 default gems do this, lockfile or not (logger, uri, set, psych, prism,
openssl…).

**Not indexed, yet.** A checkout is a directory (DEC-017), and every default
gem's files share one: the stdlib. Indexing the stdlib as one checkout per
Ruby would be shareable and immutable, but it holds every default gem's
files, so an app that bundles json 2.21.2 and any default gem would see
two `JSON`s — a confidently wrong answer where there is residue now.
Filtering it per app makes its file set depend on who indexed last. The
fix is a checkout that owns part of a directory (a gem's `s.files` under
the stdlib), which is a store change, decided separately.

## DEC-160 — A marker hedges only the names it can make, on the side it makes them

**Decided.** DEC-130's marker says more than "this scope makes methods". Its
target is the maker, the side, and the shape of the names
(`core::Maker`: `define_method|instance|_render_with_*`), and a chain's
marker counts only for a name of that shape on that side:

- `define_method` makes instance methods (class methods in `class << self`
  or a `singleton_class` block), `define_singleton_method` class methods,
  and a string of code whatever its `def`s say (`def self.x` or `def x`;
  either side when it may make them some other way).
- A name whose text the source spells in part is a shape:
  `"_render_with_#{key}"` is `_render_with_*`, and so is a method of the
  scope whose body is that string (`define_method(renderer_name(key))`,
  looked up once the file is read). An unread string's `def`s give their
  shapes from its text, `*` for each interpolation.
- A name the source spells whole is defined, not marked: a literal
  `define_method(:made)` in a class method is the class's method, with its
  block or `&blk` as the body; `define_method(meth)` where `meth =
  "sanitized_#{m}"` inside a literal loop names each value, as DEC-131's
  variable does. Only when a block in between may run elsewhere is such a
  name marked instead, by its exact shape.
- A `define_method` in a block run on something else — `mod.singleton_class.
  instance_eval do`, `Class.new(self) { … }` — marks nothing here. One sent
  to a constant (`Target.send(:define_method, n)`, `Target.define_method`)
  marks that constant, and `singleton_class.define_method(n)` defines on
  the class side.
- The reason names every marker of the scope that may have made the name,
  with its shape: `(define_method, app.rb:48; define_method \`*_x\`, app.rb:52)`.

**Why.** The 0.8.0 hunt: on mastodon, 16% of the certain "no such method"
cards became residue (rails 3%), each claiming a scope "may include it" that
could not. Three markers did most of it: actionpack's `Renderers.add`
(`define_method(_render_with_renderer_method_name(key), &block)`), inherited
by every controller; rails-html-sanitizer's
`define_method(meth_name)` over a literal list, and minitest's `define_method
:mu_pp, &:pretty_inspect` in `make_my_diffs_pretty!`. Every one of them spells
the name, or most of it.

**Measured**, the hunt's card sweep (a name no class has, `X#zz_nope` and
`X.zz_nope` on every class): mastodon 819 residue of 3,128 → 399 (0.7.0:
380); rails 607 of 4,824 → 490 (0.7.0: 477). What remains over 0.7.0 is
a hedge that holds: thor's `register` (`define_method(subcommand_name)`),
string `class_eval`s rails interpolates with a call (`#{env.delete_prefix
("HTTP_").downcase}`), a hash's keys. Every gold verdict, the rails `--refs`
queries and `--dead` on rails, activerecord and mastodon unchanged; of the
21,154 replayed clicks, 37 misses moved from "known type, method not found"
to "defined nowhere indexed", whose reason no longer names a marker that
could not have made the name.

**Not done.** A hash's `each |name, value|` (actiondispatch's `DIRECTIVES`),
`each_with_index`, and a list a method returns (`keys.each`) are not literal
loops, and their `define_method` stays an unshaped mark.

## DEC-161 — Every string of code marks the class it makes methods on

**Decided.** DEC-130 marked only `class_eval`/`module_eval` on `self` handed
a string literal. Now:

- A `class_eval` handed anything else — a local, `[…].join`, `format(…)`, a
  heredoc through `.gsub(…)` — marks its scope, with no shape: the source
  does not spell the code. A heredoc through a method that leaves code as it
  is (`.strip`, `.chomp`, `.squish`, `.freeze`, `.dup`) is the heredoc, read
  as DEC-132 reads one.
- `eval` of a string in a class body or class method is read as
  `class_eval` is: it runs there, with the class as `self`.
- `instance_eval` of a string on a class marks the class side only, since
  every `def` in it makes a class method.
- A string sent to a constant (`Target.class_eval "def x; end"`), and
  `self.class.class_eval` in an instance method, mark that class, shaped
  by the `def`s the text spells (DEC-160). Neither is read: the first is
  written in another class's file, and the second runs when the method
  does.

**Why.** The 0.8.0 hunt's probes: each of these left a real method "no such
method", exit 1, where the source says plainly that a string of code makes
methods there.

**Measured.** Every gold verdict, the 21,154 clicks, the rails `--refs`
queries and `--dead` on rails, activerecord and mastodon unchanged. The card
sweep gains five rails residues, each a string that does make methods there:
actionview's `LookupContext::Accessors` (`module_eval <<-METHOD` of a
computed name), `RouteSet::MountedHelpers`, and a test's `PostsController`.

## DEC-162 — A macro's methods are marked on the class whose body calls it

**Decided.** A `class_eval` of a string, or a `define_method`, written in an
instance method runs on whatever that method is sent to. When a class body
calls the method on itself — `add_helper` in `class Widget`, where
`include Macros` extends `Macros::ClassMethods`, or a method `class Module`
defines — `self` is that class, and so is where the methods land. The
extractor marks the method's scope with the method's name (`via`, the
fourth field of `core::Maker`), and records every call a class or module
body makes on itself outside any method, with the literal names it is
handed (`body_call`). The tree places each such mark on every class that
calls the macro, where that class's class-side lookup of the name lands on
the macro's own definition.

- **The names are the caller's.** In the macro, an interpolation of its
  `k`th positional parameter is `{k}` in the shape, and a block variable
  over its splat (`attrs.each do |name|`) is `{k*}`. At each caller they
  become the names handed: `add_reader :color` marks `color`, `add_flags
  :active, :hidden` marks `active?` and `hidden?`; one that is not a literal
  is `*`.
- **A Rails macro trekr declares is not a macro call here.** `class_attribute`,
  `mattr_*`, `delegate` and the rest of DEC-111's list already declare each
  name they make; recording their calls would mark every class that calls
  them for a string of code that builds its `def`s with `join`, which hedges
  every name. Measured before the filter: mastodon's card sweep went from
  399 residue to 1,419, 602 of them `class_attribute` on
  `ActionController::Metal`.
- **`--def` blames the file before a gem.** A name defined nowhere, called
  in a file whose own marker may make it, says so rather than DEC-126's
  "a gem may generate it".

**Why.** The 0.8.0 hunt's largest under-hedge: Rails' macro shape, a
`ClassMethods` method `class_eval`ing `def helper_made` into its caller.
Nothing marked it, so `Widget#helper_made` was "no such method", exit 1,
and `--def` on a call of it blamed a gem.

**Measured.** Every gold verdict, the 21,154 clicks, the rails `--refs`
queries, `--dead` on rails, activerecord and mastodon, and both card sweeps
unchanged. Rails' index holds 129 macro marks and 21,601 body calls (the
database 3% larger); `Minitest::Expectations#must_be_empty`, which
minitest's `infect_an_assertion` writes in a string, is now residue naming
it where it was "no such method".

**Not done.** A macro called from `included do`, from a block, or from a
class method is not a body call. Reading the macro's string at each caller,
with the names substituted, is DEC-163's, and only where both are in one
file. *DEC-212 makes a string macro's methods at a caller in another file.*

## DEC-163 — A string of code is read with the names it is handed, and its calls with them

**Decided.** Two readings of DEC-132 widen.

- **A value names calls as it names `def`s.** A call in a `class_eval`
  string whose name or receiver interpolates the loop's value
  (`helper_#{n}`, `run_#{kind}`) is read once per value, as the `def`s that
  interpolate it are: it is that value's call. What mentions no value is
  still read once.
- **A method's string is read where a class body hands it names.** A string
  of code in a method that interpolates only the method's positional
  parameters, or none, is kept; where a class or module body in the same
  file calls the method on itself with literal names, the string is read
  there as that class's code, with the names in place. A class method's is
  its own class's (`make :fast` in the class that defines `self.make`),
  whose value-free code was already read where it is written, so only what
  depends on the names is read again. A macro's (DEC-162) is read whole, in
  a class that includes or extends, in this file, a module around the
  macro, or anywhere when the macro is `Module`'s or `Class`'s.
- **What stays unread says so.** A call named by an interpolation no value
  fills — a string no caller in the file hands names, one sent to another
  object (`generated_association_methods.module_eval`), one that cannot be
  read — is recorded by its shape (`assign_nested_attributes_for_*_association`),
  and `--dead` gives each method of that shape in the file the caveat "a
  string of code calls `…`".

**Why.** The 0.8.0 hunt: `--refs V#helper_alpha` resolved with no sites, and
`--dead` called it unreferenced without a caveat, where `%w[alpha beta].each
{ |n| class_eval "def #{n}_x; helper_#{n}; end" }` calls it. And Rails' macro
shape, `add_helper :color` over `class_eval <<~RUBY def #{name}_helper`, left
`Widget#color_helper` unread when both are in one file.

**Measured.** Every gold verdict, the 21,154 clicks and both card sweeps
unchanged. The rails `--refs` queries gain three sites, each a call a loop's
value names: activesupport's `SafeBuffer` writes `to_str.#{unsafe_method}`
per method, now a confirmed `String#downcase` and `String#strip`, and a
`CommandRecorder` string's `execute` symbol is possible. `--dead`:
`Relation#insert!` and `#upsert` move from unreferenced to super-only in
rails and activerecord, reached by `super` from the `def #{method}` that
`AssociationRelation` writes per name; `assign_nested_attributes_for_one_to_one_association`
and `…_collection_association` keep their tier and gain the caveat. A
shape must spell three name characters: `*` alone, as `to_str.#{m}` leaves
where no value fills it, caveated 51 activerecord methods.

**Not done.** A macro in another file is marked (DEC-162), not read: its
string would have to be stored, and rendered by the tree. A subclass calling
its parent's class method, and a call in `included do`, are not read here.
*DEC-212 makes its `def`s from the marker's shapes; its calls still need the
string.*

## DEC-164 — One string makes at most 2,000 methods, a file 20,000

**Decided.** Before a string of code is read once per value, the methods it
would make — values times the `def`s that name one — are counted. Over 2,000
for the string, or past 20,000 for the file with what was read before it,
the string is not written out: its scope is marked (DEC-130) with the count,
`class_eval of 300000 methods, too many to read`, shaped by its `def`s
(DEC-160), and its value-free code is still read once. A macro read at its
callers (DEC-163) counts toward the file's bound.

**Why.** The hunt's stress case, 300 names over a string of 1,000 `def`s,
wrote 300,001 methods: 7.3 s to index and 5–6 s hovers, for a shape no real
codebase needs spelled out, and the answer about any one of them is the
same hedge.

**Measured.** The stress case (300 × 1,000 `def`s each calling `helper`)
indexes and answers three queries in 0.56 s, from 1.34 s; `Huge#n5_7` is
residue naming the marker, `--refs Huge#helper` still finds the 1,000 calls.

## DEC-165 — A custom `new` makes what each of its paths returns, and `super` is followed

**Decided.** DEC-133's custom `new` is read by every value it returns: each
`return` its body reaches outside a block, lambda or nested `def`, and its
last expression. `Other.new(…)` makes an `Other`; `super` (with or without
arguments) makes whatever the next `new` up the class side makes, followed
until one says something else or `Class#new` makes the class itself. A path
that is neither still counts for nothing, as DEC-133 decided.

- **Paths that agree** make that class, as before.
- **Paths that disagree** make the value either. `x = Guarded.new(flag)` is
  one write with each type, as `rescue A, B => e` is (DEC-071): the
  receiver is ambiguous. Where only one type is needed — a chain
  `Guarded.new.x`, a `let`, the implicit subject — nothing is known.
- **The rival has the name.** When a local's writes disagree and the type
  they most agree on lacks the called name while another has it, `--def`
  answers ambiguous with the other's method, and `--refs` counts the site
  `possible` for it. Before, it was residue ("the receiver's type is known")
  and `no_such_method` for the other's method, though a value of the other
  type runs it.

**Why.** The 0.8.0 hunt: `def self.new(f); return super() if f; Engine.new;
end` typed every `Guarded.new` as an `Engine` at 1.0 — confidently wrong
whenever the flag is true — and `class Reset < Factory; def self.new; super;
end; end` stopped at a `new` that says nothing, typing `Reset.new` as a
`Reset` where Ruby's `super` runs `Factory.new`, which makes an `Engine`.

**Measured.** Every gold verdict (graph_weaver, accord, polyid, flipper,
widget_shop, verdict files byte-identical), the 21,154 clicks, the rails
`--refs` queries, `--dead` on rails, activerecord and mastodon, and both card
sweeps unchanged. The rival rung stays general rather than limited to
`local:new`, since no measured answer moved.

## DEC-166 — A call that lands on a `delegate` counts toward what its target runs

**Decided.** `delegate :name, to: :x` records `x` on the method it declares
(the `def` row's `target`; a prefixed one sends another name and records
nothing). When `--refs Owner#name` tiers a call whose lookup lands on such a
delegate, and the delegate is not the method asked about, the call is sent
on to what `x` holds:

- **Typed.** `x`'s reader, looked up on the receiver's side, declares a
  return type; or `x` is `all` or `unscoped` on the class side of
  `ActiveRecord::Base` or a model, which returns the model's relation, an
  `ActiveRecord::Relation`. Where that type's lookup lands on the queried
  method, the site is `confirmed`; where the type is an ancestor of the
  owner, which defines its own (DEC-140), `possible`; otherwise it stays
  excluded as before.
- **Untyped.** `x` holds a value of no known type, which may run any method
  of the name: `possible`.

**Why.** Rails' `Querying` writes `delegate(*QUERYING_METHODS, to: :all)`, so
`Person.delete_by` runs `Relation#delete_by` on what `all` returns. `--refs`
excluded every such call as `different_owner`, landing on the delegate, and
`--dead` called `ActiveRecord::Relation#delete_by` single-caller, its callers
all reaching it through the delegation (the downcast lane's finding).

**Measured.** Every gold verdict (verdict files byte-identical), the 21,154
clicks and both card sweeps unchanged. The rails `--refs` queries move 1,766
sites out of excluded, none into it: 1,759 to confirmed, all a model's class
method through `Querying`'s delegation (`QueryMethods#where` 42 → 1,228
confirmed, `FinderMethods#first` 6 → 491, `#exists?` 3 → 91), and 7 to
possible through a delegate of no known type (`TimeWithZone#to_s` via
`Duration::Scalar`'s `to: :value`). Sampled, each is the method that runs.
`--dead` on rails: 2,279 → 2,249 candidates (activerecord alone 1,611 →
1,581). Gone are the relation methods only a delegation reaches
(`Relation#delete_by`, `#destroy_by`, `#find_or_create_by!`,
`FinderMethods#second!`, `Calculations#async_count` …) and methods reached
through a `delegate … to: :scheme` whose reader is untyped
(`Encryption::Scheme#downcase?`, `#with_context`); 13 more move from
convention-only or unreferenced to single-caller. mastodon unchanged.

**Not done.** `--def` on `Person.delete_by` still answers the delegate, which
is the declaration a reader clicks through; it could name the relation's
method as well. *DEC-211 does.* `to: :class` and a `to:` constant are untyped.

## DEC-167 — An editor answers inside a string of code trekr reads

**Decided.** The extractor keeps, for each string of code it reads (DEC-132),
the text as written with the stand-in for its values, and where each byte of
it is in the file (`Facts::strings`, never stored). The LSP's variable answers
— hover, highlight, definition — read the string's locals as the file's: its
own parse of the text, each mention placed where its bytes are. A name a
value was substituted into, and an instance variable, whose owner the
string's own parse cannot say, are left out. And a hover on a `def` in such a
string that makes one method per value lists them all: "One of 3 methods this
line makes: `Widget#alpha_x`, `Widget#beta_x`, `Widget#gamma_x`", where it
showed the first.

**Why.** The 0.8.0 hunt: in a `class_eval` heredoc trekr reads, a local had no
hover, highlight or definition, though the method calls around it answered,
and hover on `def #{n}_x` showed `W#alpha_x` alone.

**Measured.** Every gold verdict and the 21,154 clicks unchanged (the CLI
answers are untouched: only the editor's variable answers read the strings);
the extension's e2e suite passes against this build.


## DEC-180 — The stdlib is a checkout per Ruby, and an app's own copy of a default gem hides the stdlib's

Supersedes DEC-153's "not indexed, yet".

**Decided.** A checkout that resolves gems or names a Ruby indexes that
Ruby's standard library, `<prefix>/lib/ruby/<abi>/`, as a checkout of kind
`stdlib`, once per machine and shared like a gem version. The Ruby is chosen
as DEC-152 chooses one: the version `.ruby-version` or the Gemfile names,
matched to an rvm, rbenv, asdf or Homebrew install (`3.4` meaning the highest
3.4); the Ruby `$GEM_HOME` belongs to; the `ruby` on `$PATH`. A lockfile
does not change the choice: it pins gems, not the Ruby.

*A subset.* Left out (`gems::stdlib::SKIPPED`): bundler's and rubygems'
internals, keeping `bundler.rb` (`Bundler.require`) and `Gem::Version`,
`Gem::Requirement` and `Gem::Specification`; irb, rdoc, reline, readline,
did_you_mean, error_highlight, syntax_suggest, prism, `ruby_vm/`,
`bundled_gems.rb`; files of top-level `def`s (`un.rb`'s `cp`, `mkmf.rb`'s
`have_header`), which would read as private methods of every object;
opt-in core extensions (`json/add/*`, `psych/y.rb`, `objspace/trace.rb`),
which would tell every app that `Time#to_json` exists; and
`unicode_normalize/tables.rb`, which is data. Ruby 3.4.9: 179 of 981 files.

*Default gems.* Each default gem's files are read from the gemspec rubygems
wrote in `specifications/default/` (`s.files`, with Prism) into
`default_gem`, and each bundled gem's name into `gem_use.name`. When the
tree is built, the stdlib files of every default gem the app bundles by name
are hidden (`Store::tree_roots`), so an app bundling json 2.21.2 answers from
its json and never from 2.9.1's. A default gem the lockfile names at the
version the stdlib ships is found there (`gems.from_stdlib`), where it was
"not installed" or DEC-153's "not indexed".

*Layering.* core → stdlib → gems → checkout, the stdlib first by kind rather
than insert order, so a gem indexed before the stdlib still reopens it.

*Ruby's own.* A block handed to a stdlib method runs where it is written, as
one handed to core does (DEC-084's rule): `Dir.mktmpdir do … expect … end`
answered `expect` while `mktmpdir` was unknown, and stopped once the stdlib
defined it — two of graph_weaver's gold sites went correct → residue until
the rule counted the stdlib.

**Why the hiding is per app, at tree time.** DEC-153 turned down one
checkout holding every default gem because two JSONs would be a confidently
wrong answer, and filtering the checkout's file set would make it depend on
who indexed last. The file set here never changes; what an app sees is
decided from its own `gem_use` when its tree is built, and folded into the
snapshot key. A default gem's own files are the unit, so the stdlib code it
uses but does not own (`random/formatter.rb` for securerandom) stays.

**Measured** (against main at the dyn merge, with DEC-181 and DEC-182; Ruby
3.4.9). Confidently wrong is unchanged in every gold set (graph_weaver 3,
accord 0, polyid 0, flipper 7, widget_shop 1 app / 19 gem). Correct rises in
each: graph_weaver app 444 → 449 and gem code 132 → 153, accord gem 125 →
138, polyid app 361 → 363 and gem 151 → 157, flipper app 351 → 353 and gem
156 → 177, widget_shop gem 1,595 → 1,621 — calls into `FileUtils`, `Set`,
`OptionParser`, `Psych`, `Dir.mktmpdir` that were "nothing known". Ambiguous
answers that name the wrong owner first rise by three (flipper gem 2 → 3,
widget_shop gem 79 → 81). Clicks over the 13 dogfood repos: 1,917 → 1,873
empty and 3,177 → 3,186 unsure of 21,154; of the misses, "defined nowhere
indexed" 427 → 389, "unindexed ancestor" 171 → 166, "known type, method not
found" 221 → 212, while "untyped local" (+9) and "symbol argument" (+12) grow
as calls that stopped at an unknown class now stop one step later. A sweep of
50 stdlib methods' `--refs` on rails: 33 move from residue or `no_such_method`
— confidently wrong for `Pathname#join`, `URI.parse`, `SecureRandom.hex`,
`Time#iso8601`, `Net::HTTP#request`, which a gem reopening the class had
made "known" — to resolved with sites (`Pathname#join` 12 confirmed, 489
possible); none moves the other way. The existing 40 rails `--refs` queries
are unchanged but `Time.parse`, whose 45 `JSON.parse` sites are now excluded
as another owner's. `--dead`: rails and activerecord unchanged (one method
unreferenced → override), mastodon gains one single-caller. mastodon cold
index 4.6–5.2 s either way (page cache dominates), warm 0.33 s, store +2.7 MB
(136 → 139 MB); the stdlib alone indexes in 0.19 s into 2.8 MB.

**Not done.** `RUBY VERSION` in a lockfile is not read; rbenv's shims give no
prefix, as in DEC-152. A default gem whose lockfile version is another
Ruby's default (json 2.7.2 on Ruby 3.4) shows the chosen Ruby's copy. A
compiled extension's methods are hedged, not known (DEC-181); the `rbs`
gem's stdlib signatures would type them.


*Amended by DEC-242:* every checkout gets a Ruby — the one it names,
`$GEM_HOME`'s, the `ruby` on `$PATH`, else the only one installed — since
its core is that Ruby's (DEC-240).

*Amended:* the opt-in extensions (`json/add/`) are left out of a bundled
gem's `lib/` as they are out of the stdlib: an app that bundles json was
told `Time#to_json` is `json/add/time.rb`'s, which it never requires.

## DEC-181 — A stdlib class that is partly compiled hedges a name its Ruby lacks

**Decided.** Indexing a stdlib lists its compiled extensions (the `.so`,
`.bundle` or `.dll` files under the directory holding `rbconfig.rb`, by the
feature `require` names them with) and records, in `compiled`, each Ruby
file whose classes are partly compiled: one that requires an extension of
its own family — the feature's first part begins with the file's
(`monitor.rb` → `monitor.so`, `date.rb` → `date_core`, `erb/util.rb` →
`erb/escape`) — one under an extension's directory (`openssl/*`,
`psych/*`, `json/ext/generator/*`), or one that requires a loader that only
does the first (`digest.rb` through `digest/loader`). Every class and module
such a file opens gets a `dynamic` marker (DEC-130) whose maker is `compiled
extension <feature>`, so a name its Ruby lacks is residue naming the
extension — "Monitor defines methods its source does not name (compiled
extension monitor, …/monitor.rb:258)" — where it was "no such method". A
class core declares is never marked: `psych/core_ext.rb` reopens `Object`,
whose C half is Ruby itself and whose stub says what it has.

**Why.** Indexing the stdlib's Ruby turned residue into a confident "no such
method" for C methods: `Monitor#synchronize` and
`Digest::Instance#hexdigest` in the rails `--refs` sweep, and every card on
Monitor, Psych, Ripper, Socket and `OpenSSL::Cipher`. The same was already
true of `Pathname#exist?` wherever a gem reopens Pathname.

**Family, not any require.** Counting every require of an extension marked
`PP::ObjectMixin`, because `pp.rb` requires `io/console` for the terminal's
width; `Object` includes it, so every card on every class hedged (rails: 31
cards no_such_method → residue). The family rule leaves 81 files of Ruby
3.4.9's 179 marked, most of them psych's and openssl's, whose pure-Ruby
classes (`Psych::Visitors::*`) hedge more than they need to.

**Measured.** With the hedge, the rails `--refs` sweep's
`Monitor#synchronize` and `Digest::Instance#hexdigest` answer residue with
their sites (51 and 26 possible) rather than `no_such_method`, and cards on
Pathname, Digest, Ripper, Socket, Psych and `OpenSSL::*` hedge; gold verdicts
and click totals are the build's without it.

*Narrowed by DEC-220:* a compiled method RBS declares is now declared by the
stdlib stub, so `Monitor#synchronize` resolves; the hedge stays for a name
neither the Ruby nor its RBS writes.

## DEC-182 — A stdlib method the core stub also writes takes the stub's return

**Decided.** A method in the stdlib with no return type of its own takes
the core stub's `sig` for the same owner and name (`Tree::declared_returns`,
as it does an `.rbi`'s). The location stays the stdlib's.

**Why.** `core.rb` stubs `Set` (core in Ruby 3.5) and a few methods the 3.4
stdlib also writes — 19 in all: `Set`'s 16, `Kernel#pp`,
`Enumerable#to_set`, `Dir.tmpdir`. The stdlib's definition wins the lookup,
as real source should, and without this `set.size.even?` lost the
`Integer` the stub had given `size`. A gem's override of a core method does
not borrow: it may return something else.

## DEC-190 — A checkout's root is read off the disk, not asked of git

**Decided.** `scan::repo_root` walks up from the path to the nearest `.git`
it recognises — a directory with `HEAD`, `objects` and `refs`, or a
worktree's `gitdir:` file pointing at one — and returns that directory as the
filesystem spells it (`F_GETPATH` on macOS, since `canonicalize` keeps a typed
case where git's `getcwd` does not). It hands the question to `git rev-parse
--show-toplevel`, as before, whenever git might answer differently: `GIT_DIR`,
`GIT_WORK_TREE`, `GIT_COMMON_DIR` or `GIT_DISCOVERY_ACROSS_FILESYSTEM` set; a
`.git` it would not accept; a config naming a work tree, a bare repository or
per-worktree config; a directory owned by someone else (`safe.directory`); a
filesystem boundary crossed; a path inside the gitdir itself; or no `.git`
found at all, so every refusal is still git's own. `GIT_CEILING_DIRECTORIES`
is honoured rather than deferred: the walk stops below the nearest ceiling
above the start, which is git's rule, and this machine's shell sets one.

**Why, measured.** The user's report: `--symbols` was "surprisingly slow per
unit of output and much slower than rq's". It already parsed the file fresh
rather than reading the store, so "just parse fresh" was already the design. Timed inside the process on rails'
`associations.rb`: parse 0.9–2.2 ms, emit 0.1–0.2 ms, store open and roots
1–4 ms, and **`git rev-parse` 16–23 ms**. The text outline never asked git and
was already rq's speed; `--json` asks where the file sits, and the fork was
most of its answer. Every other query asks the same question for its
checkout, so each paid it too.

Rails, a store holding rails, discourse and mastodon with their gems, each
build answering from its own store, 21 interleaved rounds, medians (p90), load
6–7 from other work:

| | before | after | rq 0.59 |
| --- | ---: | ---: | ---: |
| `--symbols associations.rb --json` | 21.8 ms (25.4) | **13.7 ms** (16.3) | 12.1 ms (15.2) |
| `--symbols associations.rb` (text) | 11.4 ms (12.2) | 11.3 ms (11.7) | 12.4 ms (13.1) |
| `--ancestors ActiveRecord::Base` | 38.7 ms (47.4) | **30.6 ms** (35.6) | |
| `--def base.rb:300:5` | 57.0 ms (65.9) | **40.9 ms** (47.4) | |
| `--refs ActiveRecord::Persistence#save` | 290 ms (371) | 283 ms (358) | |

Each store's snapshot was warm, so the `--ancestors` and `--def` rows differ
by the fork alone. What is left of an outline is the process: a `true`
spawned the same way takes 7–9 ms.

**Checked.** The root is part of every JSON answer (`root`), so equivalence is
the claim: `--symbols` byte-identical to the previous build in 1,104
comparisons (184 files across rails, discourse, two installed gems and a file
in no repository; `--json`, `--ndjson` and text; from inside the checkout and
from `/tmp`). A unit test holds the walk equal to `git rev-parse` on a plain
repository and a nested directory, a repository nested inside another, a
linked worktree, and a typed-in wrong case on a case-insensitive volume, and
requires `None` — git decides — for a gitdir, an unrecognisable `.git`, and a
ceiling between the start and the repository. The gold sets, the rails
`--refs` differential, `--dead` and the click replay are unchanged (DEC-192
lists them).

**Rejected: opening the store read-only for `--symbols`.** An outline opens
the store only to learn which indexed checkout holds the file; a read-only,
no-migration open would save 1–2 ms and stop an outline from rebuilding a
store of another schema version. Every other command rebuilds it the same
way, so an outline is not the place to change that, and the milliseconds are
inside the noise of a process spawn.

## DEC-191 — The file map inserts and updates; it never replaces

**Decided.** `Store::write` writes a new path with `INSERT` and an edited
one with `UPDATE`, where it used `INSERT OR REPLACE` for both (DEC-048's
delta is otherwise unchanged). `--index --profile` now names the parts of the
write that follow the rows — `index-rebuild` (DEC-057's sort), `file-map`,
`commit` (where the WAL is written and checkpointed, the app's and the
bundle's) — and `gem-walk`, reading and hashing each gem's `lib/`, so the
phases sum to the wall time; `store-write` is what is left, the rows.

**Why, measured.** The user's report: `--index` of a 100k-file repository took
about ten minutes. A cold index of 100k distinct real files (below) takes
40 s, so the ten minutes was not the cold path. It was the one DEC-057 does
not take: a load that does not double the store — a large repository indexed
into a store that already holds other checkouts, or a second worktree of a
monorepo far from the first. Loading 50k new files (the other 50k known) into
a store of 50k, timed inside the write: **the file map 181 s of a 218 s
index**, the rows 28 s. The profile could not see it: `store-write` was one
number, and the bundle's commit sat outside every phase — 0.6–2.2 s of a
rails, discourse or mastodon index was unattributed.

Sampled, the time was `sqlite3PagerSavepoint`, reached from closing a
statement. `INSERT OR REPLACE` may delete a row as well as insert one, so
inside a transaction SQLite opens a statement journal for it, and releasing
that journal costs in proportion to what the transaction has already written
— nothing on a fresh store, ~1.8 ms a file after a 50k-file write. A plain
`INSERT` or single-row `UPDATE` opens none. The write already knew which
paths were new: it reads the stored map to diff it.

A 50k-file load (c100k below, its other 50k already known) into a store of
50k, a clone of the same store per run, two interleaved rounds, load 7:

| | wall | of which file map |
| --- | ---: | ---: |
| `INSERT OR REPLACE` | 245 s (262) | 181 s |
| **`INSERT` / `UPDATE`** | **72 s** (77) | 1.6 s |

The after column also carries DEC-192's tree (3.8 s). A first index paid it
too, less: timed inside the write before the change, discourse's map, written
after rails' into one store, was 1.2 s and mastodon's, after both, 0.53 s,
where 50k files into a fresh store took 0.3 s. After it, a cold discourse
index's maps, its gems' included, take 0.11 s. Store size is unchanged
(1,579.5 MB both).

**Correctness is DEC-048's test**: a map written, then one with a deletion,
an edit, a rename and an addition, must equal a fresh write of the second
map, surface key included. It covers both statements. The gold sets, the
`--refs` differential, `--dead` and the clicks are unchanged (DEC-192).

## DEC-192 — Every index prepares the tree snapshot (DEC-065 revisited)

**Decided.** `--index` ends — after its answer is printed — by writing the
checkout's tree snapshot when none exists under the key the store now gives
(`Tree::prepare`: the key, a `stat`, and on a miss the assembly DEC-065's
first query ran). It was done only by the index the LSP starts in the
background. A query still builds one when it finds none: after a `--def` that
refreshed an edited file, a snapshot removed by hand, a store another build
wrote. `--profile` reports it as `tree`; the tree layer's own `TREKR_PROFILE`
lines stay out of an index's profile, which is JSON under `--json`.

**Why.** The user's report: the first query after `--index` was surprisingly
slow and the ones after it fast. That was DEC-065's design, not a cold cache:
the first query after an index that moved the key assembled the namespace
and wrote the snapshot, and every later one mapped it. DEC-065 measured that
the cost is the same wherever it lands and put it on the query, because a
foreground index "may never be followed by a query". The cost is the same;
the budgets are not. A query's is tens of milliseconds and an agent issues
several right after indexing — that is why it indexed — while an index's is
seconds to minutes, and the assembly is a few percent of it. The first query
also paid more than the assembly alone: on a store holding three apps and
their gems, discourse's declarations read in 594 ms the first time and 71 ms
warm — pages the index had pushed out of the cache.

**Measured.** A cold index into a fresh store, then the same `--ancestors`
twice, each build on its own store, four interleaved rounds (two at 100k),
medians (p90), load 5–10 from other work. The index column is this build
against main, so it carries DEC-191 as well:

| | `--index` | first query | second |
| --- | ---: | ---: | ---: |
| rails | 3.10 → 3.17 s | 0.21 (0.25) → **0.02** s | 0.04 → 0.02 s |
| discourse | 16.1 → 16.3 s | 0.63 (0.89) → **0.03** s | 0.05 → 0.03 s |
| mastodon | 10.5 → 10.6 s | 0.52 (0.68) → **0.04** s | 0.05 → 0.05 s |
| 100k files (below) | 89 → 96 s | 2.6 (3.1) → **0.05** s | 0.06 → 0.04 s |

`--profile`'s `tree` phase: 0.38 s at 10k files, 1.6 s at 50k, 2.4 s at
100k. After a one-file edit to a discourse clone, six rounds: an edit inside
a method moves no declaration, so the key does not move and neither build
assembles anything (index 279 → 289 ms, first query 55 → 40 ms); an edit that
adds a class moves it, and the assembly moved from the query to the index —
index 363 → 1,133 ms, first query 952 → 46 ms, together 1.31 → 1.18 s.

**Correctness.** The snapshot is the same bytes whoever writes it — the key
is the same function of the store, the assembly the same code — and a query
still checks the header, key and checksum before it maps one. Every answer
below is byte-identical to the previous build, each build answering from a
store it indexed itself: the four gold sets and widget_shop's (every
verdict), 40 rails `--refs` answers, `--dead` over rails and activerecord,
the flipper/faraday probe, and 21,154 replayed editor clicks (misses equal
but for timestamps). The e2e test that required a foreground index to leave
the tree alone now requires it to write one, and the usage test's first
query after an index no longer counts `tree-built`.

**The cost that moved.** Two builds of different tree code on one store key
their snapshots differently and retire each other's (DEC-065 said so); an
interleaved benchmark of two binaries on one store therefore rebuilds on
every query, and is not a measurement of either. Each build here answered
from its own store.

## DEC-193 — Call sites are stored as a posting list: which files call a name, not where

**Decided.** `call_site` — one row per call, ten columns — is replaced by
`call_name(blob_id, name, calls, symbols)`: one row per name a blob calls,
with how many calls and how many of them are a symbol naming the method,
indexed `(name, blob_id)` and `(blob_id)`. Store v48. Every reader of call
sites already reparsed the files the index named: the tiering needs a file's
assignments, which are not stored (DEC-012), and an edit since the index must
still count. The index's only job was to say which files to open, and a
posting list says it. What changed for each reader:

- `files_calling` and the LSP's paged listing read postings. A page is now a
  number of blobs, with every file of each blob in it, so no page splits a
  blob's files; the order of files is the same (blob ids and call rows rose
  together).
- `written_calls` (`--dead`'s cheap filter) sums `calls - symbols` over the
  checkout's postings, capped as before; `--status` and `--gc` sum `calls`, so
  their counts are sites, as they were.
- The bare-name listing (`--refs save`) took its call rows from the index. It
  now takes them from the reparse its tiering already does, so a listed call
  and its tier always come from the same bytes. On an index that is current
  the rows are the same; on a stale one, a call deleted since the index is
  no longer listed from the old facts.

**Why, measured.** The algorithm review (scratch prototype, 600 names, 0
disagreements in files or in `written_calls`) sized it; the numbers below are
this build's. DEC-061 had found call sites 70–78 % of the store and sized a
slimming that kept them; nothing reads them per site any more. DEC-058's
covering `(name, blob_id)` index, deferred to the next schema change, is the
posting list's own.

Against the build before it (DEC-190–192), each on its own store, load
10–18 from other work, so medians of three rounds (two at 100k, four for
discourse) and one significant figure's confidence in the walls:

| cold `--index` | wall before → after | store before → after |
| --- | ---: | ---: |
| discourse + gems | 8.4 → **5.0 s** | 329 → **121 MB** |
| 10k files (below) | 3.1 → **1.9 s** | 145 → **59 MB** |
| 50k files | 26 → **9.7 s** | 725 → **300 MB** |
| 100k files | 58 → **20 s** | 1,532 → **599 MB** |
| 50k new into a store of 50k (DEC-191) | 57 → 48 s | 1,579 → 630 MB |

At 100k the rows' phase fell 32 → 10 s and the index rebuild 16 → 4.9 s. The
load that does not double the store moved least: it still inserts every
definition and constant reference into indexes that outgrew the cache.

Queries that read postings, rails, 11–15 interleaved rounds: `--refs
ActiveRecord::Persistence#save` 240 → 220 ms, bare `--refs new` (13.7k
rows) 1.19 → 1.21 s, bare `--refs save` 268 → 264 ms, `--dead
activemodel/lib` 555 → 570 ms — the same, within the noise. The reparse was
always their cost.

**Correctness.** Byte-identical to the build before it, each on its own store:
the gold sets, 40 rails `--refs` answers, `--dead` over rails and
activerecord, the probe, the click replay, `--status`; and the bare-name
listing for 125 rails names, text and `--json`, the 40 most-called among
them (`new`, `assert_equal`, …). A unit test pages a name's files two blobs at
a time and requires each file once; the pinned plans (DEC-058) read the
postings' covering index and build no temp B-tree.

## DEC-194 — A tree snapshot is keyed by the namespace, not by every definition

**Decided.** Each blob carries a second digest beside `surface`:
`namespace` (`Facts::namespace`), over exactly what the snapshot is built from
— its classes, modules and constants with their positions, and its ancestry
edges other than the dynamic markers — and each checkout a `namespace_key`
folded the way `surface_key` is. A snapshot's key (DEC-065) is made of the
namespace keys; what decides whether a resident tree is rebuilt is a
`stamp` over the snapshot's key and every root's surface key, so a method
edit still rebuilds a tree — its demand-loaded methods and markers — and the
rebuild maps the same snapshot instead of assembling one. Store v49.

**Why.** The algorithm review: the key folded every method definition and its
position, while the snapshot holds no method, so most edits rebuilt a
namespace that had not changed — 3.2–3.5 s at 100k files, on the LSP's
request thread (`state.rs::tree()`) and, since DEC-192, in `--index`. Its
estimate from rails' history: 70 % of modified files move the surface and
36 % the namespace; the rest are method edits, which now keep the snapshot.

**Measured.** One line added inside the last method of `topic.rb`, then
`--index` and `--ancestors`, each build on its own store, load 4–10:

| | `--index` before → after | first query |
| --- | ---: | ---: |
| discourse clone, 6 rounds | 450 → **142 ms** | 18 → 18 ms |
| 100k files, 3 rounds | 2.4 (p90 4.5) → **1.2 s** | 25 → 27 ms |

A new class still moves the namespace and rebuilds, as it must (discourse
438 → 434 ms). The LSP gains the same on every save that touches only
methods: its tree is rebuilt against the same snapshot, mapped, in
milliseconds.

**Correctness.** The snapshot is a function of what `Store::declarations`,
`Store::ancestry` (without dynamic markers) and the file map's paths say, and
the namespace digest covers exactly those fields and the paths; the tree's
methods and markers are loaded from the store on demand, and the stamp moves
with them. A unit test requires a new method, a changed arity and a moved
method to leave the namespace digest alone and a new class, a dropped mixin
and a moved constant to move it; an e2e test requires a method edit to keep
the snapshot file and the query after it to find the new method, and fails
with the snapshot keyed by surface. The gold sets, `--refs`, `--dead`, the
probe and the clicks are byte-identical to DEC-193's build.

**Not done: building the new tree off the LSP's request thread** and
answering from the old one meanwhile, as DEC-044 does for completion. What is
left synchronous is an edit that moves a declaration, where the old tree is
the one that is wrong about it — a save adding a class and a definition
asked at once would answer from before the save. That wants its own
decision, and a measurement of how often it is felt, which the usage log's
`tree-built` flag now counts on its own.

## DEC-195 — Where a 100k-file index goes now, and what did not clear the bar

**The corpus.** Every distinct Ruby file on this machine — the checkouts in
`~/code/lib/ruby` (rails, discourse, mastodon, CRuby, …), every installed
gem, the installed Rubies' standard libraries, other local projects — copied into
one git repository, keeping only the first copy of any content, since
identical bytes are one blob and a duplicate would be a free skip: 85k
files. The last 14.5k are the first 14.5k again with a distinct trailing comment,
distinct blobs whose constants repeat. 10k and 50k are even strides through
the same list. 472 MB of Ruby, 1.5 M definitions, 9.7 M calls at 100k.

**The user's ten minutes** was DEC-191: the file map on a load that does not
double the store. A cold index of the whole corpus was never that slow: 40 s
at the start of this work, the machine shared. What is left, one quiet run,
this branch against main, `--profile`:

| 100k files | main | now |
| --- | ---: | ---: |
| cold `--index` | 42–44 s | **19–21 s** |
| first `--ancestors` after it | 2.4 s | **0.03 s** |
| first `--def` in a module after it | 1.5 s | **0.09 s** |
| store | 1.5 GB | **0.6 GB** |

Of the 20 s (DEC-193's run): the rows 10 s with the parse behind them, the
index rebuild 4.9, the commit 1.2, the scan 0.9, `ANALYZE` 0.8, the tree
1.5, the map 0.6. The parse alone is ~4 s of CPU across 8 workers; the
single writer is still the wall, and its rows are now definitions and
constant references.

**Measured and not taken:**
- **`mmap` off for the indexing connection.** With WAL, a mapped page read
  asks the WAL index first, and a sample of the writer showed
  `walFindFrame` under most page reads. Off: 50k cold 19.1 → 16.8 s, a
  non-doubling load 36.2 → 37.2 s, both inside the noise of four and five
  interleaved rounds at load 7–17.
- **A 256 MB page cache for the write** (DEC-041 and DEC-057 tried larger
  and smaller for other reasons): 50k cold 19.1 → 27.4 s median, worse in
  three of five rounds.
- **Lowering DEC-057's bulk threshold** (drop and rebuild the indexes for a
  load of at least a quarter of the store): a 50k load into 50k, 83 → 68 s
  median over four rounds with a p90 of 115 s, the other lanes running. Worth
  measuring again on a quiet machine now that the rows are fewer; not
  changed on that evidence. *DEC-234 re-measured it: from half the store.*
- **An outline that opens the store read-only** (DEC-190): 1–2 ms.

**Not measured, and the next levers:**
- **Gems one at a time.** `index_gems` walks, parses and writes each gem in
  turn, ~36 files apiece on discourse, so the pool idles between gems and the
  walk (`gem-walk`, 0.37 s of discourse's 6.7 s) runs on one thread. Parsing
  the bundle as one stream while the writer takes gems in order would hide
  both. *DEC-232 does.*
- **The rows.** Definitions and constant references are now most of the
  writer's work; DEC-061's interning of `nesting` and `name` is the sized
  slimming for them.

## DEC-200 — Linearization is memoized per name, and a cycle has one answer

**Decided.** Every name's chain is memoized, sub-chains included, where only
the top-level `ancestors` call was before: a class linearized its whole
superclass and mixin graph afresh, so a checkout paid for `Object`'s chain
once per class. The recursion keeps a stack of frames, and a name asked
again while its frame is on it closes a cycle:

- through a **superclass or mixin edge** — not valid Ruby — it answers
  empty, as before;
- through a **path lookup** — `include Widget::Helpers` in `class
  Widget::Parser::Widget`, where `Widget` is the class itself and `Helpers`
  is found through the `::Widget` it included a line earlier — it answers
  **the chain so far**: prepends, self, the includes read so far, and the
  superclass's chain. That is what Ruby's `ancestors` says at that line, and
  it is how Ruby resolves the constant.

**Every chain is the one its name gets when it is asked first**, whatever was
asked before it. A frame records the outermost frame a cycle under it
reached (Tarjan's low-link); a chain built against an outer frame's partial
chain is not memoized, and one that closed a cycle of its own (an SCC's root)
is reused only when no frame is open — a caller inside that cycle would have
been cut where the memoized walk was not. Everything else is a memo hit.

**Which answer is right, per Ruby.** kramdown's `Kramdown::Parser::Kramdown`
does `include ::Kramdown` and then, in `kramdown/html.rb`,
`include Kramdown::Parser::Html::Parser` and `include Kramdown::Utils::Html`.
`Kramdown` there is the class; Ruby finds `Parser` through the `::Kramdown`
it already includes, so both modules are in its ancestors. Before, the class's
own chain lacked them and listed them unresolved, while its subclasses
(`GFM`, `Markdown`, `SmartyPants`) had them — because a subclass linearized
the parent in a nested walk that did not count as in flight. Both now have
them, pinned by testbed case 200 and by
`a_chain_does_not_depend_on_what_was_asked_before_it`, which asks a cycle's
names in both orders and failed before (a chain cached from inside another
name's walk answered for later askers).

**Measured.** Every class's and module's chain, instance and singleton, in
one tree (`src/tree/dump.rs`), forward and reverse order, against main
(DEC-195's build) — release, one run each, load 7–23 from other work:

| corpus | names | before | after | differ |
| --- | ---: | ---: | ---: | ---: |
| rails | 9,699 | 2.7 s | 0.3–1.1 s | 0 |
| mastodon | 16,825 | 2.0 s | 0.3–1.4 s | 0 |
| 100k files (DEC-195's corpus, no gems) | 72,176 | 649 s | 3.6–4.5 s | 1 |

The one is `Kramdown::Parser::Kramdown`, above. Forward and reverse orders
give identical dumps. 9 of rails' 9,715 memoized chains close a cycle,
10 of mastodon's, 42 of the 100k corpus's.

The pass that finds a module's includers linearizes every class, and a
`--def` inside a module pays it: `respond_to?` in `ActiveSupport::Tryable`,
each build on its own store (DEC-192), medians (p90) of interleaved rounds:

| | main | this, with DEC-201–205 | rounds, load |
| --- | ---: | ---: | --- |
| rails | 0.30 (0.30) s, 52 MB | **0.064** (0.065) s, 42 MB | 11, 4 |
| mastodon | 0.27 (0.29) s, 56 MB | **0.080** (0.086) s, 45 MB | 11, 4 |
| 100k files | 598 s, 403 MB (one run) | **0.51** (0.52) s, 307 MB | 7, 4–6 |

The includers map is also slimmer: it held every (ancestor, class) pair as
two cloned names, 2 M of them at 100k files, and now holds each ancestor's
includers as indices into the classes sorted once — 100k-file `--def`
0.78 → 0.58 s and 435 → 307 MB on its own, rails 56 → 42 MB.

Byte-identical to main on every gold set and widget_shop's verdicts, the
rails `--refs` 40- and 51-query sets, `--dead` on rails, activerecord and
mastodon, the flipper/faraday probe and `make clicks` (108 outputs, each
build indexing its own stores), and on the 100k `--def` above.

**Rejected.**
- **Caching sub-chains and skipping the cache when a cycle guard fired**
  (the algorithm lane's prototype). Same speed, but a subclass reused its
  parent's memoized chain where the build before re-linearized it nested:
  on c100k the three Kramdown subclasses lost their Html modules, and the
  answer still depended on asking order.
- **Empty for every cycle, lookups included** ("members of an SCC see each
  other empty"). Order-independent, and as fast, but Ruby's answer is the
  chain so far: all four Kramdown parsers lose three modules each (the only
  4 of c100k's 72,176 names where the two rules differ; none on rails or
  mastodon).
- **Recomputing an SCC's members after its root closes**, as a full Tarjan
  pass would. Reusing a cyclic chain only at the top level gives the same
  answers with no second pass, and so few chains close a cycle that
  recomputing them nested costs nothing measurable.

- **The includers map in the snapshot** (~0.8 MB on rails, ~8 MB at 100k
  files, a v50 snapshot keyed as DEC-194 keys it). Sampled at 1 ms, the
  100k `--def` spends 0.5 s: 44 % in the includers pass, three quarters of
  that linearizing classes, and 52 % looking `respond_to?` up along each
  includer's chain — which linearizes the same classes if the pass does
  not. A stored map would save the map's own building, ~0.06 s, and put
  linearizing every class into every `--index` (seconds at 100k files) for
  the few queries inside a module. The answer is already half a second
  there and 64 ms on rails. The larger lever is the lookup's own walk:
  `first_in_chain` allocates a `(owner, singleton, name)` key per chain
  element to probe `by_owner`, 42 % of that query. *DEC-231 removes it.*

## DEC-201 — What every definition of a name returns is memoized per tree

**Decided.** The `chain:name` rung — a call on an untyped receiver takes the
class every definition of its name agrees on returning — moves into the
tree as `Tree::agreed_return(name, argc, block)`, memoized there. The rung
keeps the identity rule and builds the receiver.

**Why.** The vote is a function of the tree and the three keys alone, and
it ran per call site that reached the rung: every definition of the name
cloned (`Tree::named`), owners deduplicated with `Vec::contains`, each
declared return resolved to a class — for `h[:a]` in a 100k-file checkout,
a vote over every `[]` in it, at most of 441k sites.

**Where.** On the tree, not keyed by the tree's address in a
`thread_local!` (the algorithm lane's prototype): the tree is immutable
for its lifetime and the memo is freed with it, where an address can be
reused by the next tree a resident session builds. Not a resolve-layer
`Receiver` stored in the tree either: the tree holds tree facts (a class
and two counts), and resolve makes the receiver.

**Measured.** `--refs 'Hash#[]' --include-excluded` on DEC-195's 100k-file
corpus, each build on its own store, two interleaved rounds, load 5–11:
DEC-200 alone 92 s → with this 43 s. Byte-identical to main, as are the
gold sets, the rails `--refs` sets, `--dead` and the clicks (DEC-200's
list).

## DEC-202 — Every definition of a name is shared, not cloned per call site

**Decided.** `Tree::named` returns `Rc<[MethodDef]>`, built once per name
and kept: a name is complete once `ensure` has loaded it (a demand load is
per name, and nothing else adds a definition of that name), so the list
never goes stale. Counting a name's other owners for the receiver-name rung
borrows their names instead of cloning each.

**Why.** Sampled on the 100k `Hash#[]` query after DEC-201, cloning and
freeing `named`'s definitions was 17 % of the tiering thread: the
receiver-name rung asks for the name's pool at every site to count its
competitors.

**Measured.** Same query and conditions as DEC-201's, three interleaved
rounds, load 6–9: 43.0 → 34.5 s. Identical output.

## DEC-203 — Method lookups and class-side chains are memoized per tree

**Decided.** A lookup — `(fqn, singleton, name, as_self)` — is memoized as
where it landed: the definition's index in the tree's methods and the
owner it was found through, from which the method is built when asked. A
class side's lookup chain — the superclass walk with each level's
extends, prepends and concern `ClassMethods` resolved — is memoized per
`(fqn, as_self)`. An instance's chain is not copied at all: it walks the
memoized `Rc<Ancestry>`.

**Why.** After DEC-202, `lookup_along` was 55 % of the 100k `Hash#[]`
tiering thread, two thirds of it rebuilding the chain it was about to walk
— a copy of the ancestors per instance lookup, the whole class-side walk
per class lookup. Both are functions of the tree once the name is loaded,
as `named` is.

**Measured.** The 100k `Hash#[]`, three interleaved rounds, load 6–9:
34.5 → 24.5 s with the first cut. That cut memoized every chain as a list of
pairs and every lookup as a cloned method; a `--def` in a module, where
nearly every lookup is asked once, paid for it — rails 53 → 81 MB, the
100k corpus 413 → 642 MB, and no faster. Sharing the ancestry for instance
chains and keeping landings instead of methods brought those to 55 and
434 MB, and without a lookup memo at all to 52 and 412 MB — but the memo
is worth 21.7 → 18.7 s on `Hash#[]`, where a site's receiver types repeat.

## DEC-204 — A file's local flow is worked out on the parse workers

**Decided.** `--refs` (and everything that gathers references) parses a
chunk of files on the pool and tiers them on one thread. A file whose calls
to the name have a local or another call as receiver now has its local
flow analysis — which reparses the file with Prism — run beside its parse,
on the pool. A file the predicate misses still works it out when asked, so
no answer can change; it is a head start, not a new path.

**Why.** After DEC-203 the flow analysis was 38 % of the tiering thread on
the 100k `Hash#[]`. Of that query's 44,867 files, 23,836 needed it; the
predicate picks 33,431, all 23,836 among them. Picking only files whose own
call has a local receiver picks 21,695, all needed but 9 % short; picking
every file with any local receiver picks 35,639.

**Measured.** 24.5 → 19.8 s, same conditions.

## DEC-205 — `--refs` parses the next chunk while it tiers this one

**Decided.** The tiering thread waited for each chunk's parallel parse
before starting on it. The next chunk's parse now runs on a scoped thread,
through the same pool, while this chunk is tiered. A single query still
keeps only the chunk being tiered (and the one being parsed).

**Why.** After DEC-204, 42 % of the tiering thread's samples were waiting on
the pool.

**Measured.** 19.8 → 16.8 s, same conditions; the thread now waits 8 % of
its time.

**All of DEC-200–205 together**, against main, each build on its own store:

| | main | now | rounds, load |
| --- | ---: | ---: | --- |
| 100k `--refs 'Hash#[]'` (441k sites) | 460 s, 2.1 GB (one run) | **15** (p90 19) s, 2.2 GB | 3, 6–8 |
| rails, the 51-query `--refs` set, whole set | 21.9 s | **10.6** s | 5, 6–13 |
| rails `--def` in a module | 0.30 s, 52 MB | 0.064 s, 42 MB | 11, 4 |
| 100k `--def` in a module | 598 s, 403 MB (one run) | 0.51 s, 307 MB | 7, 4–6 |

Every output byte-identical to main's (DEC-200's list, plus the two 100k
answers).

**Not done, and the next levers.**
- **Tiering on every worker.** The tiering thread is now the whole wall.
  With DEC-200's memos every answer is a function of the tree alone, so
  each worker could tier its own files against a tree of its own over the
  shared snapshot (`Tree` is a `RefCell`, so one per thread); the cost is a
  tree build and a method table per worker, and a `gather_refs` that takes
  a way to make trees rather than a tree. Not clean enough for this change.
  *DEC-233 measured it: not taken.*
- **Memory.** The 100k `Hash#[]` peaks at 2.4 GB RSS: `--json` builds the
  whole answer as a `serde_json::Value` (with `rooted` rewriting it) before
  printing — 1.6 GB without `--json` — and 441k references are held to be
  sorted. Streaming the array would take most of the difference. *DEC-230
  streams it.*
- **`first_in_chain`'s key** (DEC-200's list): a `(owner, singleton, name)`
  string key per chain element to probe `by_owner`. *DEC-231.*

## DEC-220 — The stdlib's RBS signatures type its chains, and declare its compiled half

**Decided.** `script/stdlib_sigs.rb` reads the `rbs` gem's signatures for
the libraries DEC-180 indexes (39 of rbs 3.8.0's 59) and writes two
checked-in stubs, both served only when the checkout's stdlib is indexed:

- `src/tree/stdlib.rb`, the compiled half: every method the Ruby reports
  with no source (`Pathname#read`, `Digest::Class.hexdigest`,
  `Date#strftime`) — 1,140, 573 of them typed — and every class a
  library's RBS declares. Served per top-level owner as
  `<core>/stdlib/Pathname.rb`; its methods are declarations
  (`defined_via: rbs`), indexed before the checkout's so a reopening
  answers with its own. A class counts only where no Ruby file declares it
  (`Digest::SHA256`, `OpenSSL::Digest::SHA1`), and only then do its
  superclass and mixins; a method only on an owner the tree knows.
- `src/tree/stdlib_sigs.rb`, the Ruby half: 792 methods written in Ruby
  that RBS types (`Random::Formatter#hex`, `Time.parse`, `Pathname#join`).
  Never a location; each lends its `sig` to the real definition, as the
  core stub does `Set`'s (DEC-182), and to a core-stub method RBS types in
  a library (`Time.parse`, `Dir.tmpdir`).

The rules are `core_sigs.rb`'s (DEC-077): one `sig` per call shape only
where every covering overload agrees, and none for a union, an optional,
`bool`, `self` or an element type. Returns are written from the top
(`::String`), since a stub's nesting would find `Psych::Set` for `Set`. A
return must be a class nothing subclasses, as core's must — a module
never is. Which methods exist, who owns them and which are compiled is
asked of the Ruby itself, run without rubygems so that an installed
digest 3.2.1 cannot stand in for the stdlib's.

**Why checked in, and one version for every Ruby.** The engine runs no
Ruby (PLAN §4), and reading RBS at index time would need the `rbs` gem, a
Ruby to run it, and a second parser in Rust — for a file that changes when
Ruby does. Generated once, the stub is deterministic, reviewable, and 127
KB (core.rb is 45). Served for every Ruby like core's, it is safe because
RBS's stdlib signatures barely move: the generator run against rbs 4.2.0
types 1,338 methods to 3.8.0's 1,365, and of the 1,314 both type, none
disagree. What does move is which libraries a Ruby has, and a stub method
on an owner the tree does not know is dropped. The generator refuses a Ruby
other than 3.4, and the stub's header names the rbs and Ruby it came from.

**Why a `def` is cut out and extracted when its name is asked.** Parsing
both stubs whole took ~12 ms of every tree build (20 → 33 ms for `--def` on
a one-file app). `corelib::cut` splits the generated text at each `def`,
wrapping it in its owner's compact name and visibility, and the tree
extracts one the first time its name is loaded, as the index's are; a test
holds every cut `def` to the rows the whole file extracts to. A lookup that
never reaches the stdlib now costs nothing (20.1 vs 20.5 ms), and one that
does a few ms.

**Measured** against main (92b9622), each build on its own store, Ruby
3.4.9.

Gold sets: confidently wrong unchanged in every set (graph_weaver 3 app /
2 gem, accord 0 / 1, polyid 0 / 5, flipper 7 / 8, widget_shop 1 / 19), and
correct unchanged. Five residues that offered the truth no longer do
(accord 1, flipper 1, widget_shop 3, all gem code): the eight candidates
shown now include a stub declaration (`OpenSSL::X509::Store#chain`,
`OpenSSL::OCSP::Response#status`, `OpenSSL::Config.load`) ahead of it.
Eleven correct chain picks gain confidence (0.05 → 0.27, 0.17 → 0.24: a
stdlib method of the name agrees) and seven lose a hundredth (0.08 → 0.07:
one more class defines `id`), and polyid's 12 `create` residues go from
truth-absent to declaration-offered.

Clicks over the 13 dogfood repos: definition misses 5,059 → 5,056 of
21,154, the "chained receiver" bucket 807 → 797; "typed, with competitors"
238 → 247 as calls that were untyped become typed but contested.

Rails `--refs` (68 queries: the 40 of DEC-200 plus 28 stdlib and core
methods): 44 unchanged. Among the 40, only chains through a stdlib return
move — `Date._parse(s).fetch` types `Hash`, so `Hash#fetch` confirmed
27 → 60 and those 33 leave `ActiveSupport::Cache::Store#fetch`'s possible;
`Digest::SHA256.hexdigest(s).first(10)` is a String, so six `first` sites
leave `Array#first` and `FinderMethods#first` for ActiveSupport's. Among the stdlib ones:

| confirmed / possible / excluded | main | this |
| --- | ---: | ---: |
| `Pathname#exist?` | residue, 0 / 220 / 303 | 5 / 37 / 481 |
| `Pathname#read` | residue, 0 / 399 / 238 | 1 / 398 / 238 |
| `Pathname#expand_path` | residue, 0 / 9 / 248 | 9 / 0 / 248 |
| `Pathname#join` | 12 / 489 / 584 | 15 / 487 / 583 |
| `Monitor#synchronize` | residue, 0 / 51 / 109 | 10 / 41 / 109 |
| `Digest::Class.hexdigest` | residue, 0 / 26 / 18 | 3 / 5 / 36 |
| `Date#strftime` | no such method | 9 / 43 / 18 |

The 178 `exist?` sites newly excluded pass an argument (`File.exist?(p)`),
which `Pathname#exist?` never takes; the 18 `hexdigest` ones are
`OpenSSL::Digest::SHA1.hexdigest`, another owner. Every newly confirmed
site sampled (24, across eight queries) is right: `@mutex = Monitor.new`,
`yaml = Pathname.new(path)`, `Pathname(file).expand_path`,
`parts = Date._iso8601(str)`, `Digest::SHA1.hexdigest(secret).first(4)`.
`--dead` on rails and activerecord: unchanged.

**The named costs.**

- *More definitions, more competitors.* A stdlib method that returns
  another class than core's same-named one makes `chain:name` refuse:
  `create_table_info.sub(…).strip` loses String to `Pathname#sub`,
  `args.flatten.join` Array to `Set#flatten`, `fn.bind(self).call` Method
  to `Socket#bind`. Ten confirmed sites fall to possible across the
  core queries, against eleven gained; none is wrong either way.
- *Residue lists fill.* OpenSSL alone declares 658 methods, and a residue
  shows eight candidates.

**Left out, and why.**

- `URI.parse(s).host`: `URI.parse` returns a union of URI's ten classes,
  and `URI::Generic` is subclassed, so a `request_uri` would be looked up
  where it is not.
- `JSON.parse(s).fetch`: RBS says `untyped`, rightly — it is whatever the
  JSON was.
- `Set.new.add(x).include?`: `add` returns `self`, which this change reads
  as DEC-077 does (not at all). Reading `self` as the identity rule would
  type every `-> self` in core too, and wants its own measurement.
- `Tempfile.new.path.upcase`, `URI::Generic#host`: `String?` is an
  optional. `Logger.new(io).info` already resolved; it returns `true`.
- `Time#iso8601` on a Ruby-3.4 `Time`: compiled into core, and core.rb does
  not list it. The stdlib's `time.rb` still writes it, and that `def`
  borrows RBS's return, which is what types `Time.parse(s).xmlschema`.
- Libraries with no Ruby at all (`stringio`, `strscan`, `zlib`, `etc`,
  `pty`, `io/console`): DEC-180 does not index them, so nothing owns the
  stubs. A class Ruby declares keeps only the mixins its Ruby writes: a
  mixin added in C (`Digest::Class` includes `Digest::Instance`) is not
  added to it.
- An app's own copy of a default gem (json from the bundle) is not the
  stdlib's, and its Ruby methods borrow nothing; the compiled stubs still
  answer on its classes, which it compiles the same way.

*Reverses if:* a gold set shows a stub `sig` making an answer confidently
wrong, or a Ruby whose RBS disagrees with 3.8.0's on a method both type.

*Superseded by DEC-240:* the stubs are no longer checked in, generated once
for every Ruby, or checked against a running Ruby. They are read at index
time from the rbs gem the app's Ruby carries, with this entry's rules, and
which methods are compiled is inferred from the index. The lazy
`corelib::cut` stays, and now serves core too.

## DEC-210 — The app's own checkout is the last layer, whatever the insert order

**Decided.** A tree's rows are layered by checkout kind — the Ruby's stdlib,
then the gems, then the checkout the tree is for — rather than by the order
the checkouts were first indexed (`Roots::layer`; SQL's `layered`). Gems keep
insert order among themselves. And of the superclasses one class's
declarations write, the first that resolves to a class is taken, not the
first in layer order.

**Why.** ARCHITECTURE said core → stdlib → gems → checkout, and DEC-180 made
the stdlib's place explicit, but the rest followed `checkout.id`: an app is
indexed before its gems, so its rows came first and a gem's came after. The
lookup takes the last definition in an owner, so a gem that reopens a class
and defines the same method as the app won over the app's — the reverse of
Ruby, which loads the bundle first and the app's definition last. A card,
`--def` and every `--refs` tier on such a method answered the gem's
(e2e: `the_apps_reopen_answers_over_a_gems`).

**The superclass pick.** Ruby requires every `class X < Y` of one class to
name the same superclass, so which declaration is read does not change the
class — only whether the index can read it. Layer order had been choosing:
an app's tapioca `.rbi` came first and gave `Concurrent::Map` and
`Concurrent::Synchronization::LockableObject` their runtime superclass;
with the app last, concurrent-ruby's own `class LockableObject <
LockableObjectImplementation`, a constant assigned a `case`, came first and
cut both chains. Measured before this rule: widget_shop's gem sites lost 11
verdicts (5 correct → wrong, e.g. `Event.new` answering sorbet's
`ClassOverride#new`; 6 correct → residue, `synchronize`, `Map#delete`) and
graph_weaver's 2, confidently wrong 19 → 24 and 2 → 3. With it, none.

**Measured** (against main 92b9622; the same moves again on 99a11f7, with
DEC-220's stubs, where widget_shop's gem ranking goes #1 8.7 % → 12.8 % on
716 → 720 offered). Every gold verdict unchanged in all five
sets; one widget_shop gem site's confidence moves 0.25 → 0.33. What moves is
residue and ambiguous candidates' order where two share an owner: the gem's
real `def` now lists ahead of the app's tapioca `.rbi` stub of it, as it
should (`ActiveSupport::LogSubscriber#fetch_public_methods`, the association
builders' `macro` and `valid_options`). Where the truth was offered, the
gem sites' ranking went #1 8.6 % → 12.7 %, MRR 0.411 → 0.436, on 719 → 723
offered (widget_shop), and #1 10.8 % → 13.5 %, MRR 0.449 → 0.462 on 37
(graph_weaver). The rails 40- and 51-query `--refs` sets, `--dead` on rails,
activerecord and mastodon, and the 21,154 clicks are unchanged: none of those
corpora has a gem and an app defining one method.

*Reverses if:* gems' order among themselves is ever read from the Gemfile or
a require graph; that is still insert order, which a lockfile lists
alphabetically.

## DEC-211 — `--def` on a call that lands on a `delegate` follows it

**Decided.** When a call's lookup lands on a `delegate … to: :x` whose `x`
has a known type — the reader's declared return, or a model's relation for
`all`/`unscoped` (DEC-166's `delegated`) — and that type's lookup finds the
name, `--def` answers the method that type runs: its owner, its kind, and
two sites, the target's first and the delegate second. `resolved_via` is
`delegate`, `receiver_type` the target's type, and `reason` says which
delegate sent it where. The target type is a bound (DEC-140), so where an
indexed subclass of it — or a module a subclass of it mixes in — overrides
the name, the answer is `ambiguous`, lists each override as a candidate, and
its confidence is the share, as DEC-081 does for `self`; with none indexed it
is `resolved`. A delegate to a value of no known type is still the
declaration it was. Hover says the call was sent on by the delegate, and
go-to-definition offers both sites.

**Why.** Person.delete_by stopped at `Querying`'s `delegate … to: :all`: the
line a reader clicks through, but not the code that runs, which `--refs`
already counted (DEC-166). DEC-166 left it open.

**Two sites, not one.** A TracePoint gold set records the delegate: Active
Support writes the delegating method with the delegate's own file and line.
Answering the relation's method alone would score every such site wrong;
keeping the delegate as the second site keeps it correct, and an editor's
peek shows the declaration beside the code it sends to.

**Ambiguous only with a named override.** Every delegate target trekr can
type today is a bound — a `sig`'s return, or the relation `all` returns,
which is the model's own subclass of `Relation` and, inside `scoping` on an
association, an `AssociationRelation`. Calling each one ambiguous without a
rival to name would withhold confidence from `Person.where`, which has none;
the override search is what makes the bound matter.

**Measured** (against DEC-210). Every gold verdict unchanged; 9 of polyid's
sites (`User.where`, `.joins`) now answer through the delegate, still
correct, `resolved_via` `const` → `delegate`. The clicks, both rails `--refs`
sets and `--dead` are unchanged: they do not read `--def`'s answer. On rails,
`Book.where` answers `QueryMethods#where` resolved, and `Book.insert_all`
answers `Relation#insert_all` ambiguous at 0.5, naming
`AssociationRelation#insert_all`.

*Kept after the comparison re-run (2026-09-30):* `script/compare.py` scores
only the first location an editor is handed, so the relation's method first
counts as wrong@1 there wherever Ruby's trace recorded the delegate: 6 of
discourse's 500 sites (`Model.where` ×5, `Model.pluck`), which an August
build that answered the delegate scored correct. The delegate line is
`delegate(*QUERYING_METHODS, to: :all)`, forty names and no code; the
method it sends to is what a reader clicking `where` wants, and the delegate
is still the second location. The comparison reports this as a tax beside
the declaration's, and `script/gold.py`, which takes either site, is
unchanged.

## DEC-212 — A string macro in another file defines what its callers name

**Decided.** When a class body in one file calls a macro written in another
— an instance method whose body `class_eval`s (or `module_eval`s) a string
that interpolates its parameters, marked by DEC-162 — and hands it literal
names, each `def` in the string whose name those names spell is a method of
the calling class: owner the caller, on the side the `def` says, its site the
macro's `class_eval` line, `kind` definition, any arity. The tree builds it
from what the store already has: the marker's shape (`{0}_helper`,
`_run_{0*}_callbacks`) and the `body_call` row's literal arguments. It is
consulted only where Ruby's lookup along the chain finds nothing (`made_along`),
so it never reorders what the chain's own source defines. A name a caller
does not spell (`add_helper some_name`), or a `define_method` macro, stays a
marker as before; a caller in the macro's own file is DEC-163's.

**Why.** DEC-162 marked these methods and DEC-163 read the string only when
the macro and its caller share a file. Across files — Rails' shape, a
concern's `ClassMethods` in one file and the models that call it in others —
`Widget#color_helper` was residue naming the macro though `add_helper :color`
states it, and `--def` on a call of it offered guesses.

**Only on a miss.** Built eagerly, with the methods of each name as it is
loaded, a warm `--def` on rails went 42 → 147 ms and 19 → 42 MB: placing the
macros resolves every caller's class side. On a miss it is the same pass the
residue path already made (DEC-162's markers), and the `--def` is 40 → 38 ms
(15 rounds, load 27). The cost is a macro-made method that overrides one the
class inherits: the inherited one still answers, as it did before.

**What the store does not have.** The macro's string is not stored — only
each `def`'s name shape and side (`core::Maker`) — so:
- the calls in the string are not read at the caller (`run_#{name}` in the
  testbed case has no caller), which would take the string's templated facts,
  its calls and `def`s with `{k}` in place of the names, stored per macro, and
  a `--refs` posting list that can name the macro's file for a call whose name
  only a caller's arguments spell;
- the site is the `class_eval` line, not the `def`'s: `define_callbacks`
  writes four methods from lines 911–923 and all answer 910. The `def`'s own
  line would take the marker to carry it — an extractor change, so a store
  bump;
- the arity is any.

**Measured** (against DEC-211). One gold verdict moves, polyid's gem site
`_run_checkout_callbacks` (`define_callbacks :checkout` in `AbstractAdapter`,
the macro in activesupport's `callbacks.rb`): residue → correct. None other in
any set; the clicks, both rails `--refs` sets and `--dead` on rails,
activerecord and mastodon unchanged. The names such macros can make, read off
each store (a class body's literal arguments against another file's
macro shape): on rails 174 cards, 152 residue → resolved, 21 still residue (`has_rich_text`,
which Action Text mixes in from an `on_load` hook and a `class_methods` block:
the caller's class side does not find the macro itself), 1 already resolved; on mastodon (with its
gems) 630, 195 residue → resolved, and the rest unchanged (341 already
resolved by the caller's own definition, 86 `no_such_method` and 8 residue
whose caller does not reach the macro). Most are `define_callbacks`'
`_run_*_callbacks` and `_*_callbacks` across every class that declares a
callback chain.

## DEC-213 — A bound call reaches a module a subclass of the bound mixes in

**Decided.** DEC-140's rule widens by one step. When `--refs` asks about a
method of a module M, and a site's receiver is a bound T — a `sig`, a
finder, the naming rung, or `self` — whose lookup lands elsewhere or
nowhere, the site is `possible` where a class below T mixes M in (directly
or through another module, `Tree::mixers_of`) and its own lookup of the name
lands on M's method ("the receiver is typed as an ancestor, and may be a
subclass that mixes this in"). The same holds for a delegate's bound target
(DEC-166). A class that is not below T, or whose lookup finds something
ahead of M, rules nothing back in. Instance methods only: a subclass that
`extend`s M is not counted.

**Why.** DEC-140's open item. `AbstractController::Base#process` calls
`process_action` on `self`; every controller runs `AbstractController::
Callbacks#process_action`, which `ActionController::Base` includes, and
`--refs` excluded that call as `different_owner` — so `--dead` called
`Callbacks#process_action` super-only. A method a subclass defines was
already reachable that way; one a subclass mixes in is the same dispatch.

**Measured** (against DEC-212). Every gold verdict, both rails `--refs` sets
and the 21,154 clicks unchanged. A sweep of the 2,843 instance methods rails
defines in a module some class with a superclass includes (by the module's
name) moves 72 sites in 35 queries, all excluded → possible, none from or to
confirmed. The largest: `ActionView::LogSubscriber::Utils#logger` (8, the
base `LogSubscriber`'s own `logger` calls), `ActiveModel::AttributeMethods#
respond_to?` from Active Support's `Object` extensions (`blank?`,
`acts_like?`; any model is an `Object`, as DEC-140 kept), the association
modules' `foreign_key_present?` and `build_record` reached from
`Association`, `Type::Helpers::Numeric#cast` and `Mutable#cast` from
`Type::Value`. Sampled, each is a template method a subclass's mixin
answers. graph_weaver's sweep of all 2,090 of its methods: none. `--dead`:
rails 2,252 → 2,244 candidates, activerecord alone 1,584 → 1,576 — gone are
`ThroughAssociation#foreign_key_present?`, `#target_scope`, `#stale_state`,
`#build_record`, `ForeignAssociation#foreign_key_present?` and each
adapter's `DatabaseStatements#write_query?` — and `Callbacks#process_action`,
`Rendering#process_action` (super-only), `ImplicitRender#method_for_action`,
`BasicImplicitRender#send_action` (override) and `ControllerRuntime#
process_action` (convention-only) become single-caller; mastodon's
`QueryHelper#perform_data_query` moves override → single-caller.

## DEC-214 — A call in an `on_load` block runs on the class that runs the hook

**Decided.** A call with no receiver, or on `self`, written directly in an
`ActiveSupport.on_load(:name) do … end` block is typed as the class that runs
the hook, on its class side (`resolved_via` `on_load`); one in a `def`
written directly in such a block, as that class's instances — a bound, since
the method is inherited (DEC-104 puts the `def` on the class). The classes
are read from the `load_hooks` edges (DEC-098) on first need
(`Tree::hooked`); where two run the hook (`:action_controller` is
`ActionController::Base` and `::API`), the first is the reading and the
other its rival, so the answer is ambiguous and names it. A block
registered `yield: true` hands the class in as an argument, and its `self`
stays the caller's. Completion of a bare word follows: the hooked class's
methods, on the block's side, where it offered `Object`'s or the lexical
class's. Each block is a resolve-time fact (`Facts::hook_blocks`), never
stored.

**Why.** DEC-127's open item. `establish_connection` in activerecord's
railtie, inside `on_load(:active_record)`, was typed as the `Railtie` the
initializer is written in and `--refs ActiveRecord::ConnectionHandling#
establish_connection` excluded it as `no_such_method`; a call in a gem's
hook block at the top of a file was residue; and VS Code completed neither.

**Not every block of the hook.** DEC-098 records a hook's mixins only when
the block is registered as its file loads, because a mixin's place in the
chain depends on when it runs. What `self` is in the block does not: an
`on_load` in an `initializer` still runs its block on the hooked class, so
its calls are typed too.

**The cost.** Completion in such a block now lists the hooked class's
methods — `ActiveRecord::Base`'s class side on an empty prefix. Measured on
rails, LSP completion at `establish_connection` in the railtie, three
servers each: the first request (which builds the tree) 335–342 ms before and
335–336 ms after; the next eleven, median 3 → 4 ms on an empty prefix, 2 →
2 ms on `estab`, which now offers `establish_connection` where it offered
nothing. A model's own body is unchanged (3 ms): nothing is added to any
chain, and the hook classes are read only for a call in a hook's block.

**Measured** (against DEC-213 on main 99a11f7). No gold verdict moves into
wrong or residue. widget_shop's gem sites gain 8 correct and 3 declarations,
from residue offering them: actionpack's and actionmailer's railtie hooks
(`protect_from_forgery`, `register_interceptors`, `register_observers`,
`smtp_settings`), railties' `engine.rb` (`prepend_view_path`, twice) and
propshaft's `before_action`; `wrap_parameters` and one `prepend_view_path`
answer ambiguous-correct, their hook run by both `ActionController::Base`
and `::API`. Confidently wrong unchanged in every set. The rails `--refs`
sets move one site, excluded → confirmed: the railtie's
`establish_connection`. `--dead` and the 21,154 clicks unchanged.

## DEC-230 — A long answer is written row by row

**Decided.** `--refs`'s JSON answer and every row listing (`emit_rows`: the
bare-name listing, `--symbols`, the usage and miss logs) write their rows
one at a time through a buffered stdout, each converted to a `Value` and
rooted (`rooted`) on its own. `emit_listing` writes an answer's object in its
keys' sorted order — the order a `serde_json::Map` has — with one key's
value streamed from the rows. The bytes are the ones `emit_json` printed.

**Why.** `emit_json` serialized the answer to a `Value`, which for `--refs`
already held every reference as a `Value` (the `json!` that built it), then
copied it (`to_value` of a `Value`), rooted the copy, rendered it to one
`String`, and printed that. Four forms of the same answer at once, the
last two each the size of the output: `--refs 'Hash#[]' --json` over the
100k-file corpus (DEC-195) prints 175 MB.

**Measured.** On da7cc69 against this lane's head, each build on its own
store, interleaved, medians (p90), load 4–8 from other work; the 100k
queries are `--include-excluded`:

| | main | this | rounds |
| --- | ---: | ---: | --- |
| 100k `Hash#[] --json` | 13.9 (19.2) s, 1,655 MB | 14.0 (24.8) s, **618 MB** | 5 |
| the same, `--ndjson` | 12.3 (15.1) s, 1,616 MB | 12.9 (13.0) s, **614 MB** | 3 |
| the same, text | 12.3 (12.6) s, 989 MB | 12.4 (13.0) s, **644 MB** | 3 |
| 100k bare `--refs new --json` | 7.2 (7.7) s, 1,315 MB | 6.9 (7.1) s, **617 MB** | 5 |
| rails `Persistence#save --json` | 126 (129) ms, 51 MB | 123 (124) ms, 51 MB | 11 |

Megabytes are peak footprint (peak RSS 2,228 → 1,186 MB for the first
row). CPU time is unchanged (38.6 → 39.6 s, 37.8 → 37.2 s); the walls are
the same within the noise. Text output falls too: `cmd_refs` built the
answer's `Value`, every reference in it, before it looked at the output
mode. What remains is the tiering's own — the tree, its memos, and the
441k references held to be sorted.

**Checked.** Byte-identical to main on the memo lane's verify set (the gold
sets, the rails `--refs` 40- and 51-query sets, `--dead` on three corpora,
the probe, the clicks) and on the 100k `Hash#[]` in `--json`, `--ndjson` and
text. A unit test renders a listing and a row set both ways, empty
included, and requires the same bytes.

## DEC-231 — A method lookup walks a name's owners without building a key per step

**Decided.** The tree's method table is keyed by name, then owner, then side
(`by_owner: name → owner → [instance, singleton]`), where it was keyed by the
triple `(owner, singleton, name)`. `first_in_chain` — the walk every lookup
makes down a chain — takes the name's owners once and probes each chain
element by its `&str`. Its early stop at the next class after a generated
method now asks whether one is held before it asks the snapshot whether the
element is a class.

**Why, measured.** DEC-200 named it: sampled at 1 ms, `first_in_chain` was
54 % of a `--def` inside a module on the 100k corpus — per chain element two
`String`s allocated for the key, hashed, and freed (~40 % of the function),
and a snapshot lookup of the element's kind (~40 %) that answered a question
only asked once a generated method was held, which it almost never is.

`respond_to?` in `ActiveSupport::Tryable` (DEC-200's query), each build on
its own store, interleaved, medians (p90), on two mains:

| | 99a11f7 → this, load 7–8 | da7cc69 → this, load 4–5 | rounds |
| --- | ---: | ---: | --- |
| rails | 65 (66) → **49** (50) ms | 119 (122) → **102** (104) ms | 11 |
| mastodon | 81 (82) → **63** (65) ms | 132 (133) → **116** (118) ms | 11 |
| 100k files | 467 (468) → **296** (297) ms | 1,031 (1,042) → **861** (864) ms | 7 |

Peak footprint unchanged (42, 39 and 300 MB on da7cc69).

**The next lever.** Between the two mains the query doubled: DEC-212's
`made_along` places every dynamic marker (`place_dynamic`) on a lookup's
first miss, to see whether a string macro made the name. Sampled on
da7cc69 with this change, placing is 75 % of the 100k `--def`. Placing only
the markers `made` needs, or keeping `made` beside the snapshot, would take
most of it back; `made_along` also still builds an `(owner, singleton,
name)` key per chain element, the pattern this entry removes. *DEC-235
does both.*

**Checked.** The table holds the same indices in the same order for every
(owner, side, name); a lookup on the side a name was never defined on finds
an empty list where it found no entry, and both answer nothing. Byte-identical
to main on the verify set (DEC-230's list) and the 100k `--def` and
`Hash#[]` answers.

## DEC-232 — The bundle's gems are indexed as one stream

**Decided.** `index_gems` settles which gems are new, then `index_bundle`
takes them together: every gem's `lib/` walked on the pool at once, every
blob none of them has seen parsed on the pool in gem order, a chunk of 128
at a time, and each gem written in turn, in the bundle's transaction, as its
files arrive. A blob two gems share is parsed for the first and known by the
second's write, as before. The app's own index (`index_files`) is unchanged
and shares the parse and the profile's accounting with it
(`parse_file`, `Received`).

**Why.** DEC-195's unmeasured lever: one gem at a time, the walk (reading
and hashing each file of `lib/`) ran on one thread, and each gem's parse —
~36 files on discourse — could not start until the gem before it was
written, so the pool idled between gems.

**Measured.** A cold `--index` into a fresh store (its snapshot included),
each build its own, interleaved, medians (p90), load 4–6 from other work:

| | main | walk in parallel only | one stream | rounds |
| --- | ---: | ---: | ---: | --- |
| rails + 73 gems | 1.47 (1.87) s | 1.45 (1.52) s | **1.38** (1.54) s | 5 |
| mastodon + 301 gems | 3.31 (5.62) s | 3.21 (3.61) s | **3.12** (3.78) s | 5 |
| discourse + its gems | 4.75 (5.87) s | 4.76 (5.05) s | **4.25** (4.44) s | 5 |
| 100k files + mastodon's bundle | 20.6 (29.5) s | 18.7 (19.4) s | **18.1** (25.6) s | 3 |

On the final build, at load 4–5: rails 1.22 (1.31) → **1.17** (1.19) s in
11 rounds, mastodon 3.42 (3.62) → **3.14** (3.36) s and discourse 4.26 (5.22)
→ **3.83** (4.02) s in 7; and before DEC-234, which leaves a first index
alone, the 100k checkout 20.7 → 19.5 s in 3, and an app with no bundle (the
100k corpus) 18.9 → 18.7 s, unchanged.
`gem-walk` falls 1,522 → 266 ms on the last, 347 → 127 on discourse. CPU is
0.1–0.6 s more, and peak footprint 5–25 MB more on the three apps across two
campaigns (47 MB less at 100k files). Walking in parallel
and then indexing one gem at a time — the contained version, twelve lines —
takes most of the gain at 100k files and none on discourse, whose gems are
many and small; the stream is what keeps the pool fed there.

**Chunks of 128, not 512.** The first cut held 512 files' facts in the
channel and 512 more being parsed: 17–38 MB over main on the three apps,
for walls no better than 128's within the noise.

**Checked.** Each index's own answer (files, blobs, parsed, gems) is
identical, and so is every answer on the verify set (DEC-230), whose stores
all index a bundle. An e2e case puts a gem with no `lib/` and one whose only
file another gem already parsed between two gems, and requires three gems
indexed and the shared blob parsed once.

## DEC-233 — Tiering on every worker, measured and not taken

**Not done.** DEC-205's next lever: tier `--refs` on every worker, each
against a tree of its own over the shared snapshot, since every answer is a
function of the tree alone (DEC-200). Built and measured on DEC-230–232
over main 99a11f7 (branch `perf2-par`, commit `1af6d57`): `Tree::seed`
copies what a tree has loaded — the namespace shared by an `Arc` over the
mapped bytes, the methods loaded so far, the carriers — and each worker
grows a tree from a seed on its own thread, reopens the store, takes chunks
of 16 files in turn, parses and tiers them, and the chunks go back in
order. The workers read each name's rows from the store once between them
(a cache in the loader) and place the dynamic markers once (a
`OnceLock`). Every answer stayed byte-identical (the verify set, the 100k
`Hash#[]`), and a unit test held workers to one thread's answer on a
fixture. It does not pay.

**Measured**, each build on its own store, interleaved, load 5–8 from other
work:

| | DEC-230–232 | on every worker | |
| --- | ---: | ---: | --- |
| 100k `--refs 'Hash#[]' --json` | 14.4 (p90 21.9) s, 613 MB | 11.9 (14.7) s, 2,901 MB | 3 rounds |
| the same, again | 12.9 (13.8) s, 610 MB | 9.0 (9.2) s, 2,909 MB | 3 rounds |
| rails, the 51-query `--refs` set | 7.24 s | 6.06 s | 5 rounds |
| rails `QueryMethods#where` | 186 (251) ms, 81 MB | 163 (196) ms, 100 MB | 11 rounds |
| rails `Persistence#save` | 74 (86) ms | 82 (89) ms | 11 rounds |

Megabytes are peak footprint. At 100k: 1.1–1.4× for 4.7× the memory, and
CPU 38 → 48 s; on rails a sixth off a set of queries, and slower on the
small one.

**Why so little.** Much of one tree's tiering is its warm-up, and every
worker repeats it. On the 100k `Hash#[]`, one tree tiers its first 800 of
44,867 files in 13 % of its time; eight workers each spent 2.5–7.5 s on
their first 800. Sampled, the first five seconds of one tree's tiering are
loading each name's methods from the store (39 %), placing the dynamic
markers (20 %), `agreed_return`'s vote (13 %) and linearizing (12 %) — work
each of eight trees does again for its eighth of the files. Sharing the
rows and the placement took CPU only 52 → 50 s (at load 20). Having the
query's own tree tier the first 1 or 8 chunks before the others grew from
it measured no better (100k 9.0 and 9.4 s; rails set 6.7 and 9.1 s). Eight
SQLite connections also contend on SQLite's global memory-statistics mutex:
`sys` 12 → 5 s with `SQLITE_CONFIG_MEMSTATUS` off, and the wall barely
moved.

**What it would cost to keep.** A seed has to carry every field a tree sets
at build time and none of its memos. The resolve lane added three fields to
the tree in the same week (DEC-212's `made`, `placing`, `hooks`), and one a
seed forgets makes the workers answer differently from each other — silently,
unless a fixture happens to exercise that field. That is a tax on the most
edited module, for a query shape (hundreds of thousands of sites) an agent
rarely asks.

**What would clear the bar.** One tree shared by the workers — memos behind
locks or sharded maps, `Rc` → `Arc`, a linearization stack per thread — so
the warm-up is paid once. Or tiering that asks the tree less per site. Not
attempted: the first is a rewrite of the tree's interior, and the budget
this would buy is the pathological query's; the 51-query rails set is
7.2 s for 51 processes, most of each its start and its tree.

## DEC-234 — A load of half the store rebuilds its indexes (DEC-057 revisited)

**Decided.** A write that brings in at least half as many new blobs as the
store already holds drops the fact indexes and rebuilds them by sorting
(`bulk_load`), where DEC-057 did that only for a load larger than the store.

**Why.** DEC-195 left it inconclusive: lowering the line to a quarter
measured 83 → 68 s with a p90 of 115 s, the machine shared. Re-measured
here at the quietest this machine got (load 3–5): a store of 50k files
(DEC-195's c50k), then a checkout of new files indexed into a copy of it,
alternating builds, four rounds each:

| new files into 50k | inserting (DEC-057) | rebuilding |
| --- | ---: | ---: |
| 12.5k (a quarter) | **4.6** s (4.5–4.9) | 6.0 s (5.5–7.9) |
| 25k (a half) | 8.0 s (7.8–12.9) | **7.8** s (7.3–8.3) |
| 50k (all of c100k's other half) | 20.3 s (19.5–20.9) | **14.5** s (13.9–15.3) |

The rebuild costs about what the store holds, the inserts what the load
holds, into indexes that outgrew the cache; they cross near a half. At a
quarter the rebuild loses by a quarter, which is why the line is not there.
At 1:1 the profile says where: the rows and their inserts 11.5 s against
4.5 s of rows and 4.7 s of rebuild, and what follows reads a compact index —
the tree snapshot 3.3 → 1.4 s, `ANALYZE` 2.0 → 0.8 s — in a store 3.5 %
smaller (630 → 608 MB).

**Checked.** The same rows either way: the index's answer is identical at
1:1, and `--refs 'Hash#[]'` and the module `--def` over the 100k checkout
are byte-identical from a store loaded each way. The verify set (DEC-230)
is unchanged. A unit test pins the line.

## DEC-235 — A lookup's miss works out only the string macros that could make its name

**Decided.** `made_along` (DEC-212) asks `made_for(name)`, memoized per
name: of the markers — read once per tree, owners resolved (`markers`) — only
the string macros whose shape, each `{k}` taken as any name, could spell
`name` (`may_expand_to`) have their callers found, and the macro's `def` is
expanded at each as before. The result is keyed by class, then side, and a
chain is probed by each element's `&str` (DEC-231's pattern). A miss no
longer places every marker; `place_dynamic` still does, for the hedges that
list them (`dynamic_in_chain`, `dynamic_in_file`), and skips the methods a
string macro makes by name, which are `made_for`'s. A macro's callers are
memoized per (owner, macro), so the two share them.

**Why, measured.** DEC-231's next lever. On the first miss of any lookup,
`made_along` placed every marker in the tree to learn whether a string
macro made the name, and placing finds each macro's callers by a class-side
lookup of the macro's name at every class whose body calls it — loading
that name's methods from the store. Sampled on the 100k `--def` in a module,
placing was 73 % of the query: those lookups 53 % (their method loads 34 %),
reading the markers 13 %, the body calls 6 %. `respond_to?` is a name no
string macro can spell, so none of it was needed.

`respond_to?` in `ActiveSupport::Tryable`, each build on its own store,
interleaved, medians (p90), load 3–6 from other work:

| | main (3ce6096) | this | rounds |
| --- | ---: | ---: | --- |
| rails | 102 (108) ms, 42 MB | **53** (55) ms, 31 MB | 11 |
| mastodon | 116 (117) ms, 40 MB | **72** (73) ms, 35 MB | 11 |
| 100k files | 868 (876) ms, 300 MB | **327** (333) ms, 242 MB | 7 |
| rails, the 51-query `--refs` set | 7.86 s | **7.12** s | 5 |
| 100k `--refs 'Hash#[]' --json` | 12.3 (12.7) s | 12.9 (14.3) s | 3 |

Megabytes are peak footprint. The module `--def` is now below where it was
before DEC-212 (467 ms at 100k, 65 ms on rails); `Hash#[]` is within the
noise (CPU 37.7 → 38.1 s).

**Why the prefilter is sound.** `expanded` makes a name only for a string
maker (`class_eval`, `module_eval`, `instance_eval`, `eval`) with a side and
a shape, from the shape with each `{k}` replaced by a name the call hands
it; the shape with each `{k}` as `*` matches every such name, so no macro
that could make `name` is skipped. `Maker::may_make` is not that test: it
hands the shape one unnamed argument, and a `{k*}` with k ≥ 1 then makes no
name at all, so it matches nothing — for the hedges it serves too.
Callers are found in the markers' order and a later expansion replaces an
earlier one, as in placement.

**Checked.** Byte-identical to main on the verify set (DEC-230's list), on
DEC-212's macro cards (174 rails and 630 mastodon queries, status, reason,
site and counts), on the linearization dump (rails, mastodon, 100k) and on
the 100k `--def` and `Hash#[]` answers. Testbed case 135 holds the
answers; a unit test requires a miss on a name no macro makes to place no
marker and find no macro's callers, fails when `made_along` places, and
requires a made method to land as before.

**One difference in kind, not seen in any answer.** Lookups made while
finding callers are memoized with `made_along` off (`placing`), as before;
since fewer callers are found at the first miss, a class-side lookup of a
macro's own name may now be asked first outside that window. It answers
differently only if a string macro in another file makes the macro itself.

## DEC-241 — `class << Time` inside `class Time` opens Time

**Decided.** A method written in `class << Name`, and a call written there,
belong to the class `Name` looks up to when the tree placed the scope
somewhere nothing declares: a constant in the body (`ZoneOffset = {…}`)
made the tree imply a `Time::Time` module with no site, and `Time.parse`,
`Time.httpdate` and every other method of Ruby's own `time.rb` landed on it.
`Tree::opened` looks such a scope up lexically instead, skipping another
site-less one; `owner_of` asks it only for a singleton method, since only
those can be in `class << X` and a module's sites are not free to ask of
every row, and `scope_fqn` for every call, so a call in the body is typed
by the class its methods land on.

**Why now.** Core's stub wrote `Time.parse` by hand, and hid it: the call
found the stub's and never looked for `time.rb`'s. Read from RBS (DEC-240),
core has no `Time.parse` — it is the stdlib's — and the call found nothing.

**Measured** against main (3ce6096), each build on its own store. Gold sets:
every verdict the same but one of flipper's stdlib sites, residue → correct
(`net/http.rb`'s `proxy_class?`, in `class << HTTP` inside `Net::HTTP` —
the same shape, a module deep). Clicks: one fewer unsure of 21,154. Rails
`--refs`: `Time.parse` excludes three `parser.parse`-style calls that pass
more arguments than `time.rb`'s `parse(date, now)` takes (possible 109 →
106); `Time#iso8601` and `Time#xmlschema` exclude their five and one sites
on another owner rather than as `no_such_method`. `--dead` unchanged.

## DEC-240 — Core and the stdlib's signatures are the app's Ruby's own, read at index time

Supersedes DEC-078's "`core.rb` stays the one source" and DEC-220's
"checked in, one version for every Ruby".

**Decided.** Core and the stdlib's stubs are no longer checked in and built
into the binary. When an index reads a Ruby's stdlib (DEC-180) it also
finds the `rbs` gem that Ruby carries — the one bundled with it, else the
highest installed for it, else another Ruby's (DEC-242) — and writes from
its signatures the three stubs the
generators wrote: **core**, the stdlib's **compiled half**, and the
**returns lent** to its Ruby half. They are stored beside the stdlib's
checkout (`rbs`, `rbs_use`), keyed by stdlib, gem and reader, so every app
on one Ruby shares one row and a reinstall or a newer rbs is read again.
The tree serves them as it served the checked-in ones — one file per
top-level owner, under a directory per Ruby's signatures
(`<core>/rbs-3.8.0-<key>/String.rb`), each `def` cut out and extracted when
its name is first asked (`corelib::cut`), core's included, and a namespace
assembled from core's files with their `def`s blanked. `--index` and
`--status` name the gem and why it was chosen, or say the Ruby carries
none.

- *The reader* (`src/rbs/parse.rs`) is hand-written: declarations, ancestry,
  constants, class aliases, `def`s with their overloads, `self.`/`self?.`,
  `attr_*`, `alias`, visibility; types only as far as telling one class
  from anything else. A member it cannot read is skipped, not the file.
  Names resolve through the scopes they are written in; generics are
  erased; RBS's unnamed modules and classes (`Random::Formatter`'s methods,
  `Random < RBS::Unnamed::Random_Base`) are their includer's; an alias
  takes the method it names, through a module's self type
  (`Kernel#object_id` is `BasicObject#__id__`).
- *The rules are DEC-077's and DEC-220's, unchanged*: a `sig` per call shape
  only where every covering overload agrees; none for a union, an optional,
  `bool`, `self` or an element type; returns written from the top; never a
  module, `Class`, or a class RBS subclasses (Enumerator excepted). The
  generators' parameter logic — call-seq names, RBS arity — is ported
  whole.
- *Compiled-ness is inferred, not asked.* A stdlib method RBS describes that
  the indexed Ruby defines lends its return. One it does not define is
  compiled — declared by the stub — when its library has a file a compiled
  extension backs (DEC-181), unless a file the index leaves out writes it
  with `def` (`json/add/`'s `Time#to_json`) or a Ruby maker on its class
  spells its shape (`Ripper::SexpBuilder`'s `on_*`). A class is declared
  only where no Ruby file declares it and its library compiles. Owners come
  from a tree of the stdlib alone (`Tree::alone`).
- *No rbs, no core.* A checkout for which no Ruby is found, or on a Ruby with
  no rbs gem anywhere, is served no stubs: `puts` and `"x".upcase` are
  residue, as anything unindexed is. There is no checked-in fallback.
- *No version gate.* Whatever the chosen rbs describes is used; a stub
  method on an owner the tree does not know is dropped, as before.

**Why.** The checked-in stubs were one Ruby's (3.4, rbs 3.8.0) for every
app, 187 KB in the binary plus two generators that needed that Ruby to run,
and a `core.rb` whose method list was curated by hand — with its drift:
`String#join`, `Math.pow`, `File.exists?` and `Enumerable#with_index` do not
exist, `Time#iso8601` was missing (DEC-220). Every Ruby 3.x installs rbs.

**Why hand-written, not `ruby-rbs`.** The bindings (0.3.0) wrap one rbs
version's C parser and refuse a file for one member of another version's
syntax; the files here come from whatever rbs the app's Ruby has. They also
add a C build and bindgen to CI. The subset trekr reads is small: this
reader parses every file of rbs 3.5.1, 3.8.0 and 4.2.0 — `core/`,
`stdlib/` and rbs's own `sig/` — with none skipped.

**Equivalence**, with the reader pointed at the rbs the checked-in stubs came
from (3.8.0, Ruby 3.4.9), stub against stub:

- *Core*: 792 methods → 2,236 (2,307 from rbs 4.2.0), 303 classes from 106;
  615 of the new methods carry a `sig`. Of core.rb's 792, 714 are the same
  owner's, 50 are now found on the ancestor RBS writes them on with the same
  return (`Array#freeze` on Kernel, `File.read` on IO, `Hash#group_by` on
  Enumerable, `Mutex#synchronize` on `Thread::Mutex`), 5 differ
  (`Time.new`, `Thread.new`, `Fiber.new` lose a hand `sig` that "Foo.new is
  a Foo" already gives; `Thread::Queue#size` and `#length` gain Integer),
  and 23 are gone: the stdlib's, answered by its Ruby when indexed
  (`FileUtils.*`, `Time.parse`, `Dir.tmpdir`), and ones RBS has not
  (`String#join`, `Math.pow`, `File.exists?`, `Enumerable#each` and
  `#with_index`, `Kernel#instance_variable_names`, `Numeric#to_i`/`to_f`/
  `to_r`/`*`/`**`/`/`, `Object#=~`). Of the 714, six returns differ:
  `Kernel#__dir__` (RBS says `String?`; core.rb's `String` was edited by
  hand), `Thread.current`, `Thread.main` and `Thread#join` (RBS's
  `Process::Waiter < Thread` makes Thread subclassed), and `Enumerable#lazy`
  → `Enumerator::Lazy` and `File.stat` → `File::Stat`, which the old
  generator could not return because it read only top-level class names.
  27 `initialize`s and the module function `Kernel#__dir__` are private, as
  in Ruby. 82 parameter lists differ, in names (`Hash#[]=(arg1, arg2)` for
  `key, value`) or in RBS's exact arity (`Array#[](start, length = nil)` for
  `*args`); `new` is always `*args`, and RBS's `(?)` is `*args`. The class
  tree is the same but for `Mutex`, `Queue`, `SizedQueue` and
  `ConditionVariable`, now `Thread::`'s with top-level aliases, as Ruby has.
- *The compiled half*: 1,140 declarations → 1,609; the 1,123 both write have
  the same returns. 17 are no longer declared: 10 a C extension replaces
  at runtime over a Ruby `def` (`CGI::Util#escape_html`,
  `ERB::Util.html_escape`, `IPSocket.getaddress`), which now lend their
  return to that `def`, and 7 Ruby aliases (`Monitor#mon_enter`). 486 are
  new: Ripper's 455 `on_*`, which Ruby makes with `alias_method` over an
  interpolated name no marker spells, and `CGI::QueryExtension`'s 28, from a
  `define_method` over a list; per-subclass copies of what Ruby defines on
  the superclass (`OpenSSL::ASN1::*#value`, `PKey::RSA#to_text`); json's
  C `to_json` on `Object`, `Integer` and friends, which Ruby mixes in; and
  what this Ruby lacks though RBS describes it — `Psych::DBM`, `Psych::Store`
  and their 26 methods, `OpenSSL::Engine`'s 15, `OpenSSL::Config#[]=`,
  `Pathname#taint`, Digest's `bubblebabble` (until required). Of classes
  only the stub declares, none of 111 is lost and 10 are gained that this
  Ruby lacks: `JSON::Pure` and its three (json 2.9.1 dropped them),
  `OpenSSL::Engine` and its error, `OpenSSL::ExtConfig`, `Psych::DBM`,
  `Psych::Store`; `JSON::State` and `JSON::UnparserError` are constants here.
- *The lent returns*: 792 → 552. 535 are identical and 251 now come from the
  core stub instead, borrowed as DEC-182 borrows (`Random::Formatter#hex`,
  `Set`'s); 2 differ (`Benchmark.bm`'s block parameter's name, and
  `Thread#run` for the Thread reason above); 4 are gone — `Array.new`,
  `String.new`, `Regexp.compile`, which "Foo.new is a Foo" covers, and
  `OptionParser::Arguable#options`, since `optparse/ac.rb` subclasses
  OptionParser and the runtime probe never loaded it — and 16 are new: the
  10 above, `Net::HTTP.newobj`, `Psych::Store`'s four (via `yaml/store.rb`),
  and `SecureRandom.alphanumeric`.

**Measured** against main (3ce6096), each build on its own store, served
the rbs bundled with Ruby 3.4.9 (3.8.0, DEC-242). rbs 4.2.0, installed later,
measured the same but where noted (`String#downcase` 38, `Array#join` 219).

| | main | this |
| --- | ---: | ---: |
| confidently wrong, every gold set | same | same |
| widget_shop gem code correct | 1,618 | 1,619 |
| clicks: definition empty / unsure | 1,863 / 3,193 | 1,835 / 3,190 |
| clicks: hover unsure | 3,589 | 3,558 |
| `--refs Array#join` confirmed / possible | 312 / 372 | 220 / 464 |
| `--refs String#downcase` confirmed | 26 | 31 |
| `--refs Array#first` confirmed | 153 | 172 |
| `--refs Logger#info` confirmed | 6 | 12 |
| `--refs Time#iso8601` confirmed | 4 | 8 |

Every other gold verdict is the same but flipper's `proxy_class?` (DEC-241)
and widget_shop's: `underscore` residue → correct, and four residues whose
truth falls out of the eight candidates shown (`quote`, `infinite?`:
`Regexp.quote` and `Float#infinite?` are among them now). Clicks "defined
nowhere indexed" 386 → 358, "typed, with competitors" 247 → 224. Of the
rails `--refs` set's 68 queries, 24 move; the rest of the exclusions are
receivers now typed (`excluded_no_such_method` → another owner, 23 of
`Pathname#to_s`'s 29) or RBS's exact arity (`Time.parse`,
`Digest::Class.hexdigest`: three `Digest::MD5.file(p).hexdigest` sites
excluded, rightly). `--dead` on rails: `AbstractController::Base#
method_added` is an override of `Module#method_added`, and
`Mapper::Resources#resource_method_scope?` a single caller once
`@scope.resource_method_scope?` is `Scope`'s. `--dead` on activerecord alone
(no Gemfile, no `.ruby-version`) is unchanged, run on the Ruby it finds
(DEC-242).

**Fixed on the way**, each a resolver assumption a fuller core broke, each
with a testbed case that fails without it: `class << Time` inside
`class Time` (DEC-241, case 240); a receiver-name guess disqualified by a
*private* method of the enclosing scope, which an explicit receiver cannot
call — core's `Kernel#autoload?` against zeitwerk's `cref.autoload?`,
four gold sites correct → residue until fixed (case 242); and a local
assigned through a second name for a class — `lock = Mutex.new` typed as
the constant `Mutex`, 50 `synchronize` sites `no_such_method` until
fixed (case 243).

**Timings**, Ruby 3.4.9 and its bundled rbs 3.8.0, load 8–13 from other
work, interleaved. The first index on a Ruby reads its signatures in
~130 ms (one-file app: 296–354 → 437–467 ms); a second app on that Ruby,
173–184 ms, as before. The store grows by the stubs, ~260 KB per Ruby.
Queries are no slower and often faster, since core is cut and extracted by
name rather than parsed whole on every tree build (DEC-220's cut, now
core's too), 21 interleaved rounds, medians: an app method 10.9 → 11.0 ms,
`"x".upcase` 12.6 → 11.0, a stdlib stub's `read` 13.5 → 12.6; rails
`--ancestors ActiveRecord::Base` 15.4 → 11.2, a `--def` into core 20.7 →
18.6, `--refs String#downcase` 115 → 112 (those three with rbs 4.2.0).

**The named costs.**

- *More definitions, more competitors.* `Enumerator::Lazy#map` returns a
  Lazy, and is now returnable (nested classes were not), so `x.map { }`
  on an untyped `x` has no agreed return: `Array#join` loses 92 confirmed
  sites to possible, none wrong. Residue lists fill further.
- *No Ruby, no core.* Where no Ruby is found at all, nothing knows
  `Object`: an `--dead` override of `Module#extended` reads as
  unreferenced. DEC-242 finds a Ruby wherever there is one.
- *RBS is not the Ruby.* What it describes that this Ruby lacks is declared
  (above), and what Ruby makes at runtime where no maker spells it is taken
  for compiled.

**Turned down.** Asking a Ruby at index time, as the generator did: the
engine runs no Ruby (PLAN §4). Keeping core.rb as a fallback for a Ruby
without rbs: a checked-in core is what this removes, and every Ruby 3.x has
rbs. Reading signatures at tree time: every query would pay the parse.

Tests are hermetic: `tests/fixtures/rbs/` is rbs 3.8.0's core trimmed to what
core.rb stubbed (with each method's call-seq), and every testbed case,
`cli_e2e` and `lsp_e2e` checkout runs on a fake Ruby 9.8.7 carrying it; a case's
`rbs/` adds its libraries' signatures.

## DEC-242 — A checkout that names no Ruby runs on the one it finds, and a Ruby's rbs is the one bundled with it

Amends DEC-180's gate and DEC-240's choice of rbs.

**Decided.** Two changes to which signatures a checkout's core comes from.

- *Every checkout gets a Ruby.* DEC-180 gave a stdlib only to a checkout
  that resolves gems or names a Ruby, which kept a test's scratch
  repository hermetic. Once core is the Ruby's (DEC-240), that gate took core
  from every directory of scripts. A checkout now runs on the Ruby DEC-152's
  chain finds — the version it names, `$GEM_HOME`'s, the `ruby` on `$PATH` —
  and, failing those, the only Ruby installed, when there is one; with none,
  or several and nothing to choose between them, no core, as before. Tests
  stay hermetic by what they put on `PATH` (only `git`) and in `HOME` (the
  fixture's Ruby, staged).
- *A Ruby's rbs is its bundled one.* DEC-240 took the highest rbs installed
  for the Ruby: this machine's Ruby 3.4.9 was served rbs 4.2.0's core, which
  describes Ruby 4.0 (Pathname is core there). A bundled gem's version is
  that Ruby's, so it comes first; then the highest installed for that Ruby;
  then the highest another installed Ruby has. rubygems keeps no mark of which
  installed rbs is the bundled one, and a later `gem install rbs` lands in
  the same directory, so the bundled one is the rbs in the Ruby's own gem
  directory whose gemspec was written within an hour of its default gems'
  (3.8.0's 5 s after, 4.2.0's five months). `--index` and `--status` say
  which (`gems.stdlib.rbs.chosen`: `bundled`, `installed`, `other`) and why,
  in words.

**Measured** against the DEC-240 build (rbs 3.8.0 pinned), each on its own
store: gold sets, clicks and the 68 rails `--refs` queries identical;
`--dead` on activerecord alone — a directory with no Gemfile — now runs on
the Ruby `$GEM_HOME` names and its 13 tier moves go back to main's
(`ActiveRecord::Enum#extended` overrides `Module#extended` again). The stubs
the bundled rbs yields are byte-identical to the pinned run's, so DEC-240's
equivalence stands as written.

**Not done.** Several Rubies installed, none named, none on `$PATH` and no
`$GEM_HOME`: no Ruby, rather than a guess. A Homebrew Ruby's bundled rbs is
found by the same rule, in its Cellar prefix.

## DEC-250 — One tree, shared by every worker: `--refs` tiers on all of them (DEC-233 revisited)

**Decided.** The tree is `Send + Sync` and every pool worker tiers against
it: `gather_refs` hands each file to the pool, which parses and tiers it
there, and the files come back in the order they were listed. DEC-233 grew a
tree per worker from a seed and paid every worker's warm-up; here the
warm-up — a name's rows, the markers' placement, `agreed_return`'s vote, the
chains — is paid once, by whichever worker asks first.

**Why it is sound.** Apart from its build, a tree only fills memos and
loads names, and each is a function of the tree and its key (DEC-200): it
does not matter which thread fills one first. Each field is one of three
kinds, and a new one says which:

- **set at build**, read-only after — a plain field;
- **a memo** — `tree::memo::Memo`, a sharded map filled by whoever misses
  first, computed with no lock held, the first value stored kept; or a
  `OnceLock` for one whole-tree value (the markers, their placement, the
  includers, the mixers, the hooks); or `Once` for a value costly to repeat
  that asks for no other of its kind (a name's rows, below): the threads
  that ask meanwhile wait;
- **per call in flight** — the linearization stack and `placing` — a
  thread-local tagged with the tree, never a field.

A memo that recurses (a lookup linearizes; a linearization resolves a path
through ancestors, which linearizes) must not hold a lock or a one-time
initializer while it computes: that is the lazy-init deadlock, across two
threads or on one. `Memo` hands out clones, never a guard. What a `RefCell`
or `Rc` field would break fails to compile (`Tree: Send + Sync` is
asserted), which is what a seed that forgot a field did silently.

The loader keeps a connection per concurrent load, reopened on demand; a
connection cannot cross threads, and one behind a lock serializes every
load. SQLite's memory statistics are off (`SQLITE_CONFIG_MEMSTATUS`, before
the first connection): they take one process-wide mutex per allocation.

**Measured**, on 4e88526, each build on its own store, interleaved, medians
(p90), load 3–7 from other work; `par` is DEC-233's branch as built
(1af6d57, on 99a11f7):

| | main | `par` | this | rounds |
| --- | ---: | ---: | ---: | --- |
| 100k `--refs 'Hash#[]' --json` | 14.3 (14.5) s, 613 MB | 8.1 (8.1) s, 2,912 MB | **7.4** (7.7) s, **729 MB** | 3 |
| its CPU | 37.7 s | 46.4 s | 39.9 s | |
| rails, the 51-query `--refs` set | 7.24 s | 5.91 s | **4.99** s | 5 |
| rails `Persistence#save` | 74 (75) ms, 39 MB | 72 (82) ms, 27 MB | **49** (50) ms, 25 MB | 11 |
| rails `QueryMethods#where` | 173 (178) ms, 83 MB | 143 (146) ms, 100 MB | **107** (111) ms, 57 MB | 11 |

Megabytes are peak footprint. The same `Hash#[]` at load 6.5–7: 16.5, 10.9
and 9.6 s. Threads (`RAYON_NUM_THREADS`, one run each, load 3): 1 → 26.1 s,
2 → 14.2, 4 → 8.3, 8 → 6.3; CPU 26.1 → 39.0 s, the rise mostly the four
efficiency cores. Placing the markers is the serial part (0.5–0.6 s), with
building the tree and writing 175 MB of answer. On 74c5998, one run at
load 7.5: 13.8 → 7.8 s.

What a single thread asks barely moves: `respond_to?` in
`ActiveSupport::Tryable` (DEC-200's `--def`) 53 → 53 ms on rails, 73 → 73 ms
on mastodon, 323 (326) → 334 (339) ms at 100k files (11, 11 and 7 rounds);
every chain of the 100k corpus linearized 2,917 → 2,910 ms (5 rounds); an
LSP session clicking every name in 9 rails files (5,117 requests) 0.36 →
0.35 ms median, p99 3.88 → 3.45, and 12 mastodon files 6.34 → 6.24 ms.

Racing costs little: at eight threads against one, lookups computed rise
3 %, macro callers 11 %, string-macro names 0.2 %, chains 0.6 % — two
workers missing one key at once both compute it. Shared memos keep one key
hashed once: hashing it for the shard and again for the table cost the
linearization 3 %.

**Checked.** Every commit of the change, on 74c5998, is byte-identical to
main, as the prototype was on 4e88526, on the verify set (108 files: the gold
sets, widget_shop, the rails `--refs` 40- and 51-query sets, `--dead` on
three corpora, the probe, the clicks), on the 100k `Hash#[]`, and on the
linearization dump of rails, mastodon and the 100k corpus — asked from one
thread, and, once the tree can be shared, from eight threads sharing it,
forward and reverse. Unit tests: eight threads asking a cycle's names in
rotated orders answer as a fresh tree; `gather_refs` on one thread and on
eight, facts kept or not, give one answer; `Once` computes once under eight
askers.

**What it costs to keep.** The three kinds above, and two rules the
compiler cannot check: a memo whose answer depends on a call's context keys
on that context (DEC-251), and the tree never hands work to the pool while a
linearization frame is open (a stolen job would see the frame). DEC-204's
head start on the flow analysis is gone: each file's worker works out what
it needs.

**Rejected.**
- **A lock per memo, one map each.** Every hit takes the same lock's cache
  line; a sharded map is the standard answer and the shards cost nothing.
- **`dashmap`.** The same structure; its guards can be held across a
  computation that touches the same shard, and a wrapper hiding them was
  needed either way. Thirty lines of std and `hashbrown`'s `HashTable`.
- **A one-time initializer per key for the recursive memos.** A chain asks
  for chains, a lookup for lookups; a thread holding one key's initializer
  while it waits on another's deadlocks against a thread doing the reverse.
- **Seeding a tree per worker** (DEC-233): four times the memory and a field
  list to keep in step.
- **Precomputing the placement or the votes at `--index`.** Every query
  would pay for a few `--refs` queries' warm-up.

## DEC-251 — A lookup made while placing is its own question

**Decided.** The lookup memo's key carries whether the lookup was made
while placing markers, where DEC-235 left a lookup made then answering later
asks. A lookup made while placing does not look for string macros; one made
outside does. They are different questions, and sharing a memo entry made
the answer depend on which was asked first — with several threads, on
timing.

**Measured.** No answer moves: every check of DEC-250's list is identical.

## DEC-252 — A name's definitions are one table, loaded whole

**Decided.** The tree's methods were one arena that every load appended to,
indexed by name and by name-owner-side. They are now a table per name —
its definitions in load order, by owner and side, and `named`'s answer —
built whole on the name's first load and never changed after, which is what
DEC-202 already said of a name: complete once loaded. A lookup lands on an
index into its name's table, or carries the method a string macro made.

**Why.** The name is the unit a load publishes, so it is the unit a
concurrent reader can take whole without a lock: one map probe, then no
more locking for the walk. A shared arena needs its indexes locked for
every walk, and ties indices across names to load order.

## DEC-270 — A Ruby is found wherever a version manager put it, and a named one that is not is said

Amends DEC-152 and DEC-180's list of installs.

**Decided.** The Rubies a named version is matched against are rvm's,
rbenv's and asdf's, as before, and now chruby's (`~/.rubies/*`,
`/opt/rubies/*`), mise's (`$MISE_DATA_DIR`, else `$XDG_DATA_HOME/mise`,
else `~/.local/share/mise`, then `installs/ruby/*`) and every Homebrew keg,
`Cellar/ruby/*` and each versioned formula's `Cellar/ruby@3.3/*`. The same
installs' own gem directories join DEC-152's machine roots, matched by the
install's version. When the version a checkout names matches no install,
the checkout still runs on DEC-152's next choice, and `--index` says so —
`gems.ruby_not_found` and a `ruby —` line naming the Ruby used instead —
as does `--status`, per checkout (`ruby_not_found`, and a `!` line).

**Why.** The hunt's `.ruby-version` of `3.3`, on a machine with Homebrew's
`ruby@3.3` installed, was answered from `$GEM_HOME`'s 3.4.9 — its stdlib,
its core, its gems — and nothing said so. `ruby@*` is a directory a `*`
component cannot match by prefix, so the kegs are listed, not globbed.

*Hermetic tests.* Homebrew's and `/opt/rubies` are machine-wide, so a test
would see whatever the machine running it has installed — this one has two
Homebrew Rubies, which turned DEC-242's "the only Ruby installed" into none.
`TREKR_TEST_SYSTEM` stands in for `/` when looking for those, and the e2e
harness points it at the fixture's home.

## DEC-271 — A reindex keeps the Ruby and signatures the last one chose, unless the checkout names another

Amends DEC-180's and DEC-242's choice of Ruby and DEC-240's choice of rbs.

**Decided.** A checkout's Ruby is the one it names, when that is installed
(DEC-270); failing that, the one its last index chose, while that Ruby is
still on disk; failing that, DEC-242's chain — `$GEM_HOME`'s, the `ruby` on
`$PATH`, the only one installed. The environment can pick a Ruby for a
checkout that has none, and never takes one away or swaps it. When the
checkout names a Ruby other than the one it ran on, the new one is taken
and `gems.stdlib.ruby` says what it replaced ("in place of the Ruby at …");
when the last choice is kept over a poorer environment, it says that ("kept
from the last index (this environment finds no Ruby)").

A Ruby's signatures follow the same rule. Which rbs gem is found depends on
`$HOME` — `~/.gem`, rvm's gem directories, another Ruby's install — so the
gem a stdlib is served with stays unless the one found now is at least as
good: bundled with the Ruby (always taken), or of the same kind and no
older. A kept gem shows as `gems.stdlib.rbs.kept`.

A name nothing defines, in a checkout with no core, says "Ruby core is not
indexed for this checkout (no Ruby found for it, or its Ruby carries no rbs
gem …)" rather than claiming core was looked in.

**Why.** The hunt's repro: a checkout indexed from a shell, then reindexed
by an editor launched from the Dock — no `.ruby-version`, several Rubies
installed, no `$GEM_HOME`, no `ruby` on `PATH`. DEC-242's chain found
nothing, the reindex wrote a checkout with no stdlib, and `"x".upcase`
became "nothing trekr indexed defines this name anywhere — not this
checkout, its gems or Ruby core", for the CLI too, until someone reindexed
from a shell. The language server's background index inherits the editor's
environment, so this is every Dock-launched editor, after every branch
switch. 0.8.0's built-in core could not be lost this way.

**The cost.** A checkout that names no Ruby does not follow `rvm use` or a
new `$GEM_HOME`: it stays on the Ruby it was first indexed on. Naming the
Ruby (`.ruby-version`) moves it, and `trekr --drop` forgets the choice.
That is the trade: an explicit choice moves it, an ambient one cannot.

**Not done.** Without a lockfile, the gems' Ruby (DEC-152) is still the
environment's, and a poorer one searches every Ruby's gems, as before; the
picks stay installed versions, where the stdlib went to nothing. *Done
in DEC-291:* the gems are looked for in this Ruby's directories first.

## DEC-272 — A Ruby's bundled rbs is the one its list names, else the one written with it

Amends DEC-242's test for the bundled rbs.

**Decided.** The rbs bundled with a Ruby is, in order:

1. the version Ruby's own list of its bundled gems names — `gems/bundled_gems`
   (`rbs 3.8.0 https://github.com/ruby/rbs`), looked for at
   `<prefix>/gems/bundled_gems`, `<prefix>/lib/ruby/<abi>/bundled_gems` and
   `<prefix>/lib/ruby/gems/<abi>/bundled_gems`, where an install keeps it;
2. else the `rbs-*` in the Ruby's own gem directory written within an hour
   of the Ruby's install — by its gemspec, or by its cached `.gem`, which
   counts as written at the install when it is older than it. The install is
   dated by the **median** of its default gems' spec times, else by
   `bin/ruby`'s.

Of several, the nearest; with none, DEC-242's "the highest installed".

**Why.** DEC-242 dated the install by the newest default spec, and the rbs
by its gemspec alone. Routine gem maintenance moves both:
`gem update --system` writes a newer default spec (rubygems-update,
bundler), so the install looked months later than the rbs; `gem pristine
rbs` rewrites the rbs gemspec. Either way rbs 4.2.0 was chosen, with "none
was bundled with it" — false. The median moves only when half the default
specs are rewritten. `gem pristine` reads the cached `.gem` and never writes
it, and a Ruby's install writes it with the tarball's time (rvm's 3.4.9:
cached March 11, installed April 10) or its own (Homebrew's: both March 11),
while `gem install` writes it when downloading.

**Measured**, this machine: rvm 3.4.9 → 3.8.0, Homebrew 3.3.11 → 3.4.0,
rbenv 3.4.10 → 3.8.0 (of 3.8.0, 4.0.2, 4.0.3, 4.2.0), rbenv 4.0.6 → 3.10.0,
each `bundled` — the same as DEC-242 gave before any maintenance. None of
these installs keeps a `bundled_gems` list; step 1 is for those that do.
Both maintenance scenarios are tests (unit and e2e), and fail on DEC-242's
rule.

**Not done.** A `gem install rbs` within an hour of installing the Ruby
(a provisioning script) is still told apart only by being further from the
install than the bundled one.

## DEC-273 — A Ruby's signatures are keyed by when their gem was written

Amends DEC-240's key.

**Decided.** The `rbs` row's key folds, beside the stdlib, the gem's path
and version and the reader's code, the write times of the gem's directory,
its `core/` and its gemspec. Three `stat`s per index.

**Why.** A reinstall at the same version and path (`gem install rbs
--force`, a Ruby rebuilt in place) kept the stubs read from the old files.
Hashing the files' content would be exact but reads a thousand files on
every index, a no-op one included; a reinstall rewrites all three of these.
`gem pristine` rewrites the gemspec too, so it costs one re-read (~130 ms),
which is the right answer anyway.

## DEC-274 — Core's files are the store's own, and go when its signatures do

Amends DEC-240's "beside the database".

**Decided.** Each Ruby's core files are written under a directory of the
store's own — `trekr.db` → `trekr.core/rbs-3.8.0-<key>/` — as its tree
snapshots are (`trekr.trees/`), rather than a `core/` shared by every store
in the directory. What goes:

- *A Ruby's signatures* once no stdlib is served with them: `--gc` drops the
  `rbs` rows no `rbs_use` names — a collected stdlib's, whose `rbs_use` went
  with its checkout — and reports them as `signatures`.
- *Their files*: `--gc` removes each `rbs-*` directory under the store's own
  whose signatures the store no longer holds (`core_files`: files and
  bytes, dry run included), and so does the first open after an upgrade,
  which drops every row.
- *What earlier builds left beside the store*: 0.8.0's flat `core/String.rb`…,
  a dev build's `core/stdlib/` and `core/rbs-*`, `core/RSpec.rb`, and
  `core.rb`, on upgrade and by `--gc` — only in a `core/` whose `RSpec.rb` is
  trekr's, and a `core.rb` that is trekr's old stub, since a store may sit
  beside anything named `core`.

**Why.** The hunt: 0.8.0's flat files were never removed on upgrade (only
`core.rb` was), and neither `rbs-*` directories nor `rbs` rows were ever
collected. A shared `core/` makes collecting them unsafe: which directories
are live is one store's answer, and a second store beside it — every e2e
test's, in the temp directory — would have its live files swept. The
directory per store is what makes "not in this store" mean "garbage".

**Cost.** A path in answers moves (`<db dir>/core/rbs-…` → `trekr.core/rbs-…`);
it never shipped, since 0.8.0 wrote the flat layout.

## DEC-275 — While the index refills after an upgrade, the editor says so

**Decided.** A background index the language server starts on an empty
store that an upgrade emptied — the CLI's own test for "nothing has been
indexed since" — is marked as a refill. Its `$/progress` begins "reindexing
after an upgrade: <root>", its `index_start` log event carries
`after_upgrade`, and until it ends every hover in that checkout closes with
"trekr is reindexing this checkout after an upgrade (started N s ago). Until
it finishes, answers are partial — this checkout's code, its gems and Ruby
core may not be read yet." — in place of "not indexed yet, so answers come
from core and gems alone", which after an upgrade is false twice over.

**Why.** The hunt's `lsp2.py`: an editor on 0.8.0 hot-reloads the new build,
whose store rebuild drops the index; for the seconds the refill takes,
every hover read "nothing trekr has indexed defines `upcase`" — a claim about
the code, where the truth was about the index.

**Not done.** A percentage. The refill is a `trekr --index` child whose
phases — the checkout, the stdlib, the signatures, the gems — have totals
known only as each starts; a number that jumps backwards between them would
be worse than the elapsed time.

## DEC-276 — Requirements that conflict are said to, with where each was written

Amends DEC-134's report of a gem not found.

**Decided.** Without a lockfile, a gem no installed version satisfies was
reported "not installed" under its merged requirements. When those
requirements come from more than one place — the Gemfile, a gemspec, a
picked gem's own runtime dependency — and some installed version meets one
place's, the gem is listed in `gems.unlocated` instead, its `why` naming each
place: "requirements that conflict, which no installed version meets
together: ~> 0.90.0 (Gemfile); >= 1.89.0, < 2.0 (rubocop-rails 2.30.0)".
One place's requirement unmet, or nothing installed at all, is still "not
installed".

**Why.** The hunt's "not installed: rubocop ~> 0.90.0, >= 1.89.0, < 2.0"
named a version no one could install, and hid that the Gemfile's pin and a
plugin's dependency disagreed — which is the fix.

## DEC-260 — A block a macro makes a method of runs where the method does

**Decided.** A macro — an instance method a class body runs on itself
(DEC-162), usually a module the class `extend`s — that hands its own
`&block` (or an anonymous `&`) to `define_method` says so in its marker
(`core::Maker::block`, stored as a trailing `|&`). A call on `self` written
in a block a class body hands such a macro runs on the side of the method
it makes: `test "x" do helper end` is an instance's `helper`, and
`define_singleton_method` would make it a class method. Between the call
and the macro, a block handed to anything but Ruby's ways of changing
`self` (`instance_eval` and kin, `Class.new`) is taken to yield to it —
`[1].each`, `assert_nothing_raised`, `Dir.chdir` — as a block in a class
body is read everywhere else. A macro that yields, `class_exec`s or
`instance_exec`s its block leaves it on the class, as before.

The macro is found from its body, not a list of names:
`ActiveSupport::Testing::Declarative#test` and Minitest's `DSL#it` are the
shape, and so is an app's own.

**Why.** The 0.8.1 hunt: a call in a `test "…" do` block was read on the
class side, found nothing there, and `--refs` excluded it as
`no_such_method`; `--dead` called a helper only tests call unreferenced.
0.8.0 hid it behind Minitest's unshaped `define_method :mu_pp` marker, which
DEC-160 rightly narrowed to the one name it makes.

**Measured** on rails, against main. `--refs 'Minitest::Assertions#assert_equal'`
17,629 confirmed, 1,156 possible, 5,761 no such method → 23,485, 919, 142.
The 51-query set's `ActiveSupport::TestCase#assert_equal` moves 5,619 of its
5,761 `no_such_method` exclusions to "different owner": they now land on
`Minitest::Assertions`, the definition that query prints, which `--refs`
counts as another owner's because the query asks through the class that
inherits it — as `ActiveRecord::Base.establish_connection` gets 0 confirmed.
Its counts 0/1,156/23,390 → 0/919/23,627: 255 sites the class side called
"ancestors not fully indexed" resolve, and 18 in a module's `test` block are
possible through its includers. Elsewhere in the set, one `require` in such
a block moves confirmed → possible (it runs on the includer's instance) and
one `self.class.name` possible → confirmed. `--dead` on rails loses one
false candidate, `PageDumpHelper#save_and_open_page`, whose three callers
are all in `test` blocks. The gold sets, the 40-query set, `--dead` on
activerecord and mastodon, and the 21,154 clicks are unchanged.

**Not done.** A block handed on by a macro to something trekr does not read
— minitest's `describe`, which `class_eval`s it into a class it builds;
`setup`, which hands it to `set_callback` — stays on the class: Arel's
`describe … it` specs are most of the 142 left.

## DEC-261 — A `method_missing` that sends a name on may run any class's method

**Decided.** A `method_missing` whose body sends the name it is handed to
another object — `target.__send__(name, …)`, `@obj.public_send(...)`,
through `send`, `__send__` or `public_send` to any receiver but `self` — is
marked on its class and side as a maker of every name, by
`method_missing` (`core::FORWARDER`). Where Ruby's lookup on a receiver
finds nothing and such a marker is in its chain:

- `--refs` counts the site `possible` ("the receiver's class sends a name it
  lacks on to another object"), for a queried method of **any** class: the
  object it is sent to is not typed. DEC-130's markers reach only the
  queried owner and its subclasses, since they make that class's methods.
- `--def` answers residue naming it: "Delegator sends a name it lacks on to
  another object (method_missing, …/delegate.rb:82)".
- It makes nothing in its own file, so a name defined nowhere is not blamed
  on it.

**Why.** The 0.8.1 hunt: indexing the stdlib completed `SimpleDelegator`'s
chain, so a call through a delegator (`LoudEngine.new(engine).start`) was
excluded as `no_such_method` and `--dead` called `Engine#start`
unreferenced. The chain had been cut short at `Delegator` before, which
hedged it.

**Only a `method_missing` that sends the name on.** DEC-112 turned down
every hand-written `method_missing`: ActiveModel::AttributeMethods defines
one, so every model's `no_such_method` exclusion would become `possible`.
That one answers attribute patterns itself and sends nothing, and so is no
forwarder; neither is any in `ActiveRecord::Base`'s chain.

**Measured**, against DEC-260's build. The index marks 44 forwarders in
rails and 76 in mastodon with its bundle (`Delegator`, `Vite::Tagger`,
`ActiveRecord::Migration`, `ActionDispatch::Integration::Runner`,
`ActionView::TestCase::Behavior`, `ActiveSupport::Duration`, …). The 51-query
set moves 237 sites excluded → possible and none the other way: 232 of them
`resources` in a `routes.draw do` block of a test, which Ruby runs on the
mapper and trekr reads on the test, whose `RoutingAssertions` forwards;
`ActiveRecord::Base.new` moves 3 of its 12,869 exclusions. `--dead` loses
false candidates — rails 8 (`SchemaStatements#bulk_change_table`,
`recreate_database`, which migrations reach through `connection`),
activerecord 2, mastodon 19 (`Vite::Tagger`'s strategies' `vite_*_tag`,
which its specs reach through the tagger) — and two move to single-caller.
The gold sets, the 40-query set and the clicks are unchanged.

## DEC-262 — A stdlib library written only in C is declared by its RBS

**Decided.** DEC-220's stub declares the compiled half of a library whose
Ruby file the stdlib holds. A library with no Ruby file at all — StringIO,
Zlib, Etc, StringScanner, PTY — is now one the stdlib has when its compiled
extension is there (`stringio.bundle` beside `rbconfig.rb`, by the feature
`require` names it with), and it is compiled whole: the stub declares every
class and method its RBS writes, `defined_via: "rbs"`, as it does
`Digest::SHA256`.

**Why.** The 0.8.1 hunt: `StringIO.new(s).read` was "no indexed constant",
though the Ruby has the extension and its rbs gem describes it. Nothing
indexed declared these classes, since the stub admitted a library only by
its `.rb`.

**Measured**, against the build before it. Rails' 51-query set: 236 sites
of `ActiveRecord::Base.new` move possible → excluded — `StringIO.new` and
`Zlib::GzipWriter.new` were untyped receivers and are now StringIO's and
Zlib's — and 7 of `ActiveRecord::Core#inspect` the same way (a
`StringIO#string`); one `String#strip` possible → confirmed. The gold sets
move three residues to a declaration offered (polyid 1, flipper 2), none
the other way; the clicks lose 2 empty and 4 "unindexed ancestor or
constant" answers. The 40-query set and `--dead` on all three corpora are
unchanged.

## DEC-263 — An inherited method on a class RBS sketches stays confident, for now

**Decided: not changed.** RBS declares `Enumerator::Lazy` with one method of
its own (`compact`, in rbs 3.8 through 4.2), though Ruby's Lazy overrides
`map`, `select`, `reject`, `zip` and the rest to stay lazy. Since core comes
from RBS (DEC-240), `[3].lazy.map { }` answers `Enumerable#map` at 1.0,
returning an Array, where 0.8.0 knew no Lazy and said residue.

**Why not a rule.** Nothing RBS writes tells a sketch from a class that
really inherits: `File` inherits `Enumerable#map` through `IO` exactly as
Lazy appears to, and is right to. The rules considered each misfire:
hedging every inherited module method on a core subclass costs `File`,
`ArithmeticSequence` and friends a correct answer; hedging a class whose
own declaration is small is a threshold, not evidence; hedging what a
module's method returns when it lands back in that module (`Enumerable#lazy`
→ Lazy → `Enumerable#map`) holds for Lazy and `Enumerable#chain` alone and
names them in all but name. Two classes do not earn a mechanism.

**Reverses if** a second sketch turns up in the gold sets or the clicks, or
the rbs gem declares Lazy's overrides — then the answer is simply right.

## DEC-264 — An editor's references tier on every core

**Decided.** The LSP's file scan (`references`, `incomingCalls`) tiers each
file's calls on the pool worker that parsed it, against the session's tree,
which every worker can share since DEC-250. Each chunk still comes back to
the request's thread in file order, where the gather keeps its cap and its
order and a stream sends its batch (DEC-056); only the work that feeds them
moved.

**Why.** The scan's comment still said the tree "is not shareable across
threads", and tiered on the request's thread while the parse fanned out —
the same split DEC-250 retired for `--refs`, whose tiering is most of the
time on a common name.

**Measured** on rails, `textDocument/references` on six definitions after a
hover warms the tree, no partial-result token (the whole scan, DEC-056's
best 1,000), 7 rounds interleaved against the build before it, load 10–12,
medians (max): `Persistence#save` 93 (100) → 44 (46) ms, `Validations#valid?`
106 (108) → 66 (68), `QueryMethods#where` 126 (140) → 70 (74),
`FinderMethods#find` 112 (119) → 60 (62), `Cache::Store#fetch` 64 (66) → 38
(40), `Callbacks#run_callbacks` 6.5 → 5.5. Every answer is identical.

## DEC-265 — A bundled default gem does not hide the stdlib's stub classes

**Rejected.** The 0.8.1 hunt found `JSON::Pure` resolving from the stdlib
stub in an app that bundles json 2.21, which has no such class, and asked
DEC-180's shadowing to cover the stubs. Built and measured: a stub-only
class was left out when its outermost namespace is opened only by stdlib
files the app hides. It fixed `JSON::Pure` and took 110 classes from
mastodon's tree, 104 of them ones its bundled copies really have — every
`OpenSSL::*` class the extension or `const_set` makes
(`OpenSSL::Cipher::AES`, `OpenSSL::Digest::SHA256`, `OpenSSL::PKey::PKey`),
because mastodon bundles openssl, and `Socket::AncillaryData` and friends,
because bundled `ipaddr.rb` reopens `Socket`.

**Why it cannot be done this way.** `JSON::Pure` is not the bundled copy's
problem: rbs 3.8's json signatures still declare it, and Ruby 3.4's own json
2.9 has no `JSON::Pure` either, so every app on that Ruby resolves it. A
stub class the RBS declares and no Ruby file does is either made in C, made
at runtime (`const_set("AES#{keylen}", …)`), or stale in the RBS, and
nothing the index reads tells the three apart: the extension binary names
`State` and `Parser` but not `AES128`, and the Ruby spells `AES` only
inside an interpolation. Hiding by namespace trades one stale class for a
hundred real ones.

**Reverses if** the stub records which library each class comes from and
there is evidence a class is stale — the rbs gem marking deprecations, or a
Ruby run once at index time to list what an extension defines.


## DEC-280 — A call on a subclass runs the method it inherits

**Decided.** `--refs Owner#name` asks about the method the owner's own place
in its chain finds: its own definition, or the one it inherits
(`resolves_to`, `inherited`; `Tree::lookup_owned`, so a module prepended to
the owner is not taken for its method). For an inherited one, a site whose
lookup lands on it is `confirmed` when the receiver is the owner or a class
below it — an instance, `self` in the owner or a subclass, a `super` from an
override below it, a delegate's target. A receiver outside that subtree that
lands on the same method — the ancestor that defines it, a sibling — is
`different_owner` ("the receiver's type inherits the same method, but is no
subclass of the owner"). A receiver typed as an ancestor of the owner by a
bound is `possible`, as for a method the owner defines (DEC-140), and so is
`self` in an ancestor (DEC-081). An owner that defines the method itself is
answered exactly as before.

```ruby
class Base; def save; end; end
class Child < Base; end
class Sibling < Base; end
Child.new.save    # confirmed for Child#save
Sibling.new.save  # excluded: Sibling runs Base#save without being a Child
```

**Why.** Asked. `--refs ActiveSupport::TestCase#assert_equal` answered 0
confirmed, every call `different_owner`, because the method's owner is
`Minitest::Assertions` — though each of those calls runs it on a
`TestCase`. The question a person asks names the class they are looking at,
not the module that happens to hold the `def`.

**A guess confirms nothing it only inherits.** The naming rung types a
local by its name, and is `ambiguous` when other classes define the method
too. A guessed type that lacks the name lands wherever its ancestors define
it, which for `to_s` is `Kernel` whatever the guess: `name.to_s`, a
`String`, was typed `Name` and would have confirmed `Object#to_s`. Such a
site is `possible` ("the receiver's type is a guess, and one that inherits
this"), where the owner's own method still confirms on a guess, as before.

**Measured** against main (8b3f1c9). Every gold verdict, all 21,154 clicks
and `--dead` on rails and activerecord alone byte-identical — `--dead` asks
about each method of the owner that defines it, which this leaves alone.
The rails 40-query `--refs` set moves 171 sites excluded → confirmed in two
queries, the 52-query set 25,043 excluded → confirmed and 55 excluded →
possible in five; nothing leaves confirmed or possible:

- `ActiveSupport::TestCase#assert_equal` (resolves to
  `Minitest::Assertions#assert_equal`): 0 → 23,378 confirmed, every one an
  implicit-`self` call in a `TestCase` subclass. The 249 still excluded are
  107 calls in a test that is a `Minitest::Test` but no `TestCase` (Arel's,
  `TestChangelog`) and 142 whose receiver's type has no such method
  indexed (`no_such_method`), as before.
- `ActiveRecord::Base.new` (`Inheritance::ClassMethods#new`, extended): 0 →
  1,490, `Topic.new` and every model's.
- `ActiveRecord::Base.establish_connection` (`ConnectionHandling`'s,
  extended): 0 → 143, the base itself and models; the 39 left are the
  connection handler's and each database task's own method.
- `AbstractAdapter#execute` (`DatabaseStatements#execute`, included): 0 →
  28, the MySQL adapters' calls and the `super` in PostgreSQL's and
  SQLite3's `DatabaseStatements#execute`; SQLite3's own override stays
  excluded.
- `Object#to_s` (`Kernel#to_s`): 0 → 4 confirmed — `self` in `Object` and
  `IO`, a `DeprecatedObjectProxy.new`, a `super` from
  `BigDecimalWithDefaultFormat`, whose `BigDecimal` the index has no `to_s`
  for — and 55 excluded → possible, each a naming-rung guess. Without the
  guess rule those 55 were confirmed.

Text only: a `self` call in an ancestor, still `possible`, now says "`self`
may be the subclass that inherits this" (27 sites in the 52-query set), and
an excluded site that runs the same method from outside the owner's subtree
says so (107).

*Reverses if:* the naming rung stops guessing among equals, when a guess
could confirm an inherited method as it does an owned one.

## DEC-290 — Under `--ndjson`, a row set streams its rows and ends with the rest of the answer

**Decided.** Every row-set command — `--refs` both ways, `--dead`,
`--symbols`, `--usage` and `--usage --misses` — writes one row per line
under `--ndjson`, each exactly the element its `--json` array holds, then
one last line, `{"answer": {…}}`: the `--json` answer without its row array,
plus `rows`, the number of lines before it. A command whose `--json` is a
bare array ends with `{"answer": {"rows": N}}`. The last line is always
written, an empty set and `--refs`' `no_such_method` included. `--json` is
unchanged. It is `ndjson_rows`, beside DEC-230's streaming writer, and both
`emit_listing` and `emit_rows` end in it.

**Why.** `--help` promised "one compact object per line", and `--refs
Owner#m` and `--dead` printed one line holding the whole answer, its rows a
nested array — `--json` without the whitespace. A streaming reader got
nothing until the end and then everything at once. The bare-name `--refs`
and `--symbols` did stream, with no end: an empty set printed nothing, which
reads the same as a crash, and the count lived nowhere.

**Why a wrapper key.** A reader has to tell the last line from a row as it
reads, before it knows the line is last. The rows have no field in common
to switch on, and adding one would make an `--ndjson` row differ from its
`--json` element. So the tail is one key no row has. Not `summary`: `--dead`
already has one, a tally of its candidates, and `{"summary": {"summary":
…}}` would make that name mean two things (DEC-080). `answer` is what it
holds.

**Why last.** `--refs`' `counts` and `--dead`'s `summary` are totals of the
rows, known once they are; and a last line is the reader's proof the stream
finished.

*Reverses if* rows start being written before they are all gathered, and a
reader needs the head (`definition`, `status`) first. Then the head goes
first as its own line, and the tail keeps `rows`.

## DEC-291 — A checkout's gems are looked for in its Ruby's directories first

Amends DEC-152, and closes DEC-271's "Not done".

**Decided.** The Ruby a checkout runs on (DEC-271: the one it names, else
the one its last index chose, else the environment's) is chosen before its
gems, and its gem directories — beside its stdlib, `~/.gem/ruby/<abi>`,
rvm's for that install and its `@global`, Homebrew's for a Homebrew Ruby,
and `$GEM_HOME`/`$GEM_PATH` when they are this Ruby's — are searched right
after the project's own (`BUNDLE_PATH`, `vendor/bundle`).

- *With a lockfile*, every other Ruby's directories follow, as before. A gem
  found only there is still indexed and is listed in `gems.other_ruby`
  (text: "found only in another Ruby's gems"), except a version the
  checkout's Ruby ships as a default gem, which is its stdlib
  (`from_stdlib`). A git checkout not found is reported where bundler on
  that Ruby would put it.
- *Without one*, a name is picked from the Ruby's directories when any
  version there meets it, and otherwise from the environment's Ruby —
  `$GEM_HOME`/`$GEM_PATH`, the `ruby` on `$PATH`, or every Ruby — and is
  listed in `gems.other_ruby`. `gems.ruby` is the Ruby's own sentence,
  the stdlib's. With no Ruby chosen, DEC-152's chain alone, as before.

**Why.** The 0.8.1 first-time tester's repro: once-campfire names Ruby
3.4.10 (rbenv), and the shell's `$GEM_HOME` is rvm's 3.4.9. The stdlib came
from 3.4.10, and every gem from 3.4.9's directories, because they were
searched first: `SecureRandom.hex` answered from
`~/.rvm/gems/ruby-3.4.9/gems/securerandom-0.4.1`. Exact versions are the same
Ruby code, but a compiled extension is built for one Ruby, and the answer
named a Ruby the checkout does not run on. Without a lockfile the gems came
from the environment's Ruby even when the stdlib was a kept one.

**Measured.** once-campfire: `SecureRandom` answers from
`~/.rbenv/versions/3.4.10/.../securerandom-0.4.1`; 25 of its 77 gems are
found only in rvm's 3.4.9 and are said, where none were. The git gems'
missing checkouts are reported under 3.4.10's `bundler/gems`.

**Why fall back at all.** A gem installed only for another Ruby is usually
a `bundle install` run from the other shell, and the same version's Ruby
code; not indexing it would turn every call into it into residue. It is
said, and a lockfile's exact version keeps it honest.

## DEC-292 — The checkout's Ruby is an object in `--index` and `--status`

**Decided.** `--index --json` has a top-level `ruby` beside `repo`, and each
checkout row of `--status --json` one beside `stdlib`: `{"version", "root",
"how"}`. `version` is the Ruby's own (`rbconfig.rb`'s `MAJOR.MINOR.TEENY`,
else the install directory's name), `root` its stdlib's root — the checkout
it is indexed as, as `gems.stdlib.root` — and `how` one of `named` (the
checkout names it), `gem_home`, `path`, `only` (the environment's, DEC-242),
or `kept` (the last index's, DEC-271). `null` when no Ruby was chosen: none
found, or `--no-gems`. The sentences (`gems.stdlib.ruby`, and `gems.ruby`
without a lockfile) stay, for text and for a person reading JSON.

**Why.** A script that wanted to know which Ruby answered had to parse "the
Ruby at ~/.rbenv/versions/3.4.10, kept from the last index (this environment
would pick …)". The facts were known as a value and printed only as words.

**`--status` asks this environment.** The store keeps which stdlib a
checkout's last index chose, not how. `--status` runs the same choice now,
with that stdlib as the last one, and reports its `how` when it lands on
the same Ruby; `null` when it would land elsewhere, which the next `--index`
does. Keeping the index's own reason would be a column, and a store version,
for a field whose only use is "why is this the Ruby".

**Not done.** `prefix` — the install directory — is `root` less
`lib/ruby/<abi>`, and nobody asked for it.

## DEC-293 — Without a lockfile, a Gemfile's git gem is its one checkout, or it is said

Amends DEC-134.

**Decided.** A Gemfile's `gem "x", github: "owner/repo"` or `git: "…"` with
no `Gemfile.lock` is looked for as bundler checks one out: a directory
`<repo>-<12 hex>` in the `bundler/gems/` beside each gem directory searched
(DEC-291's order, the checkout's Ruby's first), narrowed by a hex `ref:`.
Exactly one: that checkout is the gem, in its gemspec's directory as
DEC-150 finds it, counted in `gems.from_git`, picked as `name <revision>`,
its own gemspec's runtime dependencies joining the picks. None, or more
than one: the name is in `gems.unlocated` with why — "no checkout of rack in
~/.rvm/gems/ruby-3.4.9/bundler/gems", or "2 checkouts of rack …, and nothing
says which revision". Either way no other requirement picks a registry
release for that name: bundler takes the Gemfile's source for every
requirement on it. A `branch:` or `tag:` is not read; they name no
revision a directory carries.

**Why.** The 0.8.1 first-time tester: a Gemfile's git gem, without a
lockfile, was in none of `missing`, `unlocated` or `picked` — the one
failure that said nothing. faraday-gitsrc's `gem "rack", github:
"rack/rack"` now indexes `rack-a9833c8f3bd6`, the one checkout on the
machine; draper-gitsrc's `rails` and `mongoid` say there is none.

**Why the one checkout.** With no lockfile nothing names a revision, and
DEC-134 already takes "the highest installed" as the stand-in for what
`bundle install` would have locked. One checkout is the installed one. Two
have no order a directory name gives — the revision is a hash, and the
checkout time is not the commit's — so it is said rather than guessed.

**Also.** `--index` text says when the Gemfile is newer than
`Gemfile.lock`: the lockfile is what is read, and a Gemfile edit counts
only once `bundle install` relocks. A `path:` gem without a lockfile is
still left out; it is DEC-150's third kind of checkout.

## DEC-300 — A store trekr can't use is set aside and rebuilt, or kept for a newer trekr

**Decided.** rq's D51, in trekr's terms: `src/store/recover.rs`, its tests, and
`tests/cli_e2e.rs` and `tests/lsp_e2e.rs` for the binary and the server.

**The problem.** The store is a cache (DEC-009), but one trekr couldn't use left
every command failing until someone deleted it by hand: a file that isn't a
database, a truncated one, or a rebuild that errors all came back as exit 74, every
run. An older trekr meeting a newer store refused it ("upgrade trekr") — no worse
than an error, but two installed versions, a brew release and a dev build, meant
one of them always failed. And a language server whose store another trekr rebuilt
said "answers stop here" and waited for a restart.

**What.** `Store::open` sorts every way an open fails into one of three:
- **Broken** — SQLite says the file is corrupt or not a database, at any statement
  of the open (including the schema read that ends it, which is where truncation
  shows: SQLite compares the header's page count with the file's size on the first
  read); the drop-and-create fails for a reason other than the environment; or a
  table the schema needs is missing. The file and its WAL move to
  `<name>.broken-<unix time>`, the newest such copy the only one kept, and a fresh
  store is built. One stderr line says so and where the old file is. The rebuild is
  recorded in `upgrade` — the old version for a failed rebuild, this version for
  damage — so `--status`/queries say `not_indexed` with the reason and the editor's
  refill says it is one (DEC-275). A failed rebuild rolls back, so the copy kept is
  the store as the older trekr left it.
- **Newer** — the version is above this trekr's. The file is never written. This
  trekr uses `<stem>.v<its version>.db` beside it, says so once when it creates it,
  and works normally; core's files follow it (`trekr.v51.core/`), so a sweep of the
  main store's core directory never takes the early store's files.
- **Failed** — busy, locked, disk full, I/O, permissions, read-only. A new file would
  fail the same way, so it stays an error.

**Concurrency.** A `<store>.lock` flock, shared while opening and exclusive while
setting a store aside, plus the (device, inode) the opener saw before opening: the
one that still sees the file it failed on moves it; any other opens what is there
now. The eight-thread test fails without the re-check (a second move loses the
first rebuild's writes) and the six-process test says the line once. The WAL moves
before the store, so a new store never adopts the old WAL; a process still holding
the old file finds out on its next write (`SQLITE_READONLY_DBMOVED`) or, for the
server, sooner.

**The language server** asks between messages whether its store was replaced — the
path's inode is no longer the one it opened, or its `user_version` is no longer this
build's — and reopens through the same `Store::open`: onto the rebuilt file, or onto
its own store beside a newer trekr's. It drops its trees, re-enables refreshes,
reindexes the workspace root, and tells the person once (`showMessage`, info). When
the binary at the launch path changed, it leaves the store to the hot reload
(DEC-050) instead, since the new build opens its own. At most three reopens
per session, so a store that reads as replaced straight after reopening can't turn
the loop into a reopen per message.

**The stamp without a version bump.** `meta(key, value)` holds `schema_by`, written in
the rebuild's transaction, and the side-store message quotes it ("written by a newer
trekr (0.9.0)"). It is in `SCHEMA` and `TABLES` but also in `schema::OPTIONAL`: a v51
store an older trekr built lacks it, and nothing needs it to answer, so a missing
`meta` is not a broken store and it did not earn a bump — which would have dropped
every user's index for a line of text. An older trekr ignores the table and never
drops it (it isn't in its `TABLES`), which costs nothing: the next rebuild by this
code drops and recreates it. Rejected: a last-opener stamp, a write on the read path
whenever the binary changes, for nothing that needs it.

**Early stores, not refusal or rebuild.** Refusing fails every command of one
installed version; rebuilding would ping-pong, each version wiping the other's index.
An early store costs a second index and a cold first run. When this trekr lays down a
schema at the main path it deletes its own version's early store (the main path is its
own again) and an older version's unused for 30 days, probed by name rather than by
listing the directory. Rejected: adopting an older trekr's early store — it is a cache
at an older schema some older trekr may still be writing.

**Not done.** No `PRAGMA quick_check` on open: it reads every page, 20–30 ms on rq's
12 MB rails index, and WAL gives no unclean-shutdown signal to reserve it for; damage
deeper than the open reads still fails the command that reaches it. `--status` does
not list early stores or the kept copy (the stderr line names them), and `--gc` does
not delete them; both are one-line follow-ups outside the store.

**Reach.** trekr 0.8.0 and older still refuse a newer store with "upgrade trekr" (no
ping-pong, but no early store either); the first store bump after this release is the
first an older trekr steps around.

**Reverses if** early stores pile up in practice — then refusing with a clear message
is the simpler shape.

## DEC-310 — A string of code on Object hedges only the names it spells

**Decided.** Three readings narrow what a string of code marks (DEC-160,
DEC-161, DEC-162):

- **A name in `defined?(…)` is asked about, not called.** minitest's
  `infect_an_assertion :assert_mock, :must_verify if
  defined?(infect_an_assertion)` recorded the `defined?` as a body call
  handing the macro no names, so its shape `{1}` became `*` on
  `Minitest::Expectations`, which minitest/spec includes into `Object`. Such a
  call is neither a body call nor a macro to expand.
- **A constant of the same file holding the code is that code.** pry's
  `self.class.class_eval(*Pry::BINDING_METHOD_IMPL)`, in `Object#__binding__`,
  evaluates a constant assigned `[<<-METHOD, __FILE__, __LINE__ + 1].freeze`.
  A `class_eval` handed a constant (or its splat) that the file assigns a
  string, or a list headed by one, marks by the `def`s that string spells:
  `__pry__`, not `*`.
- **A comment line is not code, and a string that spells no `def` makes
  nothing.** The same heredoc's comment says the definition is "eval'd",
  which DEC-160's "may make methods some other way" words matched. A string
  with no interpolation, no `def`, no `define_method`/`attr_`/`alias`/…
  and no `include`/`extend`/`prepend` — pry's `class_eval("binding")`,
  rails' `eval "class Foo; yield; end"` — marks nothing.

**Why.** The 0.8.1 first-time testers: in a normal gem the certain answer
never fired. `Flipper::Gate#zz` and `Faraday::Error#zz` were residue, "Object
defines methods its source does not name", from a development dependency's
reopening of `Object` (pry in faraday's bundle, minitest in flipper's).

**The dev-dependency filter, not taken.** Hedging only what a marker's source
could reach — dropping a gem the Gemfile puts in `:development`/`:test`, or
`add_development_dependency` — was the other candidate. It is the wrong
question: the specs are code being asked about too, and they do load pry and
minitest, so their patches are real there. A marker that spells its names is
right for both. After the three readings no unshaped marker on `Object`,
`Kernel`, `BasicObject`, `Module` or `Class` is left in flipper's, faraday's,
draper's, mastodon's, once-campfire's or rails' indexes except
ActiveSupport's `class_attribute`/`mattr_*` macros, which DEC-162's filter
already keeps off every caller.

**Measured**, the card sweep (`X#zz_nope`, `X.zz_nope` for every class and
module the checkout defines): flipper 430 residue of 476 → 73 (357 back to
`no_such_method`; 42 of the rest now name an ancestor not indexed, which the
marker's reason had hidden), faraday 135 of 176 → 14 (121 back). Mastodon
and once-campfire unchanged; rails 4,331 of 13,550 → 4,329, the two strings
above that make no method on the class they are evaluated in.

## DEC-311 — A top-level `def` is reached by an implicit call that finds nothing else

**Decided.** A method defined at the top level — a spec/support helper, a
script's — is Ruby's private method of `Object`. `--dead` asks about it with
the top level as its owner, and a site is tiered against it by its receiver:

- an implicit or `self.` call whose receiver's lookup finds no method of
  the name is `possible` ("a method defined at the top level is every
  object's");
- one whose receiver has a method of its own by the name is excluded,
  `different_owner`;
- an explicit receiver is excluded: the method is private.

**Why.** The 0.8.1 testers: mastodon's `mock_omniauth`
(`spec/support/omniauth_mocks.rb:5`) was `unreferenced`, clear, while
`--refs mock_omniauth` listed its two callers in
`spec/requests/omniauth_callbacks_spec.rb`. The query named an owner of `""`,
which no call's lookup ever lands on, so every site was excluded.

**Not placed on `Object`.** The tree still gives no class a top-level
method, and `--def` on such a call stays residue. Placing them would make a
script's `def run` answer every implicit `run` in the checkout that finds
nothing else — right when the script is loaded, a confident wrong answer
when it is not — and `main`, where top-level calls run, is not typed at
all. `--dead` only needs to know a call may reach the method, which
`possible` says.

**Measured.** `--dead spec/support lib/tasks` on mastodon: 27 candidates →
26, `mock_omniauth` no longer listed.

## DEC-312 — A symbol a macro defines by is no call

**Decided.** DEC-037 records every bare symbol handed to a call as a
possible call of that name, receiver unknown. Two kinds are not:

- **A defining macro's names**, sent to `self`: every argument of
  `attr_reader`/`attr_writer`/`attr_accessor`/`attr`, `class_attribute`
  and the `mattr_*`/`cattr_*`/`thread_*` family, and the first of `scope`,
  `alias_method`, `alias_attribute`, `define_method` and
  `define_singleton_method`. The macro declares the method (DEC-111), which
  is where `--refs` lists it; the symbol is not also a caller of it.
- **Only a macro that always defines.** `has_many`, `belongs_to` and
  `attribute` define in ActiveRecord but *name* in an ActiveModel::Serializer
  — `attribute :reblogged` is how the serializer reaches `def reblogged`.
  Taking every macro DEC-111 models, as first written, moved 42 of
  mastodon's serializer methods from `convention-only` to `unreferenced`,
  clear; they stay references.
- **An operator's operand**: `object.type == :ordered`, `opts[:limit]`.
  An operator takes a value, never a method's name.

`delegate`'s names stay references: each is the target's method, which
the delegation calls, and `--dead` weighs the target's methods by them.
`alias_method`'s second argument, `accepts_nested_attributes_for`, a
callback (`before_save :x`), `helper_method` and `private :x` are
unchanged.

**Why.** The 0.8.1 testers: `--refs 'User.ordered'` listed four other
models' own `scope :ordered` declarations as possible references, and
faraday's `attr_reader :url_prefix` (connection.rb:28) was a possible
caller of the reader it declares.

## DEC-313 — A class macro's mixin lands on the class body that calls it

**Decided.** An `include`, `prepend` or `extend` of a constant written in a
method, on `self`, unconditionally and outside any block, is recorded as a
`macro` edge on the method's owner (`core::MacroMixin`:
`include|.delegate_all|Draper::AutomaticDelegation`). Once a tree is built,
each such edge is placed on every class whose body calls the method on
itself (DEC-162's body calls) and whose class-side lookup of it lands on
that method — a subclass of the class that defines `def self.delegate_all`,
or a class that extends the module whose instance method it is. The class
gains the mixin as if its body wrote it: after its own mixins, so first
among them in its chain.

- **Still no lexical edge.** DEC-097's reason stands: `has_secure_password`'s
  `include` in `ClassMethods` means the model, and recorded on the module
  it invented an ancestor. The edge is placed only on a caller.
- **Not in the snapshot.** The edges are read and placed per tree, after
  the namespace, because a caller's body call is no namespace fact: a class
  that starts calling the macro would not move the snapshot's key. Chains and
  lookups made while placing are made without the new edges and dropped.
- **A mixin that may not run is no edge**, as DEC-097 has it: `include
  Extras if on` in the macro adds nothing, and nor does one in a block.
- **One level.** A macro whose caller gains the macro's own caller's next
  macro is not followed; nor is a macro called from `included do` or a
  block, as for DEC-162.

**Why.** The 0.8.1 testers: draper's `delegate_all` is `def self.delegate_all;
include Draper::AutomaticDelegation; end`, called in each decorator's body.
`--ancestors CommentDecorator` lacked `AutomaticDelegation`, so every call the
decorator delegates to its object was "no such method".

## DEC-314 — A module's own call that no includer answers runs on an Object

**Decided.** A call on `self` in a module's instance method runs on
whatever mixes the module in. When the index knows no class that does, or
none that answers the name, the lookup continues along `Object`'s chain:
every object a module is mixed into is an `Object`, so `Kernel#Pathname`,
`Integer()`, `Array()`, `String()`, `raise` and `format` are what it runs.
`--def` answers `resolved`, `resolved_via: "object"`; `--refs` tiers the site
against that method as for any typed receiver. An includer that has the
name still answers first (`via_includers`), so a module's `to_s` is its
includer's own where the index sees one.

**Why.** The 0.8.1 testers: `Pathname(path)` in a Rails helper module was
residue, "no class the index knows of mixes it in", with `Kernel#Pathname`
its first candidate. Rails mixes helpers into the view context by name at
runtime, so no includer is ever written; the same held for `raise` and
every other Kernel method in such a module.

**Not `BasicObject`.** A module mixed only into a `BasicObject` subclass
(a proxy) has no `Kernel`; that is rare enough, and says itself by
`method_missing`, that the fallback takes `Object`.

## DEC-315 — `--dead` names the callers it cannot see, per row

**Decided.** Two blind spots the README stated are now said on each row they
touch, in `caveat`, which grades the row `lower`:

- **A view template.** trekr reads no templates. `--dead` lists the
  checkout's `*.erb`, `*.haml`, `*.slim`, `*.jbuilder`, `*.rabl` and
  `*.builder` files (`git ls-files`) and takes the identifiers of the Ruby
  they run: an ERB tag's inside, less `<%#` comments; a Haml or Slim line
  that starts with `-`/`=`, what follows a tag's `=`, `{` or `(`, and every
  `#{…}`; a Jbuilder file whole. A string's text is not a name (`t(".edit")`
  is an i18n key), its `#{…}` is. A candidate whose name is among them says
  "named in a view (app/views/…), which is not read", naming the first
  template. It is evidence of a name, not of a caller: no receiver is typed,
  so the tier stays.
- **A protocol hook.** A method Ruby or Rails calls by its name, which no
  call site writes, says "a hook *caller* calls by name"
  (`resolve::refs::protocol_hook`). The list, with who calls each:
  - Marshal: `marshal_dump`, `marshal_load`, `_dump`, `self._load`;
    YAML (Psych): `init_with`, `encode_with`; `pp`: `pretty_print`,
    `pretty_print_cycle`.
  - Ruby's operators and conversions: `to_s` (interpolation), `inspect`
    (`p`), `hash` and `eql?` (a Hash key), `==`, `<=>` (Comparable, `sort`),
    `===` and `=~` (`case`), `each` (Enumerable), `call` (`.()`, anything
    handed a callable), `to_proc` (`&`), `coerce` (Numeric arithmetic), the
    implicit conversions `to_str`, `to_ary`, `to_hash`, `to_int`, `to_io`,
    `to_path`.
  - Ruby's dispatch and hooks: `method_missing`, `respond_to_missing?`,
    `initialize_copy`/`_dup`/`_clone`, and on the class side `inherited`,
    `included`, `extended`, `prepended`, `method_added`, `const_missing`.
  - Rails: `to_param` (URL helpers), `to_partial_path` (`render`),
    `to_model`, `persisted?` and `model_name` (form, URL and i18n helpers),
    `to_key` (`dom_id`), `to_attachable_partial_path` (Action Text),
    `as_json` and `to_json` (`render json:`), `serializable_hash`,
    `cache_key`, `cache_key_with_version`, `cache_version` (cache helpers),
    `read_attribute_for_serialization`, `read_attribute_for_validation`,
    and a job's `perform` (`perform_later`, Sidekiq).

  The sources are each caller's own contract: Ruby's `Marshal`, `Psych`,
  `PP`, `Comparable`, `Enumerable`, `Numeric#coerce` and the implicit
  conversion protocol (`doc/implicit_conversion.rdoc`); ActiveModel's
  `Conversion`, `Naming` and `Serialization`; ActionText's `Attachable`;
  ActiveSupport's JSON encoding and `ActiveSupport::Cache`'s key expansion;
  ActiveJob's `perform_now`. Many of these are already `override` where
  core's or ActiveSupport's own definition is indexed (DEC-121); the caveat
  is for the rest.

**Measured, and why not a blanket caveat.** The first idea was to caveat
every method in `app/models`, `app/helpers` and presenters of an app with
`app/views`: mastodon 754 rows, once-campfire 189. Reading the templates
names 397 and 69, and a sample of each is a helper, a predicate or an
attribute the template does call (`FlashesHelper#user_facing_flashes`,
`ApplicationHelper#render_initial_state`, `ApplicationPlatform#chrome?`,
`MessagesHelper#message_timestamp`). What it cannot tell apart is a
controller action whose name a template also writes (`edit`, `disable`),
which is already `convention-only` or `super-only`. Rails' `--dead
activerecord/lib activemodel/lib actionpack/lib` gains 21 (actionpack's test
fixtures are templates), activerecord alone none. Protocol hooks: rails 38
rows, activerecord 49, mastodon 12, once-campfire 7. Tiers are unchanged;
`clear` falls on mastodon 2,837 → 2,525 and once-campfire 330 → 258.

## DEC-316 — `--dead` counts an alias's calls as its target's

**Decided.** A call of `array?`, where `alias :array? :array` (or
`alias_method`) is written beside `def array` in the same scope and side,
runs `array`'s body. `--dead` tiers the alias's call sites against the
owner as it does the method's own and adds them to its evidence.

**Why.** Exposed by DEC-312. activerecord's `PostgreSQL::Column#array` is
reached only as `array?`, and had been `convention-only` because
`opts[:array]`, a hash key, counted as a symbol reference; once an
operator's operand stopped counting it became `unreferenced`, clear. The
alias was never evidence before, and the hash key was the wrong evidence.

**Not done.** An alias written in another file, or in a reopened class's
other body, is not found: the aliases are read from the candidate's own file.

## DEC-320 — An answer from a partial index says so, and claims nothing it cannot back

**Decided.** A checkout's first index — no map stored yet, or one an index
left unfinished — marks the checkout in `meta` (`warming <root>` = the
writer's pid, files read, files the tree will span) before its first row is
written, and removes the mark in the commit that writes its gems. A reindex
of a whole map is not marked: it replaces one whole map with another in one
write. While the mark stands:

- every query answer (`--def`, `--refs`, the card, `--ancestors`) carries
  `warming: {read, of, interrupted, hint}`;
- `confidence` is scaled by the share of the tree read, floored to two
  places, so it never rounds up to whole;
- a certain absence is not: `no_such_method` becomes `residue`, with the
  counts in its `reason`, and `--refs` lists a site the receiver would rule
  out as `possible`, last, with its `ruling` kept, instead of `excluded`;
- a miss exits `2`, "no answer yet" — the code `not_indexed` already uses,
  whose fix, `trekr --index`, works here too: it waits for the running
  writer (DEC-139) and finishes what is left. An answer still exits `0`;
- `--dead` lists nothing (`status: warming`, exit 2): each of its rows is a
  claim that no caller exists anywhere;
- text output adds one stderr line saying how much is read;
- in the language server, a hover ends "trekr is still indexing this
  checkout (N of M files read), so this answer may change", completion is
  `isIncomplete`, and references rule nothing out. A definition has no field
  to say it in; the `$/progress` the index already reports is the editor's
  disclosure there. (*Amended by DEC-331:* the first definition or references
  asked says so once, as a `window/showMessage`.)

A mark whose pid is no longer running is `interrupted: true`, and its hint
is the index that fixes it; the language server starts that index itself.

**Why.** The lead's measurement: polling `--def` on discourse during a
cold background index answered `not_indexed`, then `resolved` from the app's
`lib/freedom_patches/` while the gems were unread, then `resolved` from the
gem — the middle answer confident, different from the final one, and not
disclosed. The checkout's own rows commit seconds before its gems' (on the
100k corpus, at 13 s of 19 s), and in that window an answer could not tell
it was partial: `has_checkout` was the only test, and it was already true.

**Shape.** `warming` is rq's word for an index still being built, and exit
`2` is trekr's counterpart of rq's `warming` exit (DEC-067). `status` stays
the verdict, so `resolved` is still `resolved` — a caller that branches on it
sees the same answer, and `warming` is the field that qualifies it. The
confidence is the read share because that is what backs the answer: the
files that could hold a nearer definition that have been read. It is not a
probability that the answer is final, and a constant's 1-or-0 (DEC-008)
stays 1 or 0 once the mark is gone.

**Where.** `meta` rather than a column on `checkout`: no reader needs the
mark to answer, so a store an older trekr built at this version, without
`meta`, is not rebuilt for it, and no store version moves. The pid check
means a crash leaves a checkout honestly partial instead of claiming an
index under way forever; a reused pid reads as running, which keeps the
checkout partial until its next index — the safe side.

**Measured.** discourse, release build, a fresh store per run, the editor's
own sequence over `trekr --lsp` (open `app/models/about.rb`, then hover,
definition, references, completion at fixed positions every 0.2 s until the
index ends), five interleaved rounds. Hovers that differed from the final
answer with nothing said: 8–9 per run → 0. Time to each first answer and to
the index's end unchanged within noise (index end 4.22 → 4.35 s medians).
The extra query per command is one primary-key read of `meta`.

**Not done.** "N of M" moves at the commits there are: none before the
checkout's own write, all its files at it, and the rest when the gems land.
Finer steps come with a first index written in batches (DEC-322).

## DEC-321 — The open file's own answers wait for the first part, not a tree of the buffer

**Decided.** Nothing answers same-file questions from the editor's buffer
alone before the index has the file. Outline, locals, variables and syntax
errors already need no index and answer at once; a call to a method the
same file defines, or a constant it declares, waits for the first part of a
first index (DEC-322), which holds the open file.

**Why.** Measured on a never-indexed checkout over `trekr --lsp`, five
interleaved rounds, medians: same-file definition and hover answered at
1.7 s on discourse and with the checkout's own files at 13–15 s on the 100k
corpus before DEC-322; after it, 0.28 s and 0.33 s. A tree assembled from
the open buffers — an in-memory store per session, rebuilt per edit, swapped
for the real one when the first part lands — would move that to about zero,
for a quarter of a second in a window that ends before a person has read the
file, at the cost of a second source of trees the rest of the server must
know to distrust.

**Reverses if** the first part takes long enough to see: a scan slower than
git's (`scan` is 0.4 s of it at 100k files) or a checkout where git is slow.

## DEC-322 — A first index reads what the editor has open first

**Decided.** A first index (DEC-320) writes in parts, each its own commit:

1. the files the language server says are open — a path a line on the
   child's stdin, sent at spawn and as each opens — and up to 256 files
   their constants most likely live in, by the autoloader's convention
   (`scan/near.rs`), each enclosing scope first, at most four files a name;
2. the Ruby's stdlib, its signatures, and the gems whose `lib/` holds one of
   those constants' files, with the whole bundle recorded as the checkout's,
   so a gem already on this machine answers too;
   (*amended by DEC-330:* the signatures follow in a commit of their own,
   and no other gem is read before this commit;)
3. files opened while 2 was read, and their neighbours;
4. the rest of the checkout, as one whole write — bulk-loaded when it is
   half the store or more (DEC-234), as before;
5. the rest of the gems, and the mark cleared.

A part adds paths to the map (`Store::write_part`) and folds the keys over
what the map then holds; the whole write in 4 diffs against that, so the
store ends as one write would have left it. Without a language server
nothing is told, step 1 writes nothing, and the order is stdlib, the
checkout, the gems. A reindex of a whole map is unchanged.

**Measured.** A never-indexed checkout, `trekr --lsp`, one file open, the
same requests every 0.2–0.3 s until the index ends; five interleaved rounds,
medians, the build before this change against this one with DEC-323:

| first useful answer | discourse before | after | 100k before | after |
| --- | ---: | ---: | ---: | ---: |
| definition, a constant in another app file | 1.51 s | **0.25** | 15.2 | **0.32** |
| definition, a constant in a gem | 3.62 | **0.71** | 20.1 | **1.3** † |
| hover with the definition | 4.27 | **0.74** | 17.9 | **0.35** |
| references | 1.51 | **0.25** | 20.2 | **0.35** |
| completion | 1.51 | **0.27** | 20.2 | **0.35** |
| same-file definition | 1.72 | **0.28** | — | — |
| the index ends | 4.34 | 4.46 | 20.1 | 19.9 |

† one verbose run; the 0.35 s answer before it is the corpus's own copy of
the gem, then the gem's, at 1.3 s.

Cold `trekr --index`, five interleaved rounds: discourse 3.86 → 3.82 s,
mastodon 3.00 → 3.05 s, 100k 16.8 → 16.9 s. Every store hashes the same
table by table (oids for rowids, timestamps dropped). `--index --json` is
the same on discourse and mastodon; at 100k `parsed` for the checkout falls
by 179, the stdlib's files the corpus also holds, which are now read as the
stdlib's first — the same work, attributed to the stdlib.

**Tried and not taken.** All gems before the checkout: gem answers at 2.2 s
on discourse instead of 0.7, and the checkout's bulk load then rebuilt its
indexes over the gems too — 288 → 579 ms, cold index +4.5% on discourse.
More parts for the rest of the checkout, so a file opened late is read
early: each part is a commit, and a part of the rest that is not the whole
rest gives up the bulk load — DEC-057 measured committing every 20k files
at 1.5× the time.

*Amended:* a file opened after an earlier part read it as another's
neighbour still brings its own neighbours forward; it was dropped as already
read, so discourse's `user.rb`, a neighbour of `about.rb`, waited for the
whole checkout for its `Roleable`.

**Not done.** A file opened once step 4 has begun waits for it: at 100k,
from about 1 s to 17 s in. (*Amended by DEC-332:* it is written to an early store.) A `require_relative`'s target is not followed
(facts keep no strings).

## DEC-323 — A request is answered within a second while a first index fills the store

**Decided.** In the language server, a tree built while the checkout's first
index was still running (DEC-320) is not rebuilt on the request thread when
the store moves under it. Its successor is built on another thread with its
own connection, and the request waits for it at most 400 ms (`ASIDE`), then
answers from the tree it has — which says it is partial, with the counts
the tree was built at. The serve loop puts finished trees in place at its
quiet moments. A tree whose index has ended without moving its stamp is
rebuilt too, so it stops calling itself partial. Completion waits for its
member listing at most `ASIDE` as well, always, and otherwise answers with
the locals alone, `isIncomplete`, so the client asks again as the word
grows. A tree built from a whole index is rebuilt where it was, as before.

**Why.** The lead's bar: from a never-indexed checkout, every request
answers usefully within about a second, and none waits on the full index.
After DEC-322 the first answers came at a third of a second on the 100k
corpus, but each commit moved the stamp and the next request rebuilt the
tree on the request thread — in one traced run 1.6 s after the gems, 8.6 s after the rest
(the child's ANALYZE and snapshot competing), and completion's listing
4.8–7.3 s. Requests queue behind one another, so a stall holds every
request behind it.

**Measured.** 100k corpus, `trekr --lsp`, five interleaved rounds, medians
of each run's slowest request, the build before DEC-320 against this one:
definition 2.71 → 0.55 s, hover 2.29 → 0.41, references 0.02 → 0.85 (it
now scans while the index writes; before, it had nothing to scan),
completion 5.43 → 0.41. On discourse every slowest request is 0.41 s or
less. The index's end is unchanged (20.1 → 19.9 s at 100k).

**The cost.** Up to two trees in memory for the length of a build, during a
first index only. A completion asked in the seconds after the index ends at
100k lists locals until the listing lands (about 5 s), where it used to wait
for it. (*Amended by DEC-332's addendum:* where the tree it replaced was
listed, that listing answers meanwhile.)

**Not done.** A definition from a partial tree has no field to say so;
the editor's progress is its disclosure (*amended by DEC-331*). References on the 100k corpus can
take 0.85 s mid-index, scanning files the store holds then.

## DEC-330 — A first index reads the gems its open files name before anything else beyond the checkout

**Decided.** A first index (DEC-322) lists the bundle's gems and reads none
of them up front. The gems the open files' constants most likely live in
(`scan/near.rs`, now from the listing) are read, then written with the
Ruby's stdlib and the bundle's record in one commit; the Ruby's signatures
follow in a commit of their own; the rest of the gems are read when their
own commit comes, after the checkout's. A reindex reads them all before its
one gem commit, as before, the signatures in it.

**Why.** Traced on the 100k corpus (the child's own timeline, one file
open): scan 0.25 s, the first part 0.02, locating the gems 0.005, the
stdlib's listing 0.155 (fixed apart: it hashed ~980 files to keep 179), the
gem walk 0.29–0.39 — reading and hashing 11,685 files of 304 gems to find
the 2 the open file names — the stdlib 0.04, its signatures 0.17–0.20, the
named gems 0.03. The named gems' commit landed at about 1.1 s; most of what
went before it was gems nobody had asked about. The signatures are core's
methods and return types (DEC-240): a hover on a core method needs them, a
definition in a gem does not, and they land a fifth of a second later.

**Measured.** The child's commit of the named gems, 100k corpus, five
interleaved runs on a loaded machine (load 15–27): 1.32 s → 0.76 s median.
Over `trekr --lsp`, first definition into the installed gem (medians, five
interleaved rounds against the build before, every set under some load):
discourse 1.12 → 0.40 s at load 7–9, the hover with it 1.12 → 0.42; 100k
1.97 → 1.21 s and 2.55 → 0.64 s at load 15–21. No set was quiet; the
harness polls every 0.1 s, which bounds how fine any of these is. Stores
hash the same table by table, after an LSP-driven index and after
`--index`.

**The cost.** None in work: every gem is still read once, later for most.

## DEC-331 — Definition and references say once that the index is partial

**Decided.** The first time a definition, references or implementation is
asked in a checkout while its first index fills the store (DEC-320), the
language server sends one `window/showMessage` (info): trekr is still
indexing this checkout, N of M files read, and until it finishes go to
definition and references answer from what is read so far, and may miss or
change. Once per checkout for the life of the session, and only when one of
those is asked — the person who never navigates mid-index never sees it.
A cut-short index says so instead, as its hover does.

**Why.** A hover says it in its text and completion with `isIncomplete`
(DEC-320); a definition is a list of locations, and references are too, so
neither had a way to say it, and the progress bar was the only sign. It is
not one a person connects with "this jump may be wrong". In a traced run on
the 100k corpus, a definition in a file opened mid-index went to a gem's
`local?` for 17 s, with nothing said.

**Considered.**
- *`window/showMessage` per answer:* a toast per keypress.
- *`window/logMessage`:* the Output panel, which nobody reads while
  navigating.
- *A work-done progress on the request itself:* VS Code sends no
  `workDoneToken` for definition or references, and the index already
  reports `$/progress`.
- *A notification only trekr's extension understands:* an LSP-native message
  reaches every client that shows one; the extension needs nothing.

The precedent is DEC-056's: references already says it cut with one
`showMessage`. The same limit applies — a client that shows no
`showMessage` (Claude Code's LSP tool) sees nothing, and the CLI's `warming`
field is how an agent learns it.

## DEC-332 — A file opened during a first index's bulk write is written to an early store

**Decided.** While a first index's bulk write of the rest of the checkout
(DEC-322 step 4) holds the store, the files the language server opens, and
their neighbours, are written by that index to an *early store*: a copy of
the store as of its last commit, in a directory beside it named for the
index's pid (`trekr.db.early-<pid>/`), plus those files, each batch its own
commit. The language server, finding the early store of the index its
`warming` mark names, reads it in place of the store — writes still go to
the store — until the index removes it, which it does once the bulk write is
in, when the store holds everything the early store does. An early store
whose index is gone is ignored, and swept by the next first index.

**Why.** The bulk write is one transaction of up to 17 s at 100k (8 s
parsing, 5 s rebuilding indexes, 1.5 s committing), and nothing else can
commit to the store meanwhile. A file opened in that window waited for all
of it: 1–17 s at 100k. Committing the opened files around the bulk write
means splitting it, and a split gives up the bulk load — DEC-057 measured
1.5× for parts of 20k files, and the index rebuild alone is 5 s each time.
Answering from the editor's buffer instead (DEC-321's declined option)
covers the open file and not the files it names.

**The copy.** By file, after a checkpoint has copied every committed frame
into it, so on a copy-on-write filesystem (APFS, btrfs) it is a clone in
milliseconds whatever the store holds — `VACUUM INTO` rewrote every page,
seconds for a 450 MB store. Nothing else writes the file while no commit
lands: a checkpoint only copies committed frames, and the bulk write holds
the write lock. The one commit that can land is the bulk write's own, and a
copy taken across it — `PRAGMA data_version` moved — is discarded, as the
early store is no longer needed. A checkpoint held back by a reader is
retried for half a second, then the early store is given up and the file
waits as before.

**Measured.** `trekr --lsp`, a second file opened mid-index, five
interleaved rounds against the build before (medians, time from the open):
100k, opened 4 s in, at load 15–21 — definition of a constant it names
61.4 → 0.26 s, hover 61.4 → 0.32, references 60.4 → 0.30, a call to a
method it defines 60.3 → 0.30; discourse, opened 0.8 s in, at load 7–9 —
definition 1.57 → 0.31, hover 1.73 → 0.35, references 1.74 → 0.23. The
index writes an early store of 114 files 0.14 s after the open, copy
included. Final stores hash the same as without one: the store itself is
only read. The index child's CPU at 100k, one file open, five runs: 45.0 →
45.7 s (wall 22.7 → 21.9).

The writer polls for opened files every 20 ms while the bulk write runs,
and a poll with nothing asked returns at once: a first cut that guessed
neighbours for an empty ask rebuilt a name map of every path each time, and
cost 10 s of CPU at 100k.

**The cost.** On a filesystem without clones the copy is a real one, the
store's size in time and disk, once per first index that has a file opened
mid-write. An early store holds the other checkouts as of the copy too, so
for the seconds the server reads it, answers there are that old.

**Addendum — teardown.** A pre-release hunt on the 100k corpus found the
early store's end less tidy than its start:

- *Nothing reopens an early store into being.* The server opens one only as
  it is (`Store::open_existing`: no `CREATE`, no lock file, no rebuild or
  set-aside), and so does each second connection it opens from it. An open
  that landed between the index's unlink of the file and its `rmdir` used to
  lay down an empty, schema'd store and its lock there: the `rmdir` failed
  and the server read an empty store as whole. The index renames the
  directory aside (`….gone`) before emptying it, so an open by name from then
  on finds nothing; a reader with it already open may still touch its WAL,
  so the removal is retried, and a sweep takes what is left.
- *Its removal is not a replaced store.* The serve loop's DEC-300 check
  asked the store it reads — the early store, in early mode — so the index's
  own cleanup read as "the index was replaced underneath this server": a
  message, every tree dropped (5 s of rebuilding at 100k), a redundant
  second index and one of the three reopens spent, on every idle run. It
  asks the store itself now; `follow_early` notices the early store gone and
  keeps every tree answering until its successor is built aside.
- *An index that dies leaves no early store in use.* The server checked only
  that the early store's file was there, and nothing removes the file of an
  index killed mid-write: the session read that copy for its life — a class
  saved since was "not defined anywhere" — and a hover said "still indexing"
  because a tree's `partial` kept the `interrupted` it was built with. Now
  `follow_early` also asks whether the index's pid runs, and when it does
  not, reads the store again, sweeps the early store, and runs the index
  again, as the server does for a checkout found cut short at start
  (DEC-320); so does an index child of the server's own that exits with its
  checkout still marked. Once a checkout a session, so an index that dies
  every time is not run forever. `Warming::now` asks the pid at each read.
  An early store an index left is listed by `--status` as `kept` (`kind:
  early`) and removed by `--gc` at any age, as a set-aside copy is — the
  one-line follow-up DEC-300 left.
- *`--status` says a checkout is partial.* After a first index was killed, it
  showed the 100k checkout as "18 files" and nothing else, while `--def
  --json` said `warming.interrupted`. Its row now carries the same `warming`
  as an answer, and the text a line under it: "being indexed: N of M files
  read so far", or "cut short: … — `trekr --index <root>` finishes it".
- *Completion keeps the last tree's listing* (DEC-323). A save that changes
  the tree threw the listing away with it, and on a warm 100k session
  completion then answered nothing, `isIncomplete`, for 4–8 s, each request
  waiting `ASIDE` for the new listing. The old listing now answers, marked
  incomplete, at once, until the new one lands: it differs from the new one
  by an edit's worth. Measured with the hunt's `compsave.py` (100k, warm,
  save then complete every 0.1 s for 15 s), two runs each against the build
  before: empty answers 9 of 98 from 0.8 to 5.0 s → 0 of 133; each request
  in that window 0.41 s → 0.001 s, the first after the save (the tree's own
  rebuild) 0.81–0.83 → 0.35–0.36 s.

## DEC-333 — A warm request reads the store's roots only once the store has moved

**Decided.** The language server keeps each checkout's stamp (DEC-065) with
the store's `PRAGMA data_version` it was read at, and reads it again only
when that has moved — another connection committed: an index, a refresh by
another process — or when the server wrote through its own connection,
which `data_version` does not count, or moved to another store (DEC-300,
DEC-332).

**Why.** The comparison re-run found a warm definition on discourse taking
about 7 ms server-side where an August build took 1 ms, whether or not it
found anything. `sample` put about 70% of each request in
`Store::tree_roots`, reached from `Session::tree` → `Tree::stamp` on every
request — two queries, the second building a temporary B-tree for its `IN`,
and then a key per root for every gem — while resolving took 4%. The stamp
only moves when the store does, and SQLite already counts that.

**Measured.** A warm definition on discourse (`about.rb`, `StatsCacheable`),
an already-indexed store, 200 requests after 20 to warm up, client-side
median per run, five interleaved runs on a loaded machine: 24 ms → 0.51 ms
(runs 10.7–40.6 → 0.47–0.63); an earlier set, 17 → 0.53 ms. The same
answer either way.

**What it keeps.** Everything that moved the stamp still does: every
commit by another process moves `data_version`, and a refresh the server
writes itself clears what it keeps. DEC-323's partial trees are rebuilt as
before; the check that a finished index stops a tree calling itself partial
reads `meta` on its own.

## DEC-340 — A symbol an option names a method by is a reference

**Decided.** DEC-037 recorded a symbol in an argument's position as a
possible call of its name; a symbol that is an *option's value* was not
recorded at all. Now the value of an option that takes a method of `self`
is recorded as a positional symbol is — a `symbol` call site, standing for
`self`'s instance method where the call is a class-level macro (DEC-093), so
`--def` on it answers with the method, `--refs` lists it `possible`, and
`--dead` counts it toward `convention-only`. A value that is a list counts
each symbol in it (`if: [:a?, :b?]`). The options:

- `if:` and `unless:` — every callback, filter and validation;
- `with:` — `rescue_from`'s handler;
- `to:` — the method a delegation calls;
- `reject_if:` — `accepts_nested_attributes_for`'s, when not `:all_blank`;
- any key ending `_method` or `_method_name` — an app's own macro saying so
  (`rate_limit! :x, response_method_name: :render_throttled`).

**Only on a macro written on the class**: its body, an includer body
(`included do`), or a `with_options` block in either, whose macros are the
body's own with the options merged (`with_options unless: :signed_in? do
before_action :x, if: :y? end`). In a method an option's value is a value
handed to a call: rails' `@router.add_route Address.new, to: :first` names
a mailbox, and counting it, as first written, made it a possible caller of
every `first` (8 sites across the 40 `--refs` queries). The class-level
reading lost no row on mastodon or discourse once `with_options` was read
(without it, 2 mastodon rows in a `with_options` block went back to
`unreferenced`).

**Not** `only:`/`except:`, which name the actions a filter applies to and
call none of them, nor `on:`, an event. A list of keys, not every option:
a value such as `dependent: :destroy` or `inverse_of: :owner` is no method of
`self`, and recording it would make `--def` on it answer, confidently, with
whatever `self` has of that name. The key's name is the rule: an option
named for a method takes one.

**Not a regression.** The 0.8.2 report suggested DEC-312 had dropped these.
It had not: since 3edff7c an option's value was "visited on its own", and a
symbol visited alone records nothing. 0.8.0 answered the same.

**Why.** A 0.8.2 report from a large Rails monorepo: of 118 hand-checked
`unreferenced`, clear rows, 17 were named in their own file in ways
`--dead` did not count, most of them this — `rescue_from …, with:
:respond_rate_limited`, `validates …, if: :validate_limit?`, `delegate …,
to: :env_params`. The row's reason said "no call, symbol or `super` names
it", which was false.

**Measured**, `--dead app`, against main: mastodon 237 `unreferenced` →
167 (clear 150 → 82), all 73 rows that moved now `convention-only` — 70
from `unreferenced`, 3 from `override` — each named by `if:`, `unless:` or
`with:` (`current_user?`, `rate_limited_request?`, `internal_server_error`);
none named only by a value. discourse 1,093 → 1,078 (clear 746 → 735).

## DEC-341 — A call's keywords are the keywords of a method that takes them

**Decided.** A call site's argument count holds its keywords as one
argument (`refresh(name: "a")` is 1), which is what Ruby passes to a method
that takes no keywords: one positional Hash. A method that takes keywords
(`name: nil`, `height:`) takes them as keywords, so it now also fits a call
of one more argument than it requires. `**opts` already fitted any count.

**Why.** Exposed by DEC-342. An `after_save do` block's calls were read on
the class, so `theme.theme_modifier_set` there was untyped; once they ran on
the instance it typed, and discourse's
`theme.theme_modifier_set.refresh_theme_setting_modifiers(target_setting_name:
name, target_setting_value: value)` was excluded from
`ThemeModifierSet#refresh_theme_setting_modifiers`'s references as "the
argument count does not fit" — `def refresh_theme_setting_modifiers(
target_setting_name: nil, target_setting_value: nil)` counted as taking
none. The same held for every untyped call with keywords to such a method.

**Not done.** A braced Hash (`m({a: 1})`) is positional in Ruby 3 and is
still counted as fitting a method that takes keywords; the count does not
say which was written.

**Measured**, `--dead app` against the build before it on the same store:
mastodon one row leaves the candidates (`Notification::Groups::ClassMethods
#paginate_groups`, `unreferenced`); discourse 13 rows gain references — two
`unreferenced` and seven `single-caller` rows leave, three `convention-only`
and one `unreferenced` become `single-caller` — and none loses one.

## DEC-342 — A block or condition Rails runs on the instance calls the instance

**Decided.** Two shapes Rails `instance_exec`s on the instance, which trekr
read on the class:

- **A block handed to a class-level callback**, `validate` or
  `rescue_from`: `after_save do … end`, `before_action do … end`,
  `rescue_from Error do |e| … end`. A call on `self` in it runs on the
  instance (`resolve::made_side`, beside DEC-260's macros). A callback is
  known by its name — `before_`, `after_` or `around_` and more — since
  ActiveSupport::Callbacks builds the call at runtime and no body says so;
  that covers an app's own `define_model_callbacks` too. In a concern's
  `included do` it is the includers' instance.
- **A lambda that is an `if:` or `unless:` value**, alone or in a list,
  wherever the option is written — `before_action :x, if: -> { ready? }`,
  `options.merge(if: [-> { !token? }])`. Its calls on `self` are recorded on
  the instance side of the class `self` is where the lambda is written: the
  class in its body or a `def self.`, and in a concern's `ClassMethods` (or
  `class_methods do`) the concern, whose includers' class methods they are.
  Elsewhere — a module's instance method, an instance's own method — the
  call is recorded as before.

**Why.** The 0.8.2 report: `uses_app_token?`, called only as `if: [-> {
uses_app_token? }, *conds]` in a concern's class method, and a method
called only in `rescue_from … do |error|` inside `included do`, were each
`unreferenced`, clear. `--refs` excluded both calls as "no such method"
on the class side.

**Measured**, `--dead app`, against the build before it, each on a store it
indexed: mastodon 4 rows move, all toward use (two `single-caller` rows
leave, two `convention-only` become `single-caller`); discourse 19 —
`unreferenced` 1,075 → 1,067 (`Topic#ensure_topic_has_a_category`,
`ApplicationController#is_feed_request?`, …), every move toward use. rails'
40 `--refs` queries: 3 sites move, each a call in a callback block —
`update` in `after_create do` (comment.rb:103) possible-or-excluded →
confirmed, and two that now land on the model class's own methods. The
widget_shop trace: one site moves, residue → Active Record's
`EncryptableRecord`, `has_encrypted_attributes?` in a `validate …, if: ->`
lambda — the owner Ruby ran.

## DEC-343 — A symbol trekr does not read is said to be there

**Decided.** Two answers claimed what they had not checked:

- **`--dead`'s "no call, symbol or `super` names it".** A symbol in the
  candidate's own file whose name is the method's, and which no rule reads
  as a call of it (`only: [:archive]`, `on: :create`, `opts[:limit]`,
  `kind == :ordered`), was there and was not counted. Such a row now says
  "no call or `super` names it, nor a symbol trekr reads as a call", and
  its caveat names the symbol and line ("`:archive` at line 6 is not read
  as a call"), which grades it `lower`. The tier stays: the symbol is
  evidence of a name, not of a caller. Read from the file, every symbol
  literal (`extract::symbol_literals`) less those recorded as calls and
  those in the method's own body, which it uses as values.
- **`--def` on such a symbol** snapped to the nearest other name on the line
  and answered `resolved` for it, `snapped_to` its only disclosure: `--def
  …:55` on `on: :create` answered `load_widget`. A column on a symbol is a
  deliberate point at a name, unlike whitespace. It now answers `residue`,
  `under: symbol`, "a symbol no rule reads as a method's name here: a key or
  a value", as a variable (DEC-064) and an ownerless `super` already do.
  A hash's or keyword argument's key is such a symbol: testbed 087 had
  pointed at `id` in `where(id: featured_ids)` and was answered by a snap to
  `featured_ids`; its column now points at the name it meant.

**Why.** The 0.8.2 report: 17 of 118 hand-checked `unreferenced`, clear
rows were named in their own file in ways `--dead` did not count, under a
reason that said nothing named them. DEC-340 counts the option values that
name a method; this says the rest.

**Measured**, `--dead app` against the build before it on the same store:
`unreferenced` and clear, mastodon 82 → 77 and discourse 729 → 655. Most are
controller actions an `only:` names (discourse's `SessionController#csrf`,
`#sso_login`); DEC-344 reads the routes that reach them.

## DEC-344 — A route reaches the controller action it names

**Decided.** `--dead` reads the checkout's routes — every `config/routes.rb`
git knows, the app's and each engine's, and the files they `draw` — and a
controller's public instance method a route reaches is tiered
`convention-only`, "named only by a route, at config/routes.rb:N", with
`route: {path, line}` in JSON. It is evidence of a way in, as a symbol
handed to a macro is, not a call: a routed action with one written caller
is still `single-caller`. `cli::routes` reads, without running:

- a verb's target: `get 'x', to: 'c#a'`, `'x' => 'c#a'` (any path key, an
  interpolated one too), `controller:`/`action:`, `root 'c#a'`, a bare
  `get 'photos/search'` (`photos#search`), and in a controller's scope
  `get :preview` or `get 'preview'`;
- `resources`/`resource` and their default actions (a singular resource's
  controller is plural), less `only:`/`except:`, with `controller:` and
  `module:`, their blocks and `member`/`collection`, `concern` and
  `concerns`;
- the module `namespace` and `scope module:` nest a controller in, and
  `controller :x do`/`scope controller:`.

A route's controller is the class its path camelizes to, compared without
case or underscores (so an acronym inflection — `OAuth` for `oauth` — still
matches), an engine's (`Billing::Engine.routes.draw`) under its namespace
first. Its action is what that class's lookup of the name finds, so a route
to `admin/widgets#index` reaches `Admin::BaseController#index` when the
widgets controller inherits it.

**What it cannot read is said.** A route whose path or name is built at
runtime, or whose controller or action is a path segment (`':controller(/
:action)'`), is listed. Then every public action no read route reaches
carries "a public action routes may reach (a path built at runtime at
config/routes.rb:693)", graded `lower`, as does every one in a checkout with
no routes file (testbed 343's controller, which has none, is now a plain
`WidgetPanel`). An action no route reaches, in routes read whole, says "no
call, symbol, `super` or route names it". Not read: a gem's routes
(Devise's `devise_for`, Doorkeeper's `use_doorkeeper`), `mount`ed apps, and
`direct`/`resolve`.

**Why.** The 0.8.2 report: most of 69 controller-method candidates were
public actions `config/routes.rb` maps (`post 'self_serve_token/initiate',
to: 'self_serve_token#initiate'`), each `unreferenced`, clear.

**Why read rather than caveat every action.** The caveat alone was the
fallback. Reading the routes moves discourse's 293 unreferenced public
controller actions to 7 (286 `convention-only`, each naming its route);
the 7 left are lower, behind the one route discourse builds at runtime. A
blanket caveat would have left all 293 as candidates. mastodon 4 move;
its other actions were already named by `only:` symbols.

**Measured**, `--dead app` against the build before it on the same store:
discourse `unreferenced` 1,067 → 781, clear 655 → 569; mastodon 166 → 162,
clear 77 unchanged.

**Not done.** `--refs` on an action does not list its routes; `--def` on a
route's string does not answer the action.

**Addendum, before release.** A hunt of the unreleased rules found five
edges, each one hiding a dead action or misstating a live one:

- **`with_options`** hands its options (`only:`, `except:`, `concerns:`,
  `controller:`, `module:`, `to:`) to every route in its block, nested
  blocks too, as Rails' option merger does; a call's own option wins. Read
  as a plain block, `with_options only: [:index] do resources :gizmos end`
  routed every default action, and `GizmosController#edit` was "named only
  by a route" (testbed 380; mastodon `config/routes/admin.rb:196`). A
  `with_options` whose block takes a parameter (`|r| r.resources …`) is
  listed as unread: routes written on the parameter are not read.
- **A test app's routes are not the app's.** `spec/dummy` and `test/dummy`
  routes files are skipped, as DEC-363 skips tests (rails' Action Mailbox,
  Action Text and Active Storage; graphql) (382).
- **A singular resource's controller is its name as Active Support
  pluralizes it**, by a port of its English inflections: `resource
  :settings` is `SettingsController`, `:news` `NewsController`, `:person`
  `PeopleController`, where the naive rule spelled `settingses`. When that
  plural names no controller — an app's own inflection — the name as
  written is tried (381).
- **A concern's method is a controller's action.** A route reaches what the
  controller's lookup finds, which may be a module it includes; the row
  checked only owners named `…Controller`, so a routed concern method was
  `unreferenced`, clear. A public method of a module any controller
  includes is now weighed as an action (383).
- **A `concern` is the route set's, and its routes are where it is
  written.** Its block was re-parsed alone, so its routes reported the
  block's line within itself (mastodon's `Admin::*Controller#batch` at
  `admin.rb:1`, written at `:7`), and a file it `draw`s, read with its own
  concerns, could not use it (384).

**Measured**, `--dead app` and `--dead lib` against main (a71975a) on the
same store. Tier counts unchanged on mastodon app and discourse app.
mastodon: 16 rows' route citations change — 10 `batch`/`approve` lines
corrected (`admin.rb:1` → `:7`, `api.rb:2` → `:309`) and 6 `with_options …
concerns:` routes now read (`Admin::Trends::*#batch`,
`Api::V1::Admin::Trends::*#approve`, already `convention-only` by a
symbol); `RegistrationHelper#terms_agreement_label`, a helper a controller
includes, is weighed as an action. discourse `lib`: `ExternalUploadHelpers`,
a concern its upload controllers include, gains its 5 routes (2 rows
`unreferenced`, lower → `convention-only`), and
`SecureUploadEndpointHelpers#upload_from_full_url` goes clear → lower under
the runtime-route caveat — on reading it is dead: one true row lost.

## DEC-350 — The naming rung reads a split name as the variant its file reaches

**Decided.** When the naming rung (`from_receiver_name`) takes `@user` to
mean `User` and `User` is split (DEC-072), it checks its corroborations
against the variant nearest the call: that the class answers the call, and
that the enclosing scope does not. Every other rung's type is mapped to that
variant too, after it is typed (`receiver_of`); this rung's checks ran before
the mapping.

**Why.** discourse's benchmark script declares `User = Data.define(:id, …)`
beside `app/models/user.rb`, which splits `User`. A split name has no chain
of its own, so the lookup of `id` on it found nothing, the rung gave up, and
`@user.id` and `user.id` across the app were residue. The residue ranker
then put the script's `Data` reader first, because its owner is the class
the receiver is named after. The comparison re-run counted two of its 500
sites correct → wrong against an August build, which predates the split.
Every release since 0.2.1 has had this.

**Measured.** discourse's 500 comparison sites (`script/compare.py`, seed
12): correct@1 76.6 → 77.0 %, wrong@1 19.4 → 19.0 %, found 82.4 → 82.8 %;
both moves are the `id` sites, now `ambiguous` on
`AttributeMethods::PrimaryKey#id` as they were before the split.
discourse's gold set (`script/gold.py`, 9,047 app sites): correct 5,647 →
5,699, declaration 288 → 299, confidently wrong 125 unchanged. Of 66 moved
sites, 64 gain: `user.id`, `user.staff?`, `@user.silenced?` across the
guardians and services. Two lose: `user.email.present?`, where the chain
through an untyped `user` had guessed `email` a `String` and the typed
`User` gives `email`, a column, no type (correct → residue); and one
`admin?` residue-hit → ambiguous-wrong. The gem gold sets, widget_shop's
trace and its 63 comparison sites are unchanged.

## DEC-351 — Among equal residue candidates, a definition before a declaration

**Decided.** The residue ranker's last tiebreak, after tier, checkout-first
and directory affinity: a candidate whose code is at its site (a `def`)
before one that only declares the method (a macro, an RBS or `.rbi`
signature, the RSpec stub).

**Why.** ActiveRecord's `Querying` writes `delegate(*QUERYING_METHODS, to:
:all)`, which trekr reads as declaring every querying method (DEC-131). An
untyped `topic_users.update_all`, `scope.joins`, `posts.order` or
`users.find_each` offered that line first, ahead of the relation method it
sends to, which is a candidate too. A declaration says the code is
elsewhere, and often it is in another candidate. In a fabricator,
ActiveRecord's `define_model_callbacks` declaration of `before_create` came
ahead of Fabrication's own `def before_create`.

**Measured.** discourse's 500 comparison sites: correct@1 77.0 → 78.0 %,
found unchanged (82.8 %): `update_all`, `joins`, two `find_each` and the
fabricator's `before_create` move to the first location, and nothing moves
away from it. widget_shop's 63 are unchanged. discourse's gold set moves no
resolved verdict; the truth is offered at 15 more residue sites and 6 fewer
(app residue-hit 1,885 → 1,888, gem +6), the six being truths that are
themselves declarations (`where` through `Querying`'s delegate,
`primary_key`). In the gem gold sets the only verdicts that move are
`declaration-offered` → `residue-truth-absent`: 33 sites (polyid 15, accord
16, flipper 1, graph_weaver 1), and 65 more in discourse's. That verdict means
some declaration was offered and the truth is generated, and in every one of
these the declaration that left the list was unrelated to the truth
(`StringIO`'s RBS `string` for accord's DSL `string`,
`OpenSSL::OCSP::Response#create` for FactoryBot's). The truth was not
offered in either build.

**Not done: the stdlib after the gems.** `posts.order` still offers
`OptionParser#order` first. The stdlib's candidates come before the gems'
because the stdlib is indexed first, and ordering them after the gems
fixes those two discourse sites. At app call sites in every gold set a
gem's method is the truth far more often than the stdlib's (discourse
4,865 to 30, graph_weaver 18,094 to 541). But in a tie group longer than
the list, the move pushes real stdlib answers off it: `Rails.root.join`'s
`Pathname#join` on two of discourse's 500 sites (found −2), and
widget_shop's `Singleton#instance` and `ConditionVariable#signal` (2
`residue-hit` → `residue-truth-absent`). That is +2 correct@1 against −4
found, so it is held back.
## DEC-360 — A Haml template is read line by line, its comments skipped and its commas followed

**Decided.** DEC-315's reading of a Haml or Slim template changes three ways:

- **Each line's Ruby is read on its own.** The template's Ruby was read as
  one run of text, so a quote one line left open — an apostrophe in a `-#`
  comment ("on it's own `<style>` tag") — opened a string that ran on until
  the next quote, and every name in between was string text. A line's
  slip now ends with the line.
- **A `-#` line is a comment**, and is not read at all.
- **A Ruby line ending in a comma continues** on the next line, as Haml
  reads it: `= f.input :x,` then `hint: hint_text,` on the line under it.
  The continuation was not Ruby to the reader, so `hint_text` was not a
  name in a view.

**Why.** The 0.8.2 report's lane found `ThemeHelper#custom_stylesheet`
`unreferenced`, clear, with `= custom_stylesheet` in mastodon's
`app/views/layouts/application.html.haml:41`. Thirty lines above it, `-#
Needed for the wicg-inert polyfill. It needs to be on it's own <style>
tag` opened the string. A hand-check of all 77 of mastodon's `unreferenced`,
clear rows (`--dead app`) found 12 helpers a template calls, most on a
comma's continuation line (`label_method: ->(x) { privilege_label(x) }`,
`hint: discovery_hint_text,`).

**Measured**, `--dead app`, against the build before it: mastodon clear
2,359 → 2,340; of the 77 hand-checked rows, the 12 view callers and one
helper a view also calls become `lower`, and none of the 22 true candidates
moves. discourse unchanged (its templates are ERB).

**Addendum, before release.** A file with a template's extension that is
not UTF-8, or holds a NUL, is no template: a binary `.erb` under
`app/views` was read as one, and its random bytes "named in a view" any
method whose name they happened to spell.

## DEC-361 — `--dead` counts a module function's calls through its module

**Decided.** `module_function` makes one `def` two methods (extraction emits
both): a private instance method and a public singleton copy, `via:
module_function`. `--dead` weighs the instance one and skips the copy, as it
skips every macro-made method; the copy's calls (`Extractor.extract_urls(…)`)
now count toward it, as an alias's do (DEC-316). Both run the same body.

**Why.** mastodon's `Extractor#extract_entities_with_indices` and
`PrivateAddressCheck#private_address?` were `unreferenced`, clear, each
called only as `Module.method` (`app/lib/text_formatter.rb:30`,
`app/lib/request.rb:394`) — 2 of the 77 hand-checked rows.

**Measured**, `--dead app`, against the build before it: mastodon
`unreferenced` 162 → 160, and 3 more rows leave or move toward use
(`Extractor`'s other module functions, a helper's). discourse unchanged.

**Known gap, before release.** `--refs` excludes a call through the
module (`Extractor.extract_urls(…)`) that `--dead` counts here: the two
disagree on the singleton copy until `--refs` reads it too.

## DEC-362 — ActiveModel::Serializers calls `include_<attr>?` for the attributes a serializer declares

**Decided.** ActiveModel::Serializers 0.8 and 0.9 build `include_#{name}?`
for each attribute and association a serializer declares
(`define_include_method`) and send it to decide whether the key is
serialized; no call site writes the name. A method `include_<x>?` is
`convention-only`, "named only by a symbol ActiveModel::Serializers calls it
for, at FILE:LINE", with `convention: {by, path, line}` in JSON, when:

- the tree's `ActiveModel::Serializer` has a class method
  `define_include_method` — the indexed gem has the convention, and 0.10,
  which takes `if:` instead (DEC-340 reads that), does not;
- its owner inherits `ActiveModel::Serializer`, or is a module a class that
  does mixes in (a mixin's hook runs on the serializer);
- the symbol `:x` is written in that serializer's file or an ancestor's —
  the mixin's own, a superclass's (`attributes :cooked` in
  `PostSerializer`, `include_cooked?` in `SearchPostSerializer`).

An `include_x?` no declaration names stays `unreferenced`: AMS never calls
it. discourse has such rows — `ApiKeySerializer#include_user_id?`
(its association is `:user`), `CurrentUserSerializer#include_can_localize_content?`
(its attribute is `:can_localize_content?`, whose hook is `…??`).

**Why.** 361 of discourse's 569 `unreferenced`, clear rows were this
convention (the last lane's count); in a hand-checked random 70 of them, 39.

**Not** the symbol's macro: any `:x` in those files counts, which is a
symbol of the name, not proof that `attributes` took it. A file that writes
`:x` for another reason gives a hook of that name a way in that AMS may not
use. The 9 rows left `unreferenced` were each read and none is declared.

**Measured**, `--dead app`, against the build before it: discourse
`unreferenced` 781 → 314 (clear 569 → 210); 481 rows become
`convention-only`, 359 of them from `unreferenced`, clear, 14 from
`override`. Of the 39 sampled rows, 38 move and the 39th is
`include_user_id?`, which on reading is dead. mastodon unchanged (no AMS
0.8).

**Known gap, before release.** Any `:x` in the serializer's or an
ancestor's file counts, not only one `attributes` or an association takes
(see "Not" above); a pre-release hunt confirmed it as accepted.

## DEC-363 — `--dead` says where the checkout builds a method's name at runtime

**Decided.** `--dead` reads the checkout's Ruby (`git ls-files *.rb
*.rake`, less `spec/`, `test/` and `db/`) as text, as it reads the views
(DEC-315), for two shapes of a name no call site writes (`cli::built`):

- **An interpolated symbol** whose written part starts the name with three
  name characters or more: `:"report_#{type}"` is the shape `report_*`.
  A candidate whose name has the shape says "a name of its shape is built
  at runtime (`report_*` at app/models/report.rb:382)".
- **A computed name sent to a constant**: `UserNotifications.public_send(type,
  user)` (`send`, `__send__` too), whose first argument is no literal. A
  candidate of that constant, either side, says "a name computed at runtime
  is sent to UserNotifications at app/jobs/regular/user_email.rb:240".

Each is a caveat, graded `lower`, and the tier stays: a shape is evidence a
name may be built, not that this one is.

**Why.** Of a random 70 of discourse's `unreferenced`, clear rows
(hand-checked, `--dead app`), 22 were reached this way once AMS's hooks
(DEC-362) are set aside: reports (`report_*`, sent by `Report.find`),
reviewable actions (`perform_*`), service steps (`model :x` builds
`:"fetch_#{name}"`), and mailer actions a job sends a type string to. The
last lane had proposed special-casing discourse's service `model` step;
this reads the shape the app itself writes, so it is no one app's rule.

**Not a shape**: one that starts with its interpolation. `:"#{field}_count"`
and `:"#{period}_score"` are an attribute's derived name far more often
than a method's; counted, `*_score` from a migration and `*_count` from a
serializer caveated two of the hand-checked true candidates
(`spam_silence_score`, `update_distinct_badge_count`) and no false one.
Nor a string, which is a key or a message more often than a name, except as
the computed argument of a send. Nor a name built in a spec, a test or a
migration.

**Over-reach, measured.** A shape caveats every method of its shape in the
checkout, not the class that sends it: discourse's
`ExportUserArchive` builds `:"include_#{name}?"` for its own components, and
that lowers every serializer's `include_*?` too — which DEC-362 already
calls convention-only — and one hand-checked true candidate
(`ApiKeySerializer#include_user_id?`).

**Measured**, `--dead app`, against the build before it: discourse
`unreferenced`, clear 210 → 56; of the hand-checked rows still clear before
it, 42 of 47 false ones become `lower` and 1 of 15 true ones. mastodon
`unreferenced`, clear unchanged (63); 11 other rows gain the caveat. The
read's cost is inside the noise of `--dead app` on discourse (three
interleaved runs each way, 10–20 s, on a loaded machine).

**Addendum, before release.** A comment line is no longer read for a
shape, a send or a listing: discourse's `fetch_*` was cited at a comment
(`lib/service/base.rb:6`) and is now cited at the code that builds it; no
row's tier or confidence moved. The read is parallel across files, merged
in path order (discourse `--dead app` 3.0 → 2.7 s, three interleaved runs
each). **Known gap:** a shape still lowers every method of its shape in the
checkout, not the builder's class — the pre-release hunt counted 161 of
discourse's 305 `unreferenced` app rows lowered by one, and 221 of rails'
`visit_*` rows. Scoping a shape to the class that builds it is open.

## DEC-364 — A public writer of a class that assigns attributes is reached by name

**Decided.** Active Model's `assign_attributes` — and through it `new`,
`update`, `assign_attributes` and every form or API handed params — calls
`public_send("#{key}=", value)` for each key. A public `x=` whose owner's
chain includes `ActiveModel::AttributeAssignment` (every Active Record model
and every `ActiveModel::Model`), or a module such a class mixes in, says "a
writer Active Model's `assign_attributes` calls by name", graded `lower`.
The tier stays: whether a key `x` is ever handed in is not read.

**Why.** 10 of mastodon's 77 hand-checked `unreferenced`, clear rows, and 2
of discourse's 100, were such writers: `NotificationPolicy#filter_bots=`
(`params.permit(…, :filter_bots)`), `Form::Import#mode=` (a form's
`params.expect(form_import: [:mode])`), `User::HasSettings#settings_attributes=`
(`update!(settings_attributes: …)`), a column's writer a model overrides
(`AccountConversation#participant_account_ids=`).

**Not `override`.** A writer that overrides a column's generated writer
overrides a method no file defines; the schema is read for skipping
columns, not as methods to override. A caveat is the evidence there is.

**Measured**, `--dead app`, against the build before it: the 12
hand-checked writers become `lower`, no true candidate moves; `unreferenced`,
clear mastodon 63 → 53, discourse 56 → 51. 19 and 13 rows carry the
caveat, across tiers.

## DEC-365 — An unreferenced method under an ancestor trekr has not indexed says so

**Decided.** An `unreferenced` row gains a caveat, graded `lower`, when:

- **its owner's chain has an ancestor the tree cannot resolve** and does not
  know by that name — "an ancestor trekr has not indexed
  (Devise::RegistrationsController) may call it". A gem's `app/` is not
  read (DEC-017), so an engine's controllers are such ancestors, and a hook
  the engine calls (`after_sign_up_path_for`) has no caller in the index;
- **its body calls `super`** and no overridden method was found — "it calls
  `super`, so it overrides a method trekr has not indexed". A method that
  calls `super` has something above it by definition.

**Why.** 12 of mastodon's 77 hand-checked `unreferenced`, clear rows were
Devise and Doorkeeper controller hooks (`after_update_path_for`,
`build_resource`, `can_authorize_response?`), and one an Active Record
class method a concern's `ClassMethods` overrides with `super`
(`Status::SafeReblogInsert::ClassMethods#_insert_record`, which DEC-121's
`override` misses for a module the concern extends).

**Not a known name.** discourse's `User` has a second declaration, `User =
Data.define(…)` in a benchmark script, and its chain then lists
`ActiveRecord::Base` and `Data` as unresolved; both are indexed, and the
first cut caveated 3 `User` rows (one hand-checked true candidate) with
them. A name the tree knows is not an unseen ancestor.

**Not done.** Reading an engine gem's `app/` would make these `override`,
naming the method — DEC-017's reverses-if. It changes what a gem's index
holds and when it is read (the first index's parts, DEC-322), so it is left
for a change of its own. Doorkeeper's `Helpers`, which its engine `include`s
from an `on_load` in an `initializer` block (DEC-098 reads only a hook
registered as the file loads), is not caught: 2 mastodon rows.

**Measured**, `--dead app`, against the build before it: mastodon
`unreferenced`, clear 53 → 40, the 12 engine hooks and the `super` row
among the hand-checked; no true candidate moves. discourse unchanged in
clear; 3 writers that call `super` gain the second caveat.

## DEC-366 — A namespace's `table_name_prefix` is a hook Active Record calls

**Decided.** DEC-315's protocol hooks gain three on the class side, each a
namespace module's method its models' framework asks by name:
`table_name_prefix` and `table_name_suffix` (Active Record's
`full_table_name_prefix` takes the first enclosing module that answers
them) and `use_relative_model_naming?` (Active Model's `Naming`, for the
models a namespace holds).

**Why.** 5 of mastodon's 77 hand-checked `unreferenced`, clear rows were
`Admin.table_name_prefix`, `Trends.…`, `Web.…`, `Fasp.…` and
`AnnualReport.…`, which Rails' own generator writes for a namespaced model.

**Measured**, `--dead app`, against the build before it: those 5 become
`lower`; mastodon `unreferenced`, clear 40 → 35; discourse unchanged.

## DEC-367 — An unreferenced instance method whose name a bundled gem calls says so

**Decided.** An `unreferenced` instance method whose name a gem of the
checkout's bundle writes as a call (`Store::bundle_calls`: the `call_name`
posting lists of the gems `gem_use` records for the checkout, less symbols)
says "a gem in the bundle calls a method of this name (simple_form-5.4.1,
N sites)", naming the gem that writes the most, and is graded `lower`.
Only an instance's method: a gem's call on an object it was handed is a
call on a value, and is no evidence for a class method of the same name.

**Why.** mastodon's `UserSettings::Glue#type_for_attribute` (simple_form's
form builder calls `@object.type_for_attribute`), `PerOperationWithDeadline
#reset_counter` (http's `@socket.reset_counter if @socket.respond_to?`),
and Api::BaseController's two `doorkeeper_*_render_options`, which
Doorkeeper's helpers call on the controller they are mixed into (DEC-365's
open item) — 4 of the hand-checked `unreferenced`, clear rows, each a hook
a gem calls on a duck.

**DEC-074 stands.** Its rule is that the *tier* is weighed against the
checkout, so what else is indexed cannot change it, and its reverses-if
asked for a named rule over a store-wide name count. This is neither: the
tier stays, the gems are the checkout's own bundle as its index recorded
them, and the row says which gem. Two `self.readonly?` class methods, true
candidates whose name Active Record calls on instances, are why the side
is checked: counted, they would have been caveated too.

**Measured**, `--dead app`, against the build before it: mastodon
`unreferenced`, clear 35 → 31, the 4 above; no true candidate moves; 19
rows carry the caveat. discourse 51 → 50 (`Flag#applies_to?`, a name
rspec-mocks calls).

## DEC-368 — A helper called with no receiver from another helper says so

**Decided.** Rails mixes every module in `app/helpers` into one view
context (`include_all_helpers`, on by default), so one helper calls
another's method with no receiver and no file writes the include. That call
runs on `Object` for trekr (DEC-314), finds nothing, and is ruled out. An
`unreferenced` instance method of a module under a `helpers` directory, for
which such a ruled-out call is written in `app/helpers`, says "called with
no receiver in app/helpers/x.rb:N, a helper Rails mixes into the same
views", graded `lower`. The site stays ruled out: whether the two modules
share a view is not read.

**Why.** mastodon's `StatusesHelper#prefers_autoplay?` (called from
`ApplicationHelper`), `LanguagesHelper#available_locale_or_nil` (from
`FormattingHelper`) and `AuthorizedFetchHelper#authorized_fetch_overridden?`
were 3 of its 77 hand-checked `unreferenced`, clear rows.

**Measured**, `--dead app`, against the build before it: mastodon
`unreferenced`, clear 31 → 29 (the third already had a view caveat); 5
rows carry the caveat; discourse unchanged. It reads one more pass of a
helper candidate's call sites, which only `unreferenced` helpers pay.

## DEC-369 — `--dead` counts a mailer's class-side call as its action's

**Decided.** Action Mailer runs an action through its class:
`InviteMailer.send_password_instructions(user)` reaches
`InviteMailer#send_password_instructions` by the class's private
`method_missing`, which returns a `MessageDelivery` for any name in
`action_methods`. That `method_missing` hands the name to no object, so
DEC-261 does not read it as a forwarder, and the call is ruled out
`no_such_method`. For a public instance method of a class inheriting
`ActionMailer::Base`, `--dead` now counts the class-side calls of its name
that were ruled out that way, on that class or a subclass, as confirmed
calls of the action. `--refs` is unchanged.

**Why.** discourse's `InviteMailer#send_password_instructions`, called only
as `InviteMailer.send_password_instructions(user)` in a job, was
`unreferenced`, clear — 1 of the 100 hand-checked rows; reading the rest
of discourse's mailers found 19 more.

**Measured**, `--dead app`, against the build before it: discourse 20
mailer actions move toward use — 14 leave the candidates, 6 become
`single-caller` (3 from `unreferenced`) — and none away. mastodon
unchanged: its mailers are called through `.with(…)`, whose result is not
typed.

## DEC-370 — A module whose methods are listed at runtime says so

**Decided.** DEC-363's read of the checkout's Ruby also finds
`Const.instance_methods` and `Const.public_instance_methods`: a caller
listing a module's methods to call or hand out by name. A candidate of that
constant, either side, says "its module's methods are listed at runtime
(Helpers.instance_methods at app/services/x.rb:212)", graded `lower`.

**Why.** discourse's `ThemeSettingsMigrationsRunner::Helpers` methods
(`is_valid_url`, `get_category_id_by_slug`) are attached to a JavaScript
context by `Helpers.instance_methods.each { … Helpers.method(name) }`, so
nothing calls them by name: 2 of the 100 hand-checked `unreferenced`, clear
rows.

**Measured**, `--dead app`, against the build before it: discourse
`unreferenced`, clear 45 → 42; 20 rows carry the caveat (17 of them
`PostRevisionSerializer`'s, whose method list `lib/post_revisor.rb`
checks for a `#{field}_changes` to call). mastodon unchanged.

## DEC-371 — Thor runs a command, and a generator's step, by its name

**Decided.** A public instance method of a class inheriting `Thor` is a
command Thor runs by its name (`desc "prune"`, then `bin/cli prune`); one
of a class inheriting `Thor::Group` — every Rails generator — is a step
Thor runs in turn. So is one of a module such a class mixes in (mastodon's
CLI commands live in concerns `included` into its Thor classes). Such a
method is `convention-only`, "a Thor command, which Thor runs by its name",
with `convention: {by: "Thor"}` in JSON. The rule keys on the tree knowing
the class reaches `Thor`, so an app with no Thor indexed claims nothing.
A private method is no command and is unchanged.

**Why.** Found on held-out data. The app-scope rules above were fitted to
hand-checked rows of `--dead app`; checked against `--dead lib` instead,
mastodon's 21 `unreferenced`, clear rows were 4 true, and 11 of the 17
false ones were its CLI's commands and generators (`Mastodon::CLI::Accounts
#prune`, `PostDeploymentMigrationGenerator#create_post_deployment_migration`);
discourse's random 25 of 91 held one generator.

**Measured**, `--dead lib`, against the build before it: mastodon 12 rows
become `convention-only`, `unreferenced`, clear 16 → 7; discourse 1. `--dead
app` unchanged on both. The mastodon `lib` numbers after this rule are no
longer held out.

**Addendum, before release.** Thor makes a command in `method_added`,
which fires for a method its class defines: `no_commands do`/`no_tasks
do` turn it off, and a plain module's method a Thor class includes is
defined on the module, so Thor never sees it — and its `DynamicCommand`
refuses to run a name the instance already responds to. So:

- a `def` under `no_commands` or `no_tasks` is no command;
- a module's public method is one only when written in its `included do`,
  which defines it on the including class, and that class is a Thor's.
  mastodon's CLI concerns are this shape (`Federation#self_destruct`).
  "Any includer is a Thor" was the rule; "all includers are" would have
  been as wrong, since what decides is where the `def` lands.

The hunt's two probes — a helper under `no_commands`, and a module included
by a Thor class and a plain one — were each "a Thor command" (testbed 385;
371's module moved into `included do`). Measured, `--dead lib` against
main: mastodon and discourse unchanged.

## DEC-372 — `clear` on an unreferenced row is calibrated against hand-checked rows

**Decided.** `clear` stays a word, not a number, and keeps its rule: no
caveat. What changed is what a caveat can see. DEC-360–371 each name one
way a method is reached that no call site writes, and each was found as a
cause of a hand-checked false `unreferenced`, clear row. `clear` now means
none of them was found, and this is what that measured.

**The samples.** `--dead app` on main (ff0ac62): mastodon's 77
`unreferenced`, clear rows, all of them; discourse's 569, a random 70 and
30 more that are not `include_*?` (100). Each was read, its name searched
across the repo and its installed gems, and its cause written down:

| cause of a false row | mastodon (77) | discourse (100) | rule |
| --- | ---: | ---: | --- |
| truly dead | 22 | 15 | — |
| AMS `include_<attr>?` | — | 38 | DEC-362 |
| name built at runtime / sent to its class | — | 26 | DEC-363 |
| service step `fetch_*` (`:"fetch_#{name}"`) | — | 16 | DEC-363 |
| view template caller | 12 | — | DEC-360 |
| gem engine's `app/` ancestor (Devise, Doorkeeper) | 12 | — | DEC-365 |
| writer reached by `assign_attributes` | 10 | 2 | DEC-364 |
| namespace `table_name_prefix` | 5 | — | DEC-366 |
| gem calls it on a duck / via `on_load` helpers | 4 | — | DEC-367 |
| gem not installed (devise_pam) | 4 | — | none |
| helper called by another helper | 3 | — | DEC-368 |
| `module_function` | 2 | — | DEC-361 |
| methods listed at runtime | — | 2 | DEC-370 |
| mailer action called on its class | — | 1 | DEC-369 |
| concern `ClassMethods` overriding with `super` | 1 | — | DEC-365 |
| scope lambda in a concern calling `ClassMethods` | 1 | — | none |
| name in an XPath string | 1 | — | none |

**Precision of `unreferenced`, clear**, main → this build:

| sample | before | after | true rows lost to `lower` |
| --- | ---: | ---: | ---: |
| mastodon app, all 77 (fitted) | 22/77 = 29 % | 22/28 = 79 % | 0 of 22 |
| discourse app, random 70 (fitted) | 8/70 = 11 % | 7/7 | 1 of 8 |
| discourse app, all 100 (fitted) | 15/100 | 14/14 | 1 of 15 |
| discourse app, 28 clear rows never sampled (held out) | — | 25/28 = 89 % | — |
| discourse `lib`, random 25 (held out) | 11/25 = 44 % | 11/18 = 61 % | 0 of 11 |
| mastodon `lib`, all 21 (held out) | 4/21 = 19 % | 4/16 = 25 % | 0 of 4 |

The one true row lost is `ApiKeySerializer#include_user_id?`, lowered by
DEC-363's `include_*?` shape that another class builds. Counts,
`unreferenced`, clear → lower: mastodon app 77 → 29 (lower 85 → 131),
discourse app 569 → 42 (212 → 263).

**What the held-out rows say.** On app code the rules carry over (89 %).
On `lib/` they do not reach 80 %: a CLI and a library are reached by other
conventions. mastodon's `lib` misses were 11 Thor commands, which DEC-371
then read (4/7 after it, no longer held out), and both corpora's remaining
`lib` misses are strings sent by name (`public_send("list_#{filter}")`,
`validate_method = "validate_#{name}"`, then `send`), a guardian's
`ensure_can_x!` magic, a `%i[…].each { define_method … public_send }`
forwarder, a callback object's `around_create`, a RuboCop or haml-lint
visitor hook, a method named in a YAML setting. Those are follow-ups, each
a rule of its own, and `clear` on a CLI or a library should be read with
them in mind.

**Not a graded number.** A per-row probability would have to be fitted on
these ~200 labels, and the features that predicted were the causes above,
each now a named caveat; the ones the brief suggested did not predict.
Whether the name appears anywhere at all — any file, any gem, any string —
split the fitted rows 22 true of 104 (no mention) against 15 of 73
(mentioned): no signal. Visibility did (0 of 30 private or protected rows
were dead), but every one of them is now caught by its cause (service
steps, engine hooks), and a blanket "private is reached" rule would lower
the private dead method a reader most wants found. A score fitted to two
apps' accidents would be a guess wearing a measurement's clothes; a named
caveat says what it saw.

**Addendum, before release: a known gap.** `--dead` weighs files added
since the last `--index` against an index that has not read them, so their
calls are not counted (pre-existing). The other gaps the pre-release hunt
logged are under DEC-361, DEC-362, DEC-363 and DEC-344's "Not done".

## DEC-380 — A call ruled out on a module nothing includes says its `self` is unknown

**Decided.** An `unreferenced` instance method for which a call of its name
with no receiver (or on `self`) is ruled out "no such method" on a module
trekr knows no includer of, written where `self` is that module's instance,
says "called with no receiver at FILE:LINE, in a module nothing indexed
includes, so its `self` is not known", graded `lower`. Such a row's reason,
and a DEC-368 helper row's, is now "no call trekr can place on it, nor a
symbol or `super`, names it": a call by its name is written. The site stays
ruled out in `--refs`.

**Why.** The user's original pattern: `Gated.options` returns
`kwargs.merge(if: [-> { feature_on? }])`, used as `validates :name,
**Gated.options(…)` in `Gadget`. DEC-342 records the lambda's call on the
instance side of `self` where it is written — the module `Gated` — which
has no `feature_on?` and no includer, so `Gadget#feature_on?` was
`unreferenced`, clear, "no call, symbol or `super` names it", a false
reason (testbed 386).

**The ideal not taken.** Counting the call as `possible` for every method of
the name would reverse DEC-314 (a module's own call that no includer answers
runs on an Object) for every includer-less module, in `--def` and `--refs`
too — a resolve change with gold-set reach, not a fix-lane one. This is the
honest minimum: the reason is true and the row is not `clear`. No
extraction change.

**Measured**, `--dead app`/`lib` against main, same store: tier and clear
counts unchanged on both corpora. Rows gaining the caveat:
mastodon `RateLimitable#rate_limiter`, called in an `after_create do` inside
a `class_methods do` method, which DEC-342 reads on
`RateLimitable::ClassMethods` rather than its includers' instances (a live
method; DEC-342's gap); discourse `Guardian#is_ignoring_user?` and
`#is_muting_user?`, called from `Chat::GuardianExtensions`, which a plugin
prepends to `Guardian` at runtime (both live). Five mastodon helper rows
change only their reason.

## DEC-390 — A callback block in a concern's class method runs on its includers' instances

**Decided.** A block handed to a callback, `validate` or `rescue_from`
(DEC-342's macros) inside a method of a concern's `ClassMethods` — the
module, or `class_methods do` — runs on an instance of whatever includes the
concern: the method runs on the includer's class, and Rails `instance_exec`s
the block on its instances. A call on `self` there is read on the concern's
instance side (`resolve::Made::IncludersInstance`), so the concern's own
methods answer it and its includers' answer the rest, as in `included do`.

**Why.** The 0.8.3 report: `rescue_from ActiveRecord::RecordNotUnique do
|error| … respond_duplicate_error(error, …) end` inside
`ClassMethods#rescue_duplicates` was read on `ClassMethods`, ruled out "no
such method", and `--dead` called `respond_duplicate_error` unreferenced.
DEC-342 placed such a block only when the macro is written in a class body,
`included do` or a `def self.`; DEC-380 named the gap (mastodon's
`RateLimitable#rate_limiter`, called in `after_create do` in a
`class_methods do` method). Testbed 390.

**Resolve only**; no extraction change. Measured with DEC-391 and DEC-392
(their entry has the table): mastodon's `RateLimitable#rate_limiter` leaves
the candidates; no rails `--refs` site moved by it.

## DEC-391 — A call in a block trekr cannot place is never ruled out as "no such method"

**Decided.** In `--refs` (and so `--dead`), a call on `self` that the
written scope does not answer is `possible` — "the call is in a block whose
`self` the method it is handed to may change" — instead of excluded
`no_such_method`, when it sits in a block whose `self` trekr cannot vouch
for (`resolve::self_unsettled`). A block is vouched for when every block
around the call, out to the method body or class body, is handed to:

- a method of Ruby's own (core or the stdlib) that is not one of the ways
  to change `self` — `tap`, `each`, `map`, `loop`, `Dir.chdir` — found
  through the call's receiver, or, for an untyped receiver, a name Ruby
  defines (`items.each`);
- a concern's `included`/`prepended`, read as the includer's body;

or when a rule already places it: DEC-342/390's callbacks, DEC-260's
macros that make a method of it. Everything else — a gem's DSL
(`draw do`, `scope … Proc.new { }`, `default_scope { }`), the checkout's
own macro, `instance_eval`, `Class.new { }`, `define_method` — may run it on
another object, and nothing trekr reads says whether it does. `--def` is
unchanged; a different-owner exclusion is unchanged, since the written
scope answering the name is evidence the block keeps it.

**Why.** The 0.8.3 report's safety net: an excluded call makes a live
method look deletable, and excluding it on a guess about `self` is the
dangerous direction. Testbed 391.

**The cost, named.** trekr does not read whether a checkout macro yields or
`class_exec`s its block, so testbed 260's two calls in such blocks
(`configure do`, `later do`), which were ruled out correctly, are now
`possible`. Reading it is a per-method fact ("runs its block as it stands")
that would need a stored column; not done here.

**Measured** with DEC-390 and DEC-392 — see DEC-392. rails' 40 `--refs`
queries: 90 sites excluded → possible by this rule and none in the other
direction, 46 of them `resources` in a routes `draw do` (live:
`Mapper#instance_exec`s it), the rest `where` in `scope`/`default_scope`
procs and statement-cache blocks (live), and `validate`/`execute` in
`Class.new(…) do` bodies.

## DEC-392 — A reader returns what its one expression makes or holds

**Decided.** A method with no parameters and no `sig`, whose body is one
expression, returns:

- `X.new(…)` — an `X`;
- `@x ||= …` or `@x`, and an `attr_reader :x` — an `X` when every write of
  `@x` in the file, in the same class and on the same side, is `X.new(…)`
  (`@x = X.new`, `@x ||= X.new`). A `nil` write is the variable not yet set
  and is skipped. Writes that disagree, a write of anything else, or an
  `attr_writer`/`attr_accessor` or `def x=` that lets any caller set it,
  leave it untyped.

It is recorded at extraction as the method's return
(`Def::sig_returns`, as DEC-133 does for a custom `new`), so it reaches
every rung that reads one: `chain` (`url_builder.verify_uri`), `sig`
(`b = url_builder`), a `delegate … to:` target (DEC-166) and `chain:name`.
On `self`, a subclass that overrides the reader returning something else
(or nothing trekr knows) leaves the step untyped
(`resolve::overridden_apart`); a subclass of the returned class is fine.

`@x ||= X.new` is now an assignment the ivar rung reads, as `x ||= X.new`
already was. `Class.new(Base)`, `Module.new` and `Struct.new` make a class
rather than an instance of `Class`, so they type nothing — they had typed
a local as a `Class` instance and ruled out the class methods it was sent.

**Why.** The 0.8.3 report: `url_builder.signup_verify_uri(…)` with
`def url_builder; @url_builder ||= Oauth::UrlBuilder.new; end` was
`possible`, receiver untyped; the same for `attr_reader`s set in
`initialize` and zero-argument service accessors. The ivar itself was
typed when read directly; the reader was the gap. Testbed 392.

**Bounds.** File-local, as the ivar rung is: a write in another file of
the class (a reopening, a subclass's `initialize`) is not seen. Only
`X.new`: `@x ||= X.build(…)` needs the callee's return, a resolve-time
question; not done.

**Store.** An extraction change: store version 53 → 54 on this branch
(the lead assigns the number at merge); every store reindexes once.

**Measured** (DEC-390–392 together), against main 5b712fb, each build on
stores it indexed itself:

| | main | this |
| --- | ---: | ---: |
| rails 40 `--refs`: excluded → possible, block `self` (DEC-391) | | 90 |
| … excluded → possible, `Class.new` local no longer a `Class` | | 89 |
| … possible → excluded, different owner (typed reader) | | 38 |
| … possible → confirmed | | 31 |
| … confirmed → possible (`chain:name` vote now disagrees) | | 6 |
| … excluded → possible, `chain:name` guess | | 3 |
| `--dead` rails 5 libs, unreferenced (candidates) | 553 (3,146) | 542 (3,124) |
| `--dead` discourse `app lib`, unreferenced (candidates) | 526 (5,545) | 524 (5,545) |
| `--dead` mastodon `app lib`, unreferenced (candidates) | 171 (3,544) | 170 (3,548) |
| gold, confidently `wrong`, every set | | unchanged |
| widget_shop trace (3,152), correct | 61 | 67 |
| discourse gold (900), correct | 559 | 561 |
| gem gold sets (4 × 900) | | correct +3, residue → right owner +9, one residue → `ambiguous-wrong` (`chain:name`) |

Spot-checked, every newly confirmed rails site (31: `mail.parts.first`,
`email.parts.size`, `content.attachments.first`, `@case_insensitive_cache
.fetch`) and the different-owner exclusions sampled
(`scanner.string.inspect` is String's, `context.find` is
`LookupContext#find`, `initializers.find` the collection's): each right.
The six confirmed → possible were `chain:name` guesses of `Array` for
`children`, `to_a` and `errors` — `children` is Nokogiri's, a `NodeSet` —
which a reader somewhere returning another class now disputes. mastodon's `Trends::Base#request_review`
became `unreferenced`: each `Trends.links` reader now types its subclass,
whose override answers, and the base's raises `NotImplementedError`.

## DEC-400 — An index that cannot finish says how far it got, and never exits 0

**Decided.** `--index` that stops part-way reports the checkout as the
store now holds it — "index incomplete: N of M files of ROOT read — why;
answers from it are partial until: trekr --index ROOT" on stderr, and under
`--json` `{repo, status: "incomplete", reason, hint, warming}`, `warming`
being DEC-320's object with `interrupted: true` (null when no mark stands:
a reindex's earlier map, or nothing written yet). Two ways to stop:

- **The lock outwaited** — DEC-139's ten minutes ran out behind another
  writer: exit `2`, "no answer yet, ask again". The index is what a query
  then answers `warming` from, with exit `2` too, and the remedy is the
  same: run it again. Not `74`: nothing is wrong with the store, and a
  caller that treats `74` as "the database is broken" would do the wrong
  thing about a lock that will be released.
- **A signal** — SIGINT, SIGTERM, SIGHUP: the report, then the process dies
  of that signal (130 for Ctrl-C), as the default action would have. A
  shell, `timeout(1)` and an agent's harness read a death by signal
  correctly; exiting `2` would claim the index chose to stop. The signals
  are blocked before any thread starts and taken by one thread with
  `sigwait`, so the report runs as ordinary code, not in a handler.
  SIGKILL cannot be caught; the next `--status` or query says the index
  was cut short, as before.

The language server's background index ends its progress "index cut short
at N of M files — reading the rest" (it resumes once, DEC-320), or "…
answers are partial until: trekr --index ROOT", where it said "index
failed".

**Reported** on a ~109k-file monorepo: `trekr --index .` behind a freshly
spawned `--lsp` first-indexing a second checkout printed "waiting for
another trekr writer… (60s)" and ended with no summary; `--status` said
"cut short: 180 of 144977 files read". Reproduced on mastodon (lock held
from the first commit, then SIGINT): the mark reads 179 of 17,322 — the
Ruby's stdlib, the commit before the checkout's own write, exactly where
the report's 180 stands.

**Why it stopped at all.** Not trekr's wait: no path gives up before
DEC-139's ten minutes, and when one does it fails loudly (`74`, "database
is locked"). Off a terminal DEC-171's notices come at 1 s, 60 s, 120 s;
the last one seen being "(60s)" puts the end between 60 and 120 s — where
a caller's two-minute command timeout (an agent's default) stops a process.
The exit `0` is not reproducible from trekr: a run that returns ends with
the summary, and every error is non-zero. A pipeline's status (`trekr
--index . 2>&1 | tail`) is the last command's. Either way the gap was the
same — the index stopped and said nothing — and that is what this fixes.

**Not done: a longer wait.** Considered: let `--index` wait without bound,
saying so. The reported wait was not trekr's limit, so a longer one would
not have changed it; and past DEC-139's bound for one writer's turn,
something is stuck, and an index queued forever behind it is a hang. Exit `2` makes "run it again"
the caller's explicit choice.

**Addendum — the language server's side.** An LSP pre-release hunt found
the background index's end less true than the CLI's:

- *An outwaited child is asked again.* `trekr --index c100k`, then `--lsp`
  on mastodon with the writer wait at 3 s: the child exited 2 with nothing
  written, no mark stood, so the server read it as "index failed — see
  trekr --index" and never indexed again, while every hover said "trekr
  indexes it in the background". Exit 2 is "ask again", and the server is
  the caller: it runs the index again two seconds after (`AGAIN`), as often
  as it is outwaited — never a tight loop, since each child itself waits
  DEC-139's full turn for the lock. Re-run of the hunt's `behind.py`: two
  outwaits, then "indexed" at 17.7 s, the definition resolved. A child
  killed by a signal is still resumed once.
- *A hover promises only what is under way.* "Not indexed yet" ends "trekr
  is indexing it in the background" only while an index runs, is queued or
  is about to be; "trekr indexes it once another trekr process writing the
  index is done" while one waits; "`trekr --index` indexes it" when indexing
  is off or this session's index of it failed.
- *VS Code shows no `end` message* — `vscode-languageclient`'s progress
  ignores it — so each end is sent as a `report` first. An end that leaves
  the checkout partial or unindexed for good ("answers are partial until:
  trekr --index …", "index failed") is also one `window/showMessage`
  (warning), the one place a person sees it.
- *One count, now.* Progress said "4091 of 17322" while a hover said "873 of
  17322" (the count its tree was built at) and another session "0 of 3270".
  Every surface now reads the store's mark as of the moment it speaks — the
  tree still decides *whether* an answer is partial — and says what it
  counts: "N of M files read, counting its gems and Ruby's", the tree the
  confidence is scaled over. The first mark is written before the gems are
  listed, so its `of` was the checkout's files alone: that mark now carries
  `own` (a fourth field older builds ignore) and reads "files not counted
  yet". The CLI's `warming` is unchanged. Listing the gems first would give
  that mark a whole count, at the cost of locating and walking the gems (54 ms in one profiled cold index of
  mastodon)
  ahead of the first part's answers (DEC-322), which was not worth it for
  the half-second the mark lasts.
- *A child killed after its last commit is not a failure.* Killing the
  resume 1.4–1.6 s in left a whole store and no mark, and the progress said
  "index failed". A first index — nothing whole stood when it started — that
  ends with the checkout whole says "indexed", whatever it died of.

## DEC-401 — A lambda handed to a callback runs on the instance, as an `if:` one does

**Decided.** A lambda that is a positional argument of a callback macro —
`before_action -> { authorize! unless skip_auth? }`, `after_save lambda {
… }`, `validate -> { … }` — has its calls on `self` recorded on the
instance side of the class `self` is where the macro is written, by
DEC-342's rule for an `if:` lambda: the class in its body, and in a
concern's `ClassMethods` the concern. A call in it then resolves on the
instance through the class's ancestors, so a concern the class includes —
in another file — answers it, `confirmed` when it is the method's, as for
any instance call.

A callback macro is the name DEC-342 knows one by — `before_`, `after_`,
`around_`, and `validate` — less `rescue_from`, whose positional arguments
are the classes it rescues (its `with:` handler is not a positional
lambda). ActiveSupport::Callbacks `instance_exec`s a Proc callback whatever
its arity.

**Reported:** `--dead` called a concern's `skip_auth?` unreferenced, clear,
though `Api::BaseController` includes the concern and its `before_action`
lambda calls it. The lambda's call was read on the class side, where no
such method exists, so it was excluded. Testbed 400.

**Extraction changed**: those calls are recorded on the instance side, so
the store version moves for the change to reach an existing index.

**Measured** (`--dead app lib`, main 58dba19 vs this): mastodon unchanged
(no positional callback lambda calls a method it defines); discourse one
row: `UserEmail#destroy_email_tokens`, `single-caller` clear (its
`after_destroy` block) → not a candidate, its `before_save -> {
destroy_email_tokens(email_was) }` now counted — right.
rails (`--dead` of activerecord, actionpack, activesupport, actionview,
activemodel) two rows, both Rails' own `before_action -> { … }` in a
concern's `ClassMethods` calling the concern's instance method:
`AllowBrowser#allow_browser` (`unreferenced` clear) and
`RateLimiting#rate_limiting` (`unreferenced` lower) → `single-caller` —
right. Gold, discourse (900 app + 300 gem sites): verdict files byte-identical,
9 confidently `wrong` before and after.

## DEC-402 — `--dead` says when a method's name is written in YAML config

**Decided.** A `--dead` row whose method name a tracked `*.yml`/`*.yaml`
file in the checkout writes as a scalar says "named in config (PATH:LINE),
which is not read", and is graded `lower`, as DEC-315's view caveat is. The
tier stays: it is evidence of a way in — a dispatcher's `public_send`, a
`constantize(…).public_send` — never of a caller. A scalar names a method
when it is:

- the method of `Const.method` (`sanitizer: Pkg::FooSanitizer.sanitize_uid`),
  or
- a bare name shaped as only a method is: snake_case with an underscore
  inside it, or ending in `?`/`!` (`generator: pay_schedule_resources`).

Not read: a file under a `locales/` directory, and a lockfile
(`*.lock.yml`, `*-lock.yaml`). A key is never a name — only what it is set
to.

**Reported:** a notifications config resolved with `public_send`
(`generator: pay_schedule_resources`) and an export's column → sanitizer
mapping (`sanitizer: Pkg::FooSanitizer.sanitize_uid`) reach methods `--dead`
called unreferenced, clear. Testbed 401.

**The noise rules, measured** (discourse, the one corpus here with YAML in
quantity: 4,452 files; mastodon's bench copy has none; `--dead app lib`,
clear rows that would be lowered):

| rule | YAML files read | names | clear rows lowered |
| --- | ---: | ---: | ---: |
| every scalar of a name's shape | 4,452 | 3,751 | 24 |
| … less `locales/` and lockfiles | 209 | 802 | 15 |
| … and a bare name only when snake_case or `?`/`!` (this) | 209 | 487 | 7 |

Locales are 4,243 of discourse's YAML files and every hit in them was
translated text (`deleted`, `likes`, `timer`). A bare English word matched
eight candidates beyond the rule kept, none a method reference (`area:
moderation`, `- likes`). Of the seven the rule lowers: the one
`unreferenced` row is a true positive — `TopMenu.crawler_homepage_choices`,
which `config/site_settings.yml` names as `choices:` and discourse evaluates
— and so is `HighlightJs.languages` (`single-caller`); the other five are
incidental (`area: "trust_levels"`, the column list in
`migrations/core/config/intermediate_db.yml`), each on a `single-caller` or
`convention-only` row. discourse's `unreferenced` clear 98 → 97; mastodon
unchanged; rails (five libraries) unchanged — its 201 YAML files are test
fixtures and CI config, none naming a candidate.

**Not done: reading YAML as references.** A scalar has no receiver and no
dispatch; counting it as a caller would turn a coincidence into
`single-caller`. *Reverses if:* a convention for config dispatch is common
enough to model (a key whose value Rails itself sends).

## DEC-403 — `--dead` says when a method is in generated code

**Decided.** A `--dead` row in a file a generator wrote says "in generated
code (HOW), which its runtime may call generically", and is graded `lower`.
The file is generated when:

- `.gitattributes` marks it `linguist-generated` (asked of git with
  `check-attr`, so nested attributes files and patterns count as git reads
  them);
- a directory on its path is named `generated`; or
- one of its first five lines is a comment saying so: "DO NOT EDIT" or
  `@generated` anywhere in it, or one that opens "Generated by", "Code
  generated", "Auto-generated", "This file is (auto-)generated" / "was
  generated".

**Reported:** a GraphQL client's generated `Query.from_response!` listed as
unreferenced — the client's runtime calls it on whichever class the
operation names, so no call site writes it. Testbed 402.

**A caveat, not an exclusion.** Dropping generated files from the
candidates was considered: an edit to them is undone the next time the code
is generated, so a row there is rarely actionable. Turned down because
nothing is silently dropped — a generated method truly nothing calls is a
fact about the schema or the generator's config, and the row, saying where
it comes from, is how one finds it. `lower` is the conservative grade.

**A header only as a comment that opens with it.** "generated by" anywhere
in the first lines matched a discourse migration's class name
(`AddIsAutoGeneratedToIncomingEmails`) and would match "the token is
generated by SecureRandom". Opening the comment, or "DO NOT EDIT"/
`@generated` anywhere in one, matched only generated files in the corpora
surveyed: discourse's `plurals.rb` and `intermediate_db/*.rb`, a Rails
`schema.rb`, protobuf's and graph_weaver's output.

**Measured:** `--dead` on mastodon and discourse (`app lib`) and rails
(five libraries): no row moves — none of their candidates is in a
generated file. The rule costs nothing there and catches testbed 402's
three shapes.

## DEC-420 — `--dead` weighs classes, modules and constants by the references that resolve to them

**Decided.** `--dead PATH` lists, after the methods, every class, module
and constant (`FOO = …`) declared in scope that no constant reference in the
checkout resolves to. A row carries `kind: class | module | constant`,
`name` its last segment and `owner` the namespace holding it (`""` at the
top level); a method's row now carries `kind: method`. Text names it by kind
and whole name (`class Admin::Widget`) after the methods, and the summary
line says how many rows are constants; `summary.kinds` counts each kind.
`cli::dead_consts` does the work, with no change to what an index records:
the store gains one query (`const_refs_named`), the store stays v55.

- **A reference is a `const_ref` row, resolved by Ruby's lookup** — the
  ladder `tree::resolve` runs for `--def` and an editor's references (DEC-008,
  DEC-082) — so two `Error` classes are told apart. Every spelling of the
  candidate's name is read (`C`, `B::C`, `A::B::C`, `::A::B::C`) and each row
  resolved once per `(name, nesting)`. A row that could only add to
  constants already used is not resolved: `--dead app` on discourse reads
  66k rows and resolves 3k.
- **A reference to `A::B` is a use of `A`**, which holds it, and a namespace
  holding a constant a convention reaches (DEC-421) is used too; a namespace
  holding only candidates is one.
- **A reference inside the constant's own body is not a use**: `Gizmo.new`
  in `class Gizmo`'s method, `Widget::LIMIT` in Widget for Widget.
- **Not a definition, so never a candidate**: a reopening of a constant a
  gem or Ruby declares (`class String` in `lib/core_ext`) — deleting the
  checkout's body would not remove it; anything under a `db/` (an engine's
  or a plugin's too), which Rails runs by its file; and a shared example
  group's module (DEC-092), a group RSpec names by a string.
- **A new tier, `test-only`**: referenced only from files under a `spec`,
  `test` or `tests` directory. Such a class goes with its tests; the row says
  how many and where the first is. `summary.tiers` gains `test-only`.
- **A template's constants and an executable script's are references.** A
  view's Ruby is read for constant paths (`views::constant_paths`, outside
  strings, never `x::Y`) and an extensionless file with a Ruby shebang
  (`bin/cli`, `plugins/x/evals/run`) the same way; each resolves from the
  top level, where a view or script looks constants up.
- **A compact class under a namespace Zeitwerk makes from a directory**
  (`module Billing; class Invoice::Send` in `billing/invoice/send.rb`, with
  no `Billing::Invoice` declared) is declared by the tree at the top level,
  so `Billing::Invoice::Send` resolves to nothing. A reference that resolves
  to nothing is read for such a constant by its own spelling and by the name
  its file's path gives under `app/<kind>/` or `lib/`. Six of the first
  held-out sample's 50 rows were this (discourse-workflows' services).

**Why a namespace is used through what it holds.** Ruby resolves `A::B` by
first resolving `A`; deleting `A` deletes `B`. A namespace referenced only
as a prefix is the common case — `Admin`, `Api::V1`, every Zeitwerk
directory — and listing them would put most namespaces of an app on the
list.

**Why `test-only` is a tier and not a caveat.** A test naming a class is
evidence, of a kind no rule can upgrade: the class runs in the suite and
nowhere else. That is not "nothing names it", and it is not use. On the
hand-checked rows (DEC-422) every `test-only`, clear row was named only by
tests (mastodon 21 of 21, discourse `app lib` 13 of 13).

**Measured.** `--dead app` with references alone (no DEC-421 rule):
discourse 224 `unreferenced` and 273 `test-only` constant rows, mastodon 348
and 115. Timing, `--dead app --json`, five interleaved runs each on a
loaded machine, against main (dcb0559): discourse 3.21 → 3.59 s (+12 %),
mastodon 1.97 → 2.07 s (+5 %). The reference query is ~0.1 s of it on
discourse, the text read of DEC-421 ~0.15 s.

**Not done.** A constant with one reference is no `single-caller`: inlining
a class is not what that tier is for, and a constant's one reference is its
use, not a call to fold. `--refs Widget` stays name-level; a resolved
`--refs` for a constant would reuse this module's query.

## DEC-421 — What reaches a class by its name is read, and what may is said

**Decided.** Rails and the libraries an app runs find many classes by a
name, not a reference. `cli::dead_consts::ways` reads each way, from the
tree and, as DEC-315 and DEC-363 read views and built names, from the
checkout's Ruby and templates as text (`dead_consts::named`) and its YAML
(`cli::config`, DEC-402, which now also keeps the constants a scalar names).
A class no reference reaches is `convention-only` when one of these names
it, with `convention: {by, path?, line?}` in JSON as for a method:

| `by` | reaches |
| --- | --- |
| `routes` | a controller a route names, found as DEC-344 finds the action, or by the name its file spells (`admin/users/roles_controller.rb` is `admin/users/roles`); a routes file's string spelling a controller's path (`devise_for … controllers: { sessions: 'users/sessions' }`) |
| `Rails helpers` | a module under `app/helpers`, mixed into every view |
| `ActiveSupport::Concern` | a concern's `ClassMethods`, when the concern's class side answers `append_features` from ActiveSupport::Concern |
| `Active Record` | a model's `ActiveRecord_Relation` (and the two other relation classes), which Active Record makes and reopens by name |
| `Rails`, `Rails generators`, `Rails migrations`, `Action Cable`, `Action Mailer` | what inherits a Railtie, a generator, a migration, a channel, a mailer preview |
| `MiniScheduler` | a class whose class side answers `every` from MiniScheduler::Schedule |
| `ActiveModel::Serializers`, `Pundit`, `Draper` | `XSerializer`, `XPolicy`, `XDecorator` when the library is in the tree and a class `X` is in the checkout (a policy also by a symbol `:x`, Pundit's headless policy) |
| `ActiveModel validates`, `simple_form` | an `XValidator` (an `ActiveModel::Validator`) or `XInput` whose `x` a symbol or key outside its file writes (`validates :email, email_address: true`) |
| `constantize` | a name `"Jobs::#{type.camelize}".constantize` builds, whose built part a symbol outside its own file spells (`Jobs.enqueue(:send_digest)`) |
| `association` | a model an association names by convention (`has_many :line_items`) |
| `test runner` | a class in a test directory that inherits a test case, or in a `test_*.rb`/`*_test.rb`/`*_spec.rb` file |
| `subclasses` | a subclass of a checkout class whose subclasses a line lists (`Scorable.subclasses`) |
| `registration` | an ancestor's `inherited` or `included` hook, written in the checkout, that keeps what it is handed (`<<`, `push`, `add`, `register`); a body that hands the class to another's method as it loads (`HTTP::Options.register_feature(:x, self)`) |
| `gem namespace` | a class the checkout adds to a gem's namespace whose name a symbol spells (`OmniAuth::Strategies::Patreon`, `:patreon`) |
| `string` | a whole string spelling it — any string with `::`, a one-word string only where a class is looked up (`class_name:`, `constantize`, …) and only when no other constant of the checkout ends so — or a YAML scalar (`class: Scheduler::Vacuum`, `enum: "LevelSetting"`) |

What a row cannot be sure of is a caveat, graded `lower`, the tier kept:
a controller when routes are not all read or a gem's routes are drawn
(`devise_for`, `use_doorkeeper`, `mount`); an ancestor trekr has not
indexed; a class added to a gem's namespace; a name of its shape built at
runtime (a constantized string's shape, or an association's built name,
`has_one :"#{name.underscore}_search_data"` is `*SearchData`); a model's
subclass, which Active Record instantiates by a `type` column (its parent
neither `ApplicationRecord` nor abstract); a constant read on a value
(`self.class::PERMITTED`, `const_get(:LIMIT)`); a namespace whose constants
are listed (`Levels.constants`, also through a local, `steps = Steps`), or
looked up by a computed name (`Regions.const_get(name)`), or listed by its
own code or by the `extended` hook of a module it extends
(`Migrations::Enum`), or by a sibling's ancestor
(`self.class.name.deconstantize.constantize.constants`); a class whose
ancestor constantizes a name it computes (a factory); a file in generated
code (DEC-403).

**Each rule was found as a cause of a false row** — the routes, concerns,
helpers, policies, serializers, validators, jobs and schedules while
listing `--dead app`, the rest in the hand-checked samples of DEC-422 —
and each has a testbed case (420–435) that fails with the rules removed.

**Not done, and why.**

- *A nested serializer is its parent's lookup.* AMS 0.10 looks for
  `ParentSerializer::XSerializer` first; taking every serializer nested in
  one made `ActivityPub::ActorSerializer::AccountIdentityProofSerializer`
  convention-only though mastodon has no `AccountIdentityProof`. The class
  `X` is required.
- *A one-word string anywhere.* `"Application"` (an ActivityPub actor type)
  named `Mastodon::Application`, and `inflect.acronym 'CLI'` named
  `Mastodon::CLI`. Restricted to where a class is looked up, and an
  inflection's line is skipped: discourse `app lib config` string rows 73 →
  70, mastodon 2 → 1, none of the lost a true use.
- *An ancestor's listed subclasses for any ancestor.* `AbstractController::Base
  .descendants` in discourse's `config/application.rb` made `CustomRenderer`
  convention-only; only an ancestor the checkout declares counts.
- *A factory in the namespace's files, not only an ancestor's.*
  `lib/discourse.rb` writes `table.classify.constantize`; the namespace rule
  would have lowered seven true rows under `Discourse` (`TooManyMatches`,
  `CSRF`, `VERSION::TINY`, …).
- *A symbol in the class's own file for `constantize`.* A job that enqueues
  itself to retry (`Jobs.enqueue(:retrier)` in `Jobs::Retrier`) is reached
  by nothing else; only a symbol outside the file counts. For a gem's
  namespace the own file does count: a plugin defines an OmniAuth strategy
  and registers it by its symbol in one file (`discourse-patreon`).

## DEC-422 — `clear` on an unreferenced constant is calibrated against hand-checked rows

**Decided.** As DEC-372 did for methods: `clear` means no caveat, and what
that is worth was measured by hand. Each row below was read, its name
searched across the repo and its installed gems, strings and YAML included,
and its cause written down; a row is true when nothing in the repo or its
gems uses it by name, by listing or by convention.

| sample | as drawn | after the rules it found | true rows lost to `lower` |
| --- | ---: | ---: | ---: |
| mastodon `app lib config`, all (fitted) | — | 1/1 | 0 of 1 |
| discourse `app`, all 33 (fitted) | 32/33 = 97 % | 32/32 | 0 of 32 |
| discourse `lib`, all 37 (fitted) | 29/37 = 78 % | 29/29 | 0 of 29 |
| discourse `plugins`, random 50 of 291 (held out) | 7/50 = 14 % | 7/7 | 0 of 7 |
| discourse `plugins`, the 41 clear rows left after those rules (held out when drawn) | 30/41 = 73 % | 30/30 | 0 of 30 |
| discourse `script migrations`, random 45 of 97 (held out) | 14/45 = 31 % | 14/15 = 93 % | 0 of 14 |

Of the last sample, `script/` was 14 of 15 as drawn and `migrations/`, a
CLI that discovers its steps and enums by listing constants, 0 of 30.

| cause of a false row | rows | rule |
| --- | ---: | --- |
| a test case its runner finds (a vendored gem's `test/`) | 19 | `test runner` |
| a namespace's constants looked up by a computed name (`Holidays.const_get(region)`) | 16 | caveat |
| a module extending an enum helper whose `extended` hook lists them | 14 | caveat |
| steps listed through a sibling's ancestor (`deconstantize.constantize.constants`) | 11 | caveat |
| a shared example group's module | 10 | not a candidate |
| a factory in an ancestor (`class_name.constantize` in `Step.for`) | 7 | caveat |
| a compact class under a Zeitwerk directory namespace | 6 | DEC-420 |
| steps listed through a local (`steps = Steps; steps.constants`) | 5 | caveat |
| an extensionless script (`script/x`, `evals/run`) | 2 | DEC-420 |
| an association whose name is built (`has_one :"#{…}_search_data"`) | 1 | caveat |
| subclasses listed unqualified (`Scorable.subclasses` in the namespace) | 1 | `subclasses` |
| a model's relation class reopened (`Query::ActiveRecord_Relation`) | 1 | `Active Record` |
| a script's entry module, whose body runs the import | 1 | none |

Of the true rows: unused enum-like constants (17 of `WebHookEventType`'s,
replaced by an `enum` and `TYPES`), exception classes nothing raises,
version parts, page objects' selectors, a commented-out adapter, deprecated
shims with no callers (`TopicAssigner`, `BackupRestore::Backuper`), a model
for a dropped table (`UserOpenId`).

**`lower` separates.** mastodon's 14 `unreferenced`, lower rows were all in
use (`self.class::PERMITTED_PARAMS`, Devise's controllers, the RuboCop and
haml-lint cops `.rubocop.yml` loads). `test-only`, clear was right 21 of 21
on mastodon and 13 of 13 on discourse `app lib`; of mastodon's 21, 17 are
Sidekiq schedulers that a real checkout's `config/sidekiq.yml` names — the
bench corpus has no such file; with it the YAML scalar rule makes them
`convention-only` (testbed 425).

**What the held-out rows say.** On app code — discourse `app`, its plugins'
apps, mastodon — `clear` held after the rules. On code that finds its parts
by reflection — a CLI's steps, a vendored library's generated definitions —
it did not until the rules that sample produced, and the next such code
will have its own idiom. Read `clear` on a library or a tool with DEC-372's
warning.

**Counts**, `--dead app lib config`, rebased on dcb0559: mastodon 467
constant rows (426 `convention-only`, 26 `test-only`, 15 `unreferenced` of
which 1 clear); discourse 676 (520, 81, 75 of which 61 clear); discourse
`plugins` 991 (526, 276, 189 of which 37 clear).

## DEC-440 — A hook's `include` runs the included module's own hook there and then

**Decided.** When a module's `included`/`extended`/`prepended` hook mixes
another module into its base (DEC-102), and that module has a hook of its
own, the inner hook's edges are applied at that point in the outer hook —
before the outer hook's next line — as Ruby runs them. They were applied
only once the outer hook had finished, so an inner `base.extend` landed
after every outer one.

**Reported** by the comparison (2026-10-01): `Sidekiq::Job.included` does
`base.include(Options)` — whose hook extends `Options::ClassMethods` — and
then `base.extend(ClassMethods)`. `Job::ClassMethods`, extended last, is
nearer the singleton, and its `sidekiq_options` overrides `Options`'.
trekr put `Options::ClassMethods` last and answered every worker's
`sidekiq_options` with it at confidence 1.0. Testbed 440.

**Measured** (compare.py, 500 sites each, against main dcb0559): discourse
correct@1 78.0 → 78.4 %, wrong@1 18.0 → 17.6 %; mastodon 64.2 → 64.6 %,
34.6 → 34.2 %. All four moved sites are `sidekiq_options`, wrong at 1.0 →
correct. widget_shop, and the graph_weaver, accord and polyid gold sets
(app sites, `APP_SAMPLE=600 SEED=12`), unchanged.

## DEC-441 — A `class_eval` block in a method body does not replace the class's own methods

**Decided.** A `def` in a `Const.class_eval do … end` block written inside
a method body is recorded with `via: "class_eval in a method"` — still a
definition, but one that exists only once that method has run. Among the
definitions one owner has of a name, the last written wins (the store
orders a reopening after what it reopens), unless it is one of these: then
the owner's unconditional definition wins, and the deferred one answers
only when it is all there is.

The same reasoning as DEC-097's for a runtime mixin: what exists only if a
method is called is not what a load of the app runs. A method handed the
class to evaluate in (`def self.enable_expect(host = ::RSpec::Matchers);
host.module_exec do`, DEC-086) is not marked: it is an installer, which
exists to be called, and rspec-expectations defines `expect` that way at
boot (testbed 051 caught the first cut, which marked it too). A block in an
`after_initialize do` or a Railtie's `initializer do` is not a method body
and keeps winning as a reopening, which is how plugins patch.

**Reported** by the comparison: discourse's
`script/bulk_import/uploads_importer.rb` overrides
`RailsMultisite::ConnectionManagement.current_db` inside
`configure_site_settings`, and every `ConnectionManagement.current_db` in
the app resolved to that script at confidence 1.0. It now resolves to the
gem's `delegate :current_db` line, which is the method Ruby ran; the scorer
still counts it wrong@1, because the gold set records a multi-line
`delegate` at its first line. Testbed 441.

**Extraction changed**, so the store version moves.

**Measured**: no compare.py site moved in verdict on either corpus, and
the gem gold sets are unchanged; discourse's one site changed answer to
the right method, as above.

## DEC-442 — A residue's confidence is how often its first candidate ran, on the gold sets

**Decided.** A residue's `confidence` was a flat 0.0, whatever its first
candidate rested on: a guess wearing a measurement's clothes, and one that
could not pick out the residue worth showing. It is now the share of
gold-set residues resting on the same evidence whose first candidate was the
method Ruby ran:

| evidence | first candidate ran | confidence |
| --- | ---: | ---: |
| the call is on `self` (implicit or explicit) | 77 of 104 | 0.7 |
| an untyped receiver, and at most 3 definitions of the name | 48 of 67 | 0.7 |
| an untyped receiver, and more than 3 | 21 of 141 | 0.1, refit 0.2 below |
| no definition of the name at all | — | 0.0 |

A call on `self` is ranked by its own class's ancestors and namespace (the
residue tiers), and a name few classes define leaves little to choose from.
`agreement` carries the evidence ("6 definitions share the name; 0.2 of
residues with more than 3 …"), and `--explain` prints it as `evidence`.

**How it was fitted.** Every residue answer with candidates in the 0.8.4
comparison's samples (discourse 500, mastodon 500, widget_shop 63; 312
answers after DEC-440/441), scored by whether the first candidate is the
traced truth. Signals tried: the number of definitions of the name, the
receiver's shape, whether the receiver was typed, the first candidate's
tier, and the count tied at that tier. The definitions count and the
receiver's shape carried it; the rest were noisy or redundant with them.

**Held out.** Fit on discourse, checked on mastodon: self 0.80 → 0.52 held
out, few 0.75 → 0.70, many 0.08 → 0.19. Fit on a random half of all three,
checked on the other: self 0.75 → 0.73, few 0.70 → 0.73, many 0.14 → 0.16.
The order of the classes, and which side of 0.5 each falls on, held every
way. Brier score on the held-out corpus (lower is better): mastodon 0.400
flat 0, 0.218 for 1/definitions, 0.204 this; discourse 0.549, 0.311, 0.181.

**Rounded to one decimal**: a class is 67–141 answers, so its rate is good to
about ±0.05, and the corpora differ by up to 0.3 on the self class. Self and
few are both 0.7 at that precision, so they are one grade.

**Declined.** `1/definitions` needs no fit and is monotone, but it
under-calls every shared name (3 definitions: 0.33 predicted, 0.65
observed), because the ranking carries information it ignores. The first
candidate's tier was not used: "the enclosing class inherits from its
owner" was right 8 times in 31 on an explicit receiver — `present?` on
`ActiveRecord::Core` because a model's own ancestry was asked about another
object. That is a ranking question, not a confidence one, and is left for
a decision of its own.

### DEC-442 refit, after DEC-444 and DEC-445

DEC-444 and DEC-445 resolved 40-odd residues, mostly on `self` and on
relations, so the classes were counted again on the final build (by the
first CLI candidate, since the LSP now hides the weak ones): self 63 of 83
(0.76), few 42 of 61 (0.69), many 20 of 115 (0.17). Held out by corpus,
fit on discourse: self 0.85 → 0.52 on mastodon, few 0.67 → 0.68, many
0.10 → 0.21; a random half: 0.76 → 0.76, 0.71 → 0.65, 0.12 → 0.23. Self
and few stay one grade, 0.7; many moves to **0.2**, the nearer tenth to
0.17. Nothing changes on the LSP's side of 0.5. Brier on the held-out
corpus: mastodon 0.419 flat, 0.232 for 1/definitions, 0.221 this;
discourse 0.560, 0.391, 0.186.

## DEC-443 — The LSP returns a residue's guesses when the first is a fair one

**Decided.** `initializationOptions.unresolved` (VS Code: `trekr.unresolved`)
decides what `textDocument/definition` returns for a residue:

- `confident`, the default: its candidates, as `peek` would, when the
  first's confidence (DEC-442) is at least 0.5; otherwise nothing.
- `peek`: every candidate, best first, up to five — what it always did.
- `best`: the first candidate only.
- `none`: nothing.

An editor shows a location the same whether it was resolved or guessed, so
the setting is where a guess is let through. Hover still says the call was
unresolved, and the CLI still returns every candidate.

**Why `confident`.** compare.py over LSP, 500 sites each (after
DEC-440/441; answered / correct@1 / wrong@1 / found):

| mode | discourse | mastodon |
| --- | --- | --- |
| peek | 96.0 / 78.4 / 17.6 / 83.2 | 98.8 / 64.6 / 34.2 / 73.0 |
| best | 96.0 / 78.4 / 17.6 / 79.6 | 98.8 / 64.6 / 34.2 / 67.0 |
| **confident** | 86.2 / 77.6 / **8.6** / 79.2 | 81.0 / 61.2 / **19.8** / 65.0 |
| none | 67.6 / 62.8 / 4.8 / 64.0 | 65.8 / 51.4 / 14.4 / 53.8 |
| ruby-lsp | 63.0 / 51.4 / 11.6 / 56.8 | 72.6 / 51.2 / 21.4 / 57.0 |

`confident` halves wrong@1 for 0.8 and 3.4 points of correct@1, and puts
trekr below ruby-lsp's wrong@1 on both corpora while staying 26 and 10
points ahead on correct@1. Of the answers it gives, 90 % and 76 % are right,
against 82 % and 65 % for `peek`. `none` gives back most of trekr's lead
over ruby-lsp to shave four more points. `best` changes nothing scored at
@1. widget_shop: 100 / 61.9 / 38.1 → 95.2 / 61.9 / 33.3.

**What it costs**: found drops 4 and 8 points — a weak guess's peek list
was sometimes the way to the truth. An agent or a user who wants it sets
`peek`.

**Addendum — agents get `peek`.** `confident` is the default for a person
in an editor, who reads a jump as an answer. An agent is not that reader: it
can weigh five candidates against the code, and it has `trekr --def` and
`--refs` to check one. What it cannot do is learn why a definition came back
null — Claude Code's LSP tool shows no `window/showMessage` (DEC-331), and a
definition has no field for a reason — so under `confident` a weak residue
is the worst answer an agent can get: nothing, and no way to know there was
something. The trekr plugin's `.lsp.json` therefore sets
`"initializationOptions": {"unresolved": "peek"}`; Claude Code passes it
through (the plugins reference lists `initializationOptions`, "Options sent
in the initialize request", among an `lspServers` entry's fields). The
hover at the same position still says the call is unresolved.

Declined: defaulting by `clientInfo.name` ("Claude Code"). It would make one
server answer differently per client with no setting anyone can see, and the
plugin is the one place Claude Code users get trekr's LSP from.

## DEC-445 — A call at the top of a file runs on `main`, an Object

**Decided.** An implicit call written at the top level of a file — no class
or module around it, and in no block — has `main` for its receiver, an
instance of `Object`: `resolved_via: main`. A top-level `def` is Object's
private method and is found first, when only one file writes the name;
otherwise the lookup is Object's chain, so `require` is `Kernel#require`.
A call in a block keeps whatever `self` the block was given, which no file
states, and stays as it was. DEC-115's `describe` sent from `main` to
`RSpec` still answers first.

**Why.** `self` at the top level is `main`; leaving the call untyped ranked
any class's own `def require` first — discourse's `script/require_profiler.rb`
for every `require "base64"`. The earlier view (a unit test) was that `main`
is not indexed and so residue was the honest answer, but `main` is an
Object, and Object is.

**Measured** (compare.py, 500 sites each, after DEC-440–443): discourse
correct@1 77.6 → 79.0 %, wrong@1 8.6 → 7.2 %: seven top-level `require`s
wrong → correct (Ruby ran zeitwerk's `Kernel#require`, which the index
layers last), and fourteen `Fabricator(...)` and one `require_dependency`
residue → resolved, already right. mastodon and widget_shop unchanged.
Gem gold sets: correct and confidently wrong unchanged; a gem's top-level
`require` moves from a guess to `right-owner-wrong-site` (26 graph_weaver,
35 accord, 18 polyid sites) — `Kernel#require`, at the core stub's line,
where Ruby ran the replacement `bundled_gems.rb` defines at boot.

**Testbed** 442; 086 and 098 now answer a Minitest spec's bare `describe`
with minitest/spec's `Kernel#describe`, which is what it runs.

## DEC-444 — A relation chain is typed step by step

**Decided.** Three sources of `ActiveRecord::Relation`, so a chain's next
call lands on ActiveRecord instead of on whichever class shares its name:

- **ActiveRecord's own query methods return a relation.** `src/tree/
  activerecord.rb` states it as `sig`s — `QueryMethods#where`, `order`,
  `includes`, `joins`, `limit`, `or`, `select` without a block, …,
  `SpawnMethods#merge`, `Scoping::Named::ClassMethods#all`, and the same
  names on `Querying`, which a model's class methods `delegate` to `all`.
  They are lent to the gem's methods of that owner and name, as the RBS
  signatures are to the stdlib's (DEC-220): never a location, never an
  app's or another gem's method. `where` with no argument returns the
  WhereChain `where.not` is called on, which a positional count cannot tell
  from `where(x)` without saying something false, so only the calls that
  pass one are described.
- **A `scope` returns a relation**, and **a `has_many` (or `habtm`) reader
  is an `ActiveRecord::Associations::CollectionProxy`** — recorded at
  extraction, as a `belongs_to` reader's class is. A class that defines its
  own `scope` (Mongoid) is typed the same way; DEC-136's check that the
  scope runs on a relation is a tree-time rule this does not repeat.

The relation is not "of" a model: a model's scope called on one still goes
to the relation's lookup and, missing there, to residue.

**Reported** by the comparison: 14 discourse and 24 mastodon wrong@1 were
relation chains — `posts.order` answered with OptionParser's `order`,
`.pluck` and `.exists?` with unrelated classes'. ActiveRecord builds each
relation with `spawn`, `clone` and `klass.all`, which no reading types.

**Measured** (compare.py, 500 sites each, after DEC-440–443, 445):

| | discourse | mastodon | widget_shop |
| --- | --- | --- | --- |
| before | 86.2 / 79.0 / 7.2 / 80.6 | 81.0 / 61.2 / 19.8 / 65.0 | 95.2 / 61.9 / 33.3 / 61.9 |
| sigs | 87.6 / 80.4 / 7.2 / 82.0 | 82.0 / 62.4 / 19.6 / 66.2 | unchanged |
| + scope, has_many | 88.0 / 80.8 / 7.2 / 82.4 | 83.8 / 63.8 / 20.0 / 67.6 | 100 / 66.7 / 33.3 / 66.7 |

(answered / correct@1 / wrong@1 / found). 24 sites moved, 22 to correct.
Two mastodon sites went from a hidden residue to wrong: `.where` and
`.includes` on a `has_many` reader, which Ruby ran as CollectionProxy's own
`delegate(*delegate_methods, to: :scope)` — a list computed from
`QueryMethods.public_instance_methods`, which no reading names — and which
trekr answers with `QueryMethods#where`, the method that delegate sends to.
That is DEC-211's choice for a delegate, reached without the delegate. One
of the two is `resolved`, so mastodon's resolved-wrong count is 54, up one.
Gem gold sets unchanged.

**Extraction changed** (scope and has_many return types). Testbed 443.

### DEC-444 addendum: a relation hands a name it lacks to its model

**Reported** by the pre-release hunt: a model's class method or scope
called on a typed relation was ruled out. `Post.where(author_id: 1).popular`
and `user.posts.visible` were excluded from `--refs Post.popular` ("define
no such name") and `--dead` called `Post.popular` unreferenced, clear —
discourse's `Upload.with_no_non_post_relations` went from 3 possible to 3
excluded, and mastodon's `Tag.find_normalized!` and chatwoot's
`SortHandler::ClassMethods#sort_on_last_user_message_at` the same way.
The "not done" line above was the cause: a relation is "of" a model, and
`ActiveRecord::Delegation` hands a name the relation lacks to that model's
class, as DEC-116 already did for a scope's own body.

**Decided.** A name a relation (a `Relation` or a `CollectionProxy`) lacks
is looked up on its model's class side — scopes, class methods, an
extended `ClassMethods` — `resolved_via: relation`. The model is read back
along the chain: the class it starts from (`Post.where(…)`), through any
number of relation steps, or the class a `has_many` reader names
(`class_name:`, else its singular; a `source:` without `class_name:` names
none). With no model said — a local, a parameter — or a model that lacks
the name, `--refs` counts the site `possible`, never excluded; a model that
has the name from another owner rules it out as any receiver does. The
`has_many` reader now records its model (an extraction change, within
v56). Testbed 450.

**A variable assigned a chain is typed as the chain is.** Reported by the
LSP hunt: `accounts = Account.where(…).order(:id); accounts.limit(3)` was a
residue among 31 `limit`s, hidden by the editor's `confident` mode, while
the chain written out resolved. An assignment's value that is a call on a
call (`a.b.c`), or a call on a constant or a local whose own `sig` says
nothing, is now typed by the chain rung: the receiver typed, the method's
signature read, the gem's lent ones included (`via: sig`). A finder
(`Post.find`) keeps its convention, tried first. A write in a loop that
reads the variable it writes is bounded by the chain's depth. Testbed 451.


### DEC-445 addendum: `main` only where a plain script runs

**Reported** by the pre-release hunt: every top-level call was typed as
`main`, also in files Ruby evaluates on another object. On discourse,
`--refs Rake::DSL#task` went from 256 possible (0.8.4) to 258 excluded,
and every Rake `task` resolved at 1.0 to a `def task` that thor's
`rake_compat.rb` writes inside an `instance_eval do` block — a gem file
discourse never requires; `Plugin::Instance#register_asset` went from 129
possible to 123 excluded; a Gemfile's `gem` was `Kernel#gem` and a
`config.ru`'s `map` was "the receiver's type is known".

**Decided.**

- **A Rake file's `main` extends `Rake::DSL`** (a `.rake`, a `Rakefile`):
  a name the DSL has resolves there, the rest on Object.
- **A file evaluated on another object is not typed**, as before DEC-445.
  By name where its DSL shares names with Kernel's: a Gemfile or `gems.rb`
  or `*.gemfile` (Bundler's `gem` against `Kernel#gem`), a `config.ru`
  (Rack::Builder), a Jbuilder view. Otherwise by evidence: a file with a
  top-level call that neither Object nor a checkout's top-level `def`
  answers runs on something else — a Discourse `plugin.rb`
  (`register_asset`), a Guardfile, a Capfile with `install_plugin`. No
  further list: a name list would have to know every DSL, and the evidence
  rule needs none.
- **A top-level `def` is Object's only in the checkout, and only outside a
  block.** A gem's is Object's if something requires its file, which the
  index does not know; a `def` in a block is whatever the block runs on
  (`instance_eval do def task`), and exists only if the block runs. Such a
  `def` is recorded `via: "def in a block at the top level"` (an
  extraction change, within v56).

**Measured** on discourse, `--refs`: `Rake::DSL#task` 242 confirmed, 14
possible (tasks in a `namespace` block, untyped as before), 16 excluded
(specs' own `task` lets) — 256 reach it, as in 0.8.4, and 242 of them now
confirmed; `Plugin::Instance#register_asset` 3 confirmed, 129 possible, 8
excluded (a registry's own method). Gemfile `gem` and `config.ru` `map` are
residue at 0.2 (DEC-442 addendum). Testbed 452.

### DEC-442 addendum: a residue's evidence says what it rests on

**Reported** by the pre-release hunt: the classes were applied by the
call's shape alone. A typed receiver whose type lacks the name (a
relation) took "an untyped receiver" classes, and an implicit call in a
file another object evaluates took the self class, 0.7 — which cleared
the LSP's 0.5 for `config.ru`'s `map` on a wrong `self`.

**Decided.** The self class needs a class to rank by: an implicit call
with no class or module around it takes the untyped classes. A receiver
typed and found lacking — not by `self` — is a class of its own: 3 of 13
such residues in the 0.8.4 comparison's final samples (discourse and
mastodon, `c2`) ran the first candidate, **0.2**; the 13 are few, so it is
the nearer tenth of a rate good to about ±0.1, and on the LSP's side of 0.5
either way. `agreement` says "the receiver's type is known and lacks the
name".
