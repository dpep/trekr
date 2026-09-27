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
