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

### Shipped now

`not_indexed`, because it is the half that needs no policy: `--def`, `--refs`
and `--ancestors` on an unindexed checkout say so and exit 2, instead of
answering `residue` and reading as a finding about the code.

**Reverses if** the probe's blind spot turns out to bite in practice — the shape
to watch is an editor-driven workflow where files change constantly and nothing
touches `.git/index`. The answer then is a filesystem watcher in the LSP front,
which is a resident process that already exists and may legitimately watch,
rather than a daemon for the CLI.
