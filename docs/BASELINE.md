# trekr vs ruby-lsp — a measured head-to-head

The comparison PLAN §5 promised. Everything here is a run on this machine, and
the conditions matter more than the numbers, so they come first.

Reproduce with `script/baseline.py` (see the bottom of this file).

## Conditions

| | |
|---|---|
| machine | Apple M2, 8 cores, 2026-08-25 |
| corpus | `rails` — 3,307 Ruby files, its own checkout, gems installed |
| trekr | this repo, release build, index already built (`trekr --index`, 3.0 s) |
| ruby-lsp | **0.26.11**, isolated `GEM_HOME`, Ruby 3.4.9 |
| ruby-lsp indexer | the **old in-process indexer**, not Rubydex |

**On the version.** PLAN §8's whole competitive read is against ruby-lsp 0.27,
the Rubydex-backed rewrite. As of today that is **not published to RubyGems** —
`gem install ruby-lsp --version '>= 0.27.0'` finds nothing, and no `rubydex` gem
is installed alongside 0.26.11. So this measures the *shipping* incumbent, not
the one whose numbers PLAN §8 extrapolated. Every conclusion below should be
re-checked when 0.27 ships; the startup and memory figures in particular are the
ones Rubydex exists to improve.

**On fairness.** ruby-lsp is doing more than trekr: it composes a bundle,
indexes gems, and serves completion, formatting and rename that trekr refuses to
implement. Where a number is a consequence of that scope rather than of quality,
it is said so.

## Startup and memory

| | trekr | ruby-lsp 0.26.11 |
|---|---:|---:|
| `initialize` → response, **cold** (never opened here) | **6 ms** | **96 s** |
| `initialize` → response, warm | 6 ms | 1.1 s |
| first `goToDefinition` after that | 487 ms | 19 ms |
| warm `goToDefinition` median | **1.0 ms** | 9.6 ms |
| peak RSS after 45 queries | **176 MB** | **631 MB** |

The 96 s is not indexing — it is ruby-lsp *composing a bundle*. It writes a
`.ruby-lsp/Gemfile` into the checkout, resolves it, and only then starts. That
is the cost PLAN §1 named as the first durable edge, and it is real: a checkout
whose bundle is not installed cannot be served until one is. trekr's index is on
disk before the editor starts, so `initialize` returns in single-digit
milliseconds whether or not the repo has ever been seen — measured at 5 ms on a
never-indexed repo, where it correctly answers nothing until `trekr --index` runs.

The 487 ms first definition is trekr assembling its tree; ruby-lsp has already
paid that inside its 96 s. After the first query trekr is **~10× faster per
answer** and uses **3.6× less memory**, while ruby-lsp is also serving
completion and formatting that trekr does not implement.

## Answer quality — 45 goToDefinition positions

Positions chosen by stable content key from rails' call sites: 30 whose receiver
has a shape trekr's ladder can attempt, and 15 chained (`other`) receivers that
DEC-020 deliberately declines. Both servers got exactly the same 45.

| | count |
|---|---:|
| both answered | 11 |
| — agreed | 8 |
| — disagreed | 3 |
| only trekr answered | 8 |
| only ruby-lsp answered | 22 |
| neither | 4 |

**ruby-lsp answers more than twice as often, and that is a real win for it.**
Of its 22 extra answers, 13 point at repo source and 9 at RBS type stubs
(`array.rbs`, `module.rbs`) — a real answer, though not source you can read.
Fifteen of the 22 are the chained-receiver bucket trekr declines outright.

**Where both answered and disagreed, trekr was right 3 for 3** — hand-checked
against the source:

| position | trekr | ruby-lsp | correct |
|---|---|---|---|
| `stamped.updated_at`, `stamped = Mixin.new` | the `mixins` schema column | `Task#updated_at` | **trekr** — `Task` is an unrelated class |
| `Rails.application` | `Rails.application` in `rails.rb` | a helper in `abstract_unit.rb` | **trekr** |
| `as.call(env)`, `as = MemoryStore.new` | `Rack::Session::Abstract::Persisted#call` | `Method#call` in `method.rbs` | **trekr** |

Spot-checking seven of ruby-lsp's extra answers by hand: six are right
(`migrations.all?` → `Array#all?`, `require` → RubyGems' `Kernel#require`,
`@response.body` → `ActionDispatch::Response#body`), and one is confidently
wrong — `@store.read` on a cache store answered `Dir#read`.

So the shape of the difference is not "one is better". It is:

- **ruby-lsp answers a chained or untyped receiver by guessing** — from a
  variable's name, or from RBS — and is right most of the time and wrong some of
  the time, with nothing in the answer to say which.
- **trekr answers only when the receiver resolves**, and says `residue` with a
  reason otherwise. It never returned a wrong location in this set.

Which is better depends entirely on the consumer. For a human skimming, a
usually-right guess is useful. For an agent, a wrong go-to-definition costs a
file read and a retry, and there is no signal to distinguish it — which is the
argument PLAN §1 makes for confidence-graded answers, now with a concrete
example of the failure it is guarding against.

**A trekr gap this exposed**: `require`, `Array#each` and friends resolve to the
core stub, which is not a real file, so `location()` drops them and the answer is
empty. ruby-lsp returns the RBS declaration. Returning *something* — the stub's
own line, marked as such — would be better than silence.

## findReferences

Same positions — each server asked at the method's own definition line.

| query | trekr | ruby-lsp |
|---|---|---|
| `ActiveSupport::Testing::Declarative#test` | 6,665 in **1.2 s** | 6,829 in 5.9 s |
| `ActiveRecord::Querying#where` | 1,255 in **0.44 s** | **190** in 5.2 s |
| `ActiveRecord::ConnectionHandling#lease_connection` | 1,137 in **0.38 s** | 1,177 in 5.9 s |

**4.4× to 13× faster on every query.** On two of the three the counts are close;
on `where` they are not, and the gap is the interesting part. `where` exists
because of `delegate(*QUERYING_METHODS, to: :all)`. ruby-lsp finds **190**
references to a method that has over 1,800 same-name call sites in the repo —
it cannot attribute calls to a method no `def` declares. trekr models the
delegation (session 7), so it sees them.

Two caveats, both against trekr:

- Its 6,665 for `test` is slightly *lower* than ruby-lsp's 6,829, and some of
  that difference will be sites trekr excluded. Excluded sites are the product,
  but they are also where a recall bug would hide, and this comparison does not
  adjudicate them.
- **The LSP `references` numbers above are un-narrowed.** At the time of this
  run the server took only the *name* from the position, so `Querying#where` and
  every other `#where` shared one answer. Fixed immediately after (the server
  now resolves the position to its owner, as the CLI always did), so re-running
  will give different — smaller and more accurate — counts for trekr.

## Verdict, and what it says to do next

| | winner | margin |
|---|---|---|
| cold start on an unprepared checkout | **trekr** | 6 ms vs 96 s |
| warm start | **trekr** | 6 ms vs 1.1 s |
| per-query latency, warm | **trekr** | ~10× on definition, 4–13× on references |
| memory | **trekr** | 176 MB vs 631 MB |
| goToDefinition **coverage** | **ruby-lsp** | 33/45 vs 19/45 |
| goToDefinition **correctness where they differ** | **trekr** | 3/3 |
| references on a DSL-defined method | **trekr** | 1,255 vs 190 |

Predictions on record before measuring, scored honestly:

1. *"trekr wins startup and references decisively."* **Held**, and by more than
   expected on references.
2. *"ruby-lsp wins some method definitions where GuessedType happens to be
   right — count those as their win."* **Held, and understated.** It answered 22
   positions trekr did not, and roughly six in seven of a hand-checked sample
   were right. That is a bigger win than "some".
3. *"Constants roughly a tie."* Not separately measured; folded into the 45.
4. *"ruby-lsp may not boot without a bundle."* **Wrong** — it composes one
   itself. The cost is 96 s and a `.ruby-lsp/` directory written into the
   checkout, not a failure.

What this says to improve, in order:

1. ~~Owner-narrow the LSP references path.~~ **Done** — it was the largest
   correctness gap this exercise found, and it was a five-line fix once the
   measurement pointed at it.
2. **Return a location for core methods.** `require` and `Array#each` resolve
   and then answer nothing because the core stub is not a real file.
3. **The chained-receiver decision (DEC-020) now has a price tag**: 15 of the 45
   positions, which ruby-lsp mostly answers correctly. That does not overturn
   the decision — those answers come from guessing, and one of the seven checked
   was confidently wrong — but "we decline 1 in 3 positions" is the honest
   statement of its cost.

## Reproducing

The harness is scripted LSP sessions over stdio against both servers; the
scripts live under `/tmp` in the session that produced this and are not
committed, because they hard-code an isolated `GEM_HOME` and a ruby-lsp install
that this repo deliberately does not depend on. What is committed is this file
and the conditions above. Re-running means: install ruby-lsp into a scratch
`GEM_HOME`, drive both with the same LSP client, and use content-keyed sampling
(`script/bench.py`'s `stable_sample`) so the query set is the same one.

---

## Postscript, 2026-08-25 — after ranked residue and core locations

The ruby-lsp column above stands: same binary, same install, unchanged
conditions. Only trekr changed, in the two ways this document said to change it.

**goToDefinition coverage, same 45 stable-keyed positions:**

| | before | after | ruby-lsp |
|---|---:|---:|---:|
| answered | 19/45 | **44/45** | 33/45 |

The 25 new answers are not new resolutions. They are the ranked candidates the
CLI always produced and the LSP surface was discarding, plus core definitions
that now have a file to point at. `hover` at those positions says
`status: Residue, confidence: 0.00`, which is the whole difference between this
and guessing.

**Where the two now disagree — 20 positions, split by whether trekr was
confident:**

| trekr's own status | count | who was right |
|---|---:|---|
| `resolved` | 4 | trekr 3, near-tie 1 |
| `residue` (a ranked guess) | 16 | roughly even |

The four confident disagreements are the three hand-adjudicated above — where
ruby-lsp sent `stamped.updated_at` to an unrelated class, `Rails.application` to
a test helper, and `as.call` to `Method#call` — plus `migrations.all?`, where
trekr answers `Enumerable#all?` and ruby-lsp the more precise `Array#all?`.
Call that one theirs.

Of nine guessed disagreements adjudicated by hand: trekr's top candidate was
right on `require`, `Module#undef_method` and `RouteSet#draw`; ruby-lsp was right
on `@response.body`, `time.time_zone`, `sorted_groups.each` and
`database.service`; both were wrong on `@store.read`. **Four honest losses in
nine** — positions where our top guess is wrong and theirs is right. On a
sample that small the only safe claim is "roughly even", which is what the
prediction said (half to two-thirds) and is close enough to call it held.

**The predictions, scored:**

1. *"Answered lands at 34–40, at or above ruby-lsp's 33."* **Beaten** — 44.
   Under-predicted because core locations closed a bucket I had counted as
   separate.
2. *"Top-candidate correctness on newly-answered positions worse than our
   resolved answers, roughly half to two-thirds right."* **Held** — about half,
   against 3/4 on the confident ones.
3. *"Core locations add a handful."* **Held.**
4. *"`concerning`/`table_name` move nothing measurable on rails."* **Held** —
   no movement in this table; `table_name` was done for correctness.

**What this changes about the DEC-020 price tag.** It does not overturn the
decision — trekr still does not *resolve* a chained receiver, and the 16 guesses
are labelled as guesses. What it removes is the part of the price that was
self-inflicted: 1 in 3 positions returning **null** when a ranked answer was
already computed. The remaining cost is that our guess is right about half the
time on those positions, and we say so.


## Runtime truth: the TracePoint gold set (session 12)

Every accuracy number above this line came from a hand audit of a sample. This
one does not. `script/trace_gold.rb` runs inside a bootable Rails app under a
`TracePoint`, recording for each call site which method Ruby *actually*
dispatched to and where that method is defined; `script/gold.py` asks
`trekr --def` the same question and scores it.

First run, on `widget_shop` (Rails 8.1, full bundle): **859 gold call sites**,
250 scored.

| verdict | share | meaning |
| ------- | ----- | ------- |
| correct | 17.6 % | resolved, and to the file and line Ruby used |
| residue-hit | 8.0 % | declined to resolve, but offered the truth as a candidate |
| residue | 39.6 % | declined, and did not offer it |
| **wrong** | **13.2 %** | resolved, confidently, somewhere else |
| missed | 21.6 % | found no name at that position |

Found the true definition, resolved or offered: **25.6 %**.

**Read this with its caveat, which is large.** 247 of the 250 sites are inside
*gem* code — Rails' own internals — because widget_shop's app code is 40 lines
of pure declaration with no method bodies to call from. Rails internals are the
hardest Ruby there is: module builders, `included do` blocks, abstract methods
overridden per adapter, and `Kernel#require` replaced by Zeitwerk. This is a
floor, not the number for app code, and it is not comparable to the 42 % `--def`
figure measured on rails constants.

The confident misses cluster into three shapes, and they are more informative
than the headline:

* **abstract/override pairs** — `write_query?` and `build_statement_pool` are
  declared on the abstract adapter and dispatched to the SQLite3 one, because
  `self` was a SQLite3 adapter. Static resolution finds the declaration; Ruby
  ran the override.
* **`included`** — resolved to the concern's own `included do` rather than
  `ActiveSupport::Concern#included`.
* **monkey-patched core** — `require` really goes to Zeitwerk's `Kernel`
  override.

Only the third is beyond reach. The first two are ranking and lookup questions
with known shapes.

**Next**: this needs an app with real method bodies. The harness takes
`TREKR_EXERCISE` and any bootable app, so that is a matter of pointing it
somewhere better, not of writing more harness.


## The gold set on app code (session 13)

Session 12's numbers were measured through two harness bugs — the tracer
dropped every call within one file, and every call with an explicit receiver —
so they described a filtered slice and are **superseded**. Same corpus, fixed
harness: 1,067 sites became 3,073.

widget_shop now carries ~137 lines of app code written independently of the
resolver (`app/services/`, `app/jobs/`, `app/models/concerns/`). All 63
app-scope sites scored, plus 2,922 gem sites.

| verdict | app code | plain app methods | gem code |
| ------- | -------- | ----------------- | -------- |
| correct | 19.0 % | 26.7 % | **38.3 %** |
| offered as candidate | 14.3 % | 20.0 % | 23.6 % |
| **found the definition** | **33.3 %** | **46.7 %** | **61.9 %** |
| confidently wrong | 17.5 % | 24.4 % | 6.1 % |
| no name at that position | 19.0 % | 26.7 % | 6.7 % |

**Predicted 45 % correct on app code; got 19 %.** The per-bucket predictions
were wrong in an instructive direction, and the headline result is the
inversion: **gem code scores twice as well as app code**. Rails' own internals
are ordinary Ruby — explicit receivers, plain method calls — while a Rails
*app*'s surface is macros, and macros are where this engine is weakest.

Two structural causes, both visible in the per-site list rather than the
totals:

* **Class-body macros are not call sites at all.** `belongs_to`, `has_many`,
  `scope`, `delegate`, `has_one`, `after_save` — 12 of 63 app sites answer
  "no name at this position", because the extractor *consumes* a macro to
  generate the methods it implies and never records the macro call itself.
  Asking what `belongs_to` is gets nothing.
* **Rails-generated methods are a different question, and are scored as
  one.** For `price_cents` or `supplier`, runtime truth points at
  `attribute_methods.rb` / `association.rb`, where the `define_method` ran.
  trekr points at `belongs_to :supplier` or the schema column — which is the
  answer a person wants. 18 of 63 app sites are this shape; blending them into
  one rate would have flattered or condemned the engine depending on which
  side was called correct, so they are reported apart: **22 % answered with
  the declaration, 33 % offered it, 44 % nothing**.

The remaining plain-method misses are `included` and `class_methods`
(resolved to the concern's own block rather than `ActiveSupport::Concern`) and
class-method calls that should land in a gem's `ClassMethods` module —
`find`, `find_by`, and the enum scope `retired`.


### After the two app-code fixes (same 63 sites)

Recording macros as call sites, then preferring real source over `.rbi` stubs:

| verdict | before | after |
| ------- | ------ | ----- |
| correct | 19.0 % | **42.9 %** |
| found the definition | 33.3 % | **57.1 %** |
| confidently wrong | 17.5 % | **7.9 %** |
| no name at that position | 19.0 % | **4.8 %** |

Plain app methods alone: correct 26.7 % → **60.0 %**, found **80.0 %**, wrong
**11.1 %**. The gem floor is unchanged at 36 % / 61 % — neither fix touches it,
which is the check that they did what they claimed and nothing else.

The predicted 45 % turned out close to the truth (42.9 %); what the prediction
missed was that two defects were masking it, not that the ladder was weak.

Every app-code miss that remains: `find` / `find_by` / the enum scope
`retired` resolve to `Widget::CommonRelationMethods` from Tapioca's DSL file
where runtime truth says `ActiveRecord::Core::ClassMethods` (the AR-finder
shape); `after_save` and two `id` calls inside `included do` and `self.class`
chains find no name; `count` and `quantity` stay residue.


### Why the finder rung does not show up in these numbers

The AR-finder rung (`w = Widget.find(id)` types `w`) moved the gold set by
**zero**, and that is not evidence against it. widget_shop is Tapioca-equipped:
`sorbet/rbi/dsl/widget.rbi` declares that `Widget.find` returns `::Widget`, so
the `sig` rung already typed every one of these locals — `--def` reports
`via=sig` on exactly the sites the new rung was built for.

So this corpus cannot measure the rung, and the corpus measurement is the
evidence that stands: across rails, discourse and mastodon — none of which use
Sorbet — 4,333 finder assignments would newly type **12,005 call sites**.

The general lesson for the gold set: a Sorbet-equipped app measures a *more
favourable* engine than most Rails apps, because signatures do work the
receiver ladder would otherwise have to. A second corpus without RBIs would
measure the common case.


## Sigs on vs sigs off — a controlled experiment (session 14)

`widget_shop-nosorbet` is a git worktree of widget_shop with the whole
`sorbet/` tree removed and the app code byte-for-byte identical. Same
exerciser, same harness, same binary, same sample seed. 63 app call sites in
each; 400 gem sites sampled.

| verdict | sigs ON | sigs OFF |
| ------- | ------- | -------- |
| app correct | 42.9 % | **49.2 %** |
| app found the definition | 57.1 % | **61.9 %** |
| app confidently wrong | 7.9 % | **4.8 %** |
| app no name at that position | 4.8 % | **0.0 %** |
| plain app methods correct | 60.0 % | **68.9 %** |
| plain app methods found | 80.0 % | **86.7 %** |
| gem floor correct | 36.0 % | 38.2 % |

**Removing Sorbet makes trekr better on every measure.** Predicted the
direction (better, not worse) and roughly the size: predicted 45 % correct and
5 % wrong, got 49.2 % and 4.8 %.

That is worth stating plainly, because the naive expectation is the opposite —
delete type information, lose accuracy. What actually happens is that Tapioca's
committed RBIs introduce a **shadow namespace** that competes with real code:

* `find` and `find_by` are `wrong` with sigs on, resolving to
  `Widget::CommonRelationMethods` — an owner that exists only in
  `sorbet/rbi/dsl/widget.rbi`. Runtime truth says
  `ActiveRecord::Core::ClassMethods`. With the RBI gone they resolve correctly.
* `after_save` and two `self.class`-chained `id` calls find no name with sigs
  on and resolve without them.

DEC-019's rule (session 13) preferred real source over a stub **at the same
owner**. It did not help when the RBI invents an owner that does not exist at
runtime and that owner sits earlier in the lookup chain — so the rule was
extended to span the chain (DEC-019 update). With that in, the sigs-ON column
becomes **46.0 % correct / 4.8 % wrong / 60.3 % found**, closing half the gap;
the table above is the state that motivated the change.

**Both columns matter.** The target monorepo is roughly 30 % Sorbet-covered, so
neither column is "the" number: sigs-off is the common case, sigs-on is the
case where a signature is available *and* where the shadow namespace does
damage. Reporting one alone would mislead in whichever direction was chosen.

### The finder rung, measured end to end at last

Session 13 shipped `w = Widget.find(id)` → `w` is a `Widget` and could not
measure it, because widget_shop's RBIs already typed those locals `via=sig`.
Sigs off, the rung carries them: `find` and `find_by` move from **wrong to
correct**, and the app `missed` count goes to zero. This is the rung's first
end-to-end evidence, and it is the difference between crediting a feature and
knowing it works.

### Worktree blob sharing, on a real second checkout

Indexing the worktree: **35 files, 0 blobs parsed, 0.05 s**. Every `.rb` blob
was already known from the main checkout, because the app code is identical and
facts are keyed by blob OID. Predicted 0 parsed and under a second.


### Ranking the residue (session 14)

A residue answer is only worth having if the right guess is near the top of the
list a reader scans, so the gold set now measures that directly: the 1-based
position of the true definition among the ranked candidates, for every site
where it was offered at all.

Two named signals added — the receiver's *name* (`@widget` ranks `Widget`'s
methods, a convention that ranks and never promotes) and this checkout's own
code before a dependency's. Measured on the no-Sorbet corpus, same binary
otherwise:

| | before | after |
| --- | --- | --- |
| app: truth ranked #1 | 50.0 % (4/8) | **66.7 %** (6/9) |
| app: top-3 | 62.5 % | **77.8 %** |
| app: MRR | 0.623 | **0.760** |
| gem: truth ranked #1 | 59.6 % (31/52) | **64.2 %** (34/53) |
| gem: top-3 | 76.9 % | **81.1 %** |
| gem: MRR | 0.723 | **0.757** |

The app sample is nine sites, so treat that column as a direction and the gem
column as the number. Both move the same way, and the gem column is 53 sites.

Ranking also surfaces slightly more truth inside the eight-candidate cap:
app found-the-definition 61.9 % → 63.5 %, gem 63.8 % → 65.0 %.

Found while adding this: the existing "same file" signal compared a
checkout-relative path against an absolute one and had never fired.


### The "missed" verdict was hiding a crash (session 15)

Three of the sigs-on app sites scored `missed` — "no name at this position".
They were not misses. `trekr --def` was **aborting** on them with a stack
overflow, and the scorer could not tell a dead process from an empty answer.
It can now: `crashed` is its own verdict.

With the crash fixed, the sigs-on column becomes **49.2 % correct / 6.3 %
wrong / 58.7 % found**, and `missed` is zero. That is the same `correct` rate
as the sigs-off column (49.2 %), so the gap the controlled experiment measured
was, on that axis, entirely this bug.

The lesson generalizes past this scorer: a harness that maps *every* failure to
one benign bucket will hide the severe ones behind the ordinary ones, and it
will do so most convincingly when the benign bucket is plausible.


### The Rails-generated bucket is not a gap (session 15)

Session 14 reported that bucket as "44 % nothing at all" and made it the
largest block of unanswered app-code sites. That was a **scorer artifact**. The
scorer decided whether an answer pointed "into the app" by comparing against
the common prefix of the traced source files — which is `app/`. A generated
attribute is answered with `db/schema.rb` (DEC-022), which shares no directory
with `app/`, so every one of those was filed as residue.

Scored against the *checkout* root, the bucket is fully covered:

| generator family | sites | trekr's answer |
| ---------------- | ----- | -------------- |
| schema attribute (`price_cents`, `quantity`, `name`, `reference`) | 8 | the schema column, all offered |
| association reader (`supplier`, `orders`, `widget`) | 6 | the `belongs_to` / `has_many` line |
| enum predicate (`active?`, `draft?`, `retired?`) | 3 | the `enum` line |
| the `enum` macro call itself | 1 | resolved |

**22.2 % resolved outright, 77.8 % offered as a candidate, 0 % nothing.**

So the recommendation is the opposite of "model the next two generator
families" — all three families are already modelled and already produce the
right declaration. What is missing is **promotion**: those answers sit among
candidates instead of resolving, because the receiver is an ivar filled from an
untyped constructor parameter (`@order = order`), and no rung types it.

That makes receiver typing, not generator coverage, the lever for the largest
remaining block — and it is the same signal already shipped for *ranking* in
session 14 (`@widget` names `Widget`), which would have to be corroborated
before it could promote.


## The scorer's verdicts, audited (session 16)

Two published numbers had already been wrong because the harness mapped unlike
outcomes to one bucket. So before measuring anything else, every verdict the
scorer can emit was walked and asked: **what distinct realities land here?**
Four conflations were found and split.

| was | split into | why it matters |
| --- | ---------- | -------------- |
| `wrong` | `wrong`, `right-owner-wrong-site`, `column-mismatch` | Resolving to a *different method* and resolving to the right method at the wrong line are different failures. And a gold entry whose column names a different token is a **harness** defect, not an engine error — counting it as one overstates the error rate and hides a fixable gold-set bug. |
| `residue` | `residue-ranked-out`, `residue-nothing-known` | Knowing nothing is a coverage gap; knowing things and ranking the truth out of the top eight is a ranking gap. They call for opposite work. |
| `crashed` | `crashed`, `not-indexed` | Exit 2 is trekr's defined "cannot serve", not a crash. Three gem sites looked like a live P0 and were Ruby *stdlib* files in no indexed checkout. |
| `missed` | renamed `no-name` | It never meant "we missed it"; it means the position holds no name. |

`column-mismatch` is excluded from the denominator and reported beside the
table, because it is a fact about the gold set rather than about trekr.

### Corrected tables

Same corpus, same seed, same sample as session 14, with the audited scorer and
the current binary. **Corrections, stated rather than quietly superseded:**

| | session 14 | corrected |
| --- | --- | --- |
| app correct | 49.2 % | **50.0 %** (62 scored; 1 excluded as a harness fault) |
| app found the definition | 61.9 % | **64.5 %** |
| app confidently wrong | 4.8 % | 4.8 % (unchanged) |
| gem correct | 38.2 % | 38.2 % (unchanged) |
| gem confidently wrong | 5.2 % | **3.8 %** — 1.5 pts were right-owner-wrong-site, 1.0 pt was not-indexed |
| generated bucket "nothing" | 44 % | **0 %** (corrected in session 15; now 17.6 % resolved / 82.4 % offered) |

The gem floor's residue splits **12.5 % ranked-out / 16.0 % nothing-known** —
so roughly two fifths of what looked like a coverage problem is a ranking
problem. One caveat kept honest: `residue-ranked-out` means the truth was not
among the eight candidates returned; it cannot by itself distinguish "in the
index but ranked past eight" from "not in the index while same-named methods
are".


### Receiver-name promoted from ranking to typing (session 16)

Same corpus, same seed, same sample; the rung is the only change.

| | ranking only | + typing rung |
| --- | --- | --- |
| app correct | 50.0 % | **54.8 %** |
| app generated: resolved | 6.5 % | **24.2 %** |
| app generated: offered | 21.0 % | 3.2 % |
| app confidently wrong | 4.8 % | **4.8 %** |
| gem correct | 38.2 % | **39.0 %** |
| gem confidently wrong | 3.8 % | **3.8 %** |

**14 of the ~23 app sites where the answer was already sitting in the
candidate list were promoted to resolved — 61 %.** Predicted 60–75 %, so the
size was right. Gem promotion was +0.8 points against a predicted "under 5",
also right: gem code names receivers after their class far less often.

**Zero new confidently-wrong on either corpus**, against a threshold of +2.0
points set before running, and the three wrong app sites are the same three
before and after — so nothing that resolved correctly was traded away.

`found the definition` is unchanged at 64.5 %, exactly as predicted: these
sites already counted as found. The rung does not find new answers, it
**promotes answers already found** from a list a human must read to one the
engine will stand behind — which is the difference between a tool that helps
you look and one that answers.

The one design mistake worth recording: the first version also disqualified on
"an assignment exists that we could not type", which sounded prudent and
removed almost the entire population — `@widget = widget` from a constructor
parameter is exactly that shape, and it is the case the rung exists for. The
guard was also redundant: the rung is only reached because assignment typing
already failed.


### What the resident front is actually asked (session 16)

`trekr --usage`, over the log accumulated since session 11 — 22 requests across
3 sessions. **A thin sample, and that is itself the first finding**: real LSP
usage so far is a handful of spot-checks per session, not a stream.

| operation | calls | answered | median | p90 |
| --- | --- | --- | --- | --- |
| `definition` | 8 | 75 % | 414.7 ms | 781.3 ms |
| `hover` | 4 | 100 % | 1.4 ms | 13.0 ms |
| `prepareCallHierarchy` | 3 | 100 % | 0.3 ms | 0.4 ms |
| `incomingCalls` | 2 | 50 % | 385.1 ms | 385.1 ms |
| `references`, `workspaceSymbol`, `documentSymbol`, `outgoingCalls`, `implementation` | 1 each | — | — | — |

Three things worth acting on:

* **`definition` is the product** — 36 % of all calls, more than the next two
  together. Everything else is a rounding error by comparison.
* **Its median is 415 ms, not the 0.2 ms the amortization measurements
  promised.** Those measurements were right about a *warm* session; real usage
  restarts the server between spot-checks, so nearly every `definition` call is
  somebody's first and pays the cold tree build. The resident front only
  amortizes for a client that stays resident.
* **`implementation` has answered nothing, ever** (1 call, 0 answered), and
  `definition` came back empty a quarter of the time.

The sample is too small to set an agenda on its own, but the shape of the cold
-start problem is now measured from real usage rather than from a bench loop.


### The empty `definition` responses (session 17)

`--usage` reported `definition` answering nothing a quarter of the time. Mining
the log for *which* sites: both empties are the same file,
`widget_shop/app/models/report.rb`, which **no longer exists** — a scratch file
made during a spot-check and deleted after. Of eight logged `definition` calls,
the two empty ones are that file. So the quarter is an artifact of an
eight-call sample, not a product gap.

The product question behind it was worth asking anyway, and the answer is that
the surface already does the right thing: **LSP `definition` does not drop
residue candidates.** Session 11 taught `goToDefinition` to return ranked
candidate locations when the receiver does not resolve, capped at five, order
being the disclosure — verified again here against a live residue position.
Nothing to change.

What the same probe *did* surface is that `@account.local?` had begun answering
`resolved` at 0.03 confidence, which became DEC-027.


### The ranked-out slice is not a ranking problem (session 20)

Three sessions carried "10.8 % of gem residue is ranked-out — sitting yield for
ranking features" as a lead. It was wrong, and the test that settles it is one
line: **raise `MAX_CANDIDATES` from 8 to 500 and re-score.**

The bucket did not shrink by a single site. The true definition is not in the
candidate pool at all, so no ordering can reach it. The verdict is renamed
`residue-truth-absent` — a second flavour of coverage gap, where *something*
with that name was found but not the thing Ruby ran.

Two ranking features were built and measured against it before this was
understood: chain proximity moved nothing (tier 0 rarely holds two candidates),
directory affinity moved one site of 65. Both turned down (DEC-028).

**What to do instead**: the two residue buckets are now 16.0 % nothing-known
and 10.8 % truth-absent, and both are coverage. Understanding *why* the truth
is absent — unindexed source, an owner the extractor did not model, a
runtime-built method — is the question with 27 % of the gem sample behind it.


### The namespace fixpoint, profiled (session 20)

`assemble` is the largest remaining item in a tree build now that methods load
on demand. Profile only, no redesign:

| corpus | declarations | fixpoint rounds | fixpoint time | of total assemble |
| ------ | ------------ | --------------- | ------------- | ----------------- |
| rails | 19,697 | 3 | 22 ms | of 34 ms |
| discourse | 69,305 | 3 | 97 ms | of 143 ms |

**It scans every declaration three times.** Round one places almost everything;
rounds two and three exist to catch the stragglers — `class A::B` where `A` was
itself written compactly and had not been placed yet — and to observe that
nothing new appeared. So roughly two thirds of the fixpoint is re-scanning
declarations that were settled on the first pass.

The shape of the fix, for whoever takes it: after round one, re-scan only the
declarations that *failed* to place. That should make rounds two and three
nearly free and take discourse's assemble from 143 ms toward ~50 ms. It is a
change to the loop's bookkeeping, not to what it computes, which makes it
cheap to verify — the assembled namespace must be identical.


## Classifying the absent truth (session 21)

DEC-028 established that the residue cannot be reached by ranking. This asks
what the missing methods *are*. `script/absent.py` runs every gold site,
keeps the ones where trekr never names the true definition, and sorts them.

**1,104 of 2,987 gold sites**, by bucket:

| bucket | share | meaning |
| ------ | ----- | ------- |
| not-reached | 87.0 % | the definition is parseable from its file, and trekr did not answer with it |
| not-extracted | 10.0 % | the file is indexed and nothing trekr extracts sits at that line |
| unindexed-source | 3.0 % | Ruby's own stdlib, in no indexed checkout |
| core-stub | 0.1 % | answered from the vendored core stub |

`not-extracted` splits into 75 with no `def` and no shape we recognise, 27
`define_method`, and 8 delegation macros. `unindexed-source` is **entirely**
Ruby's stdlib — `set.rb`, `rubygems.rb` — which is a stated limit, not a TODO.

### The classifier had the same flaw it was built to find

`not-reached` was defined as "`--symbols` on the definition's file finds it".
But `--symbols` parses a file directly, independent of any tree — so the bucket
**conflates two different things**: a definition that is in this query's tree
and was not reached, and a definition whose *file belongs to a checkout this
query's tree does not contain*. Exactly the conflation this project has spent
three sessions hunting elsewhere, in a script written to hunt it.

Chasing one example settled which it mostly is.

### A query inside a gem sees only that gem

`delegate` at `actionpack-8.1.3.1/lib/action_controller/metal.rb:176` is
residue: *"the receiver's type is known but nothing in its ancestors defines
this name"*. The truth is `Module#delegate`, in **activesupport**.

* From the **rails checkout**, `--refs Module#delegate` finds the definition and
  **143 confirmed call sites**.
* The gold site is inside the *actionpack gem directory*, and that is the
  checkout the query resolves against (DEC-024, extended in session 15 so gem
  positions could be answered at all). A gem has no `Gemfile.lock`, so its tree
  is **that one gem plus core**. activesupport is not in it, and cannot be.

**So cross-gem resolution fails by construction**, and 2,924 of the 2,987 gold
sites are inside gem files.

### What this means for every gem number since session 12

The "gem floor" has been measuring trekr **configured as one gem at a time** —
a configuration no user is ever in. An agent working in an app that asks about
a gem file is asking from a checkout that resolves the whole bundle. The gem
floor is therefore a **lower** bound with an unknown amount of slack, and its
residue figures should not be read as coverage gaps until the context question
is settled.

The app sample cannot substitute yet: at 63 sites it is too small, and a third
of it is the Rails-generated bucket, which answers with the declaration rather
than the generator by design (session 15) and so registers as "absent" against
runtime truth however well it behaves.


## The gem floor, re-measured with gem context (session 22)

Session 21 found that a query inside a gem resolved against a tree of that one
gem plus Ruby core, so cross-gem methods were unreachable by construction —
and that 2,924 of the gold set's 2,987 sites are inside gem files. DEC-029's
fix answers a gem position from an app that resolves the gem. Same corpus,
same seed, same sample:

| | before | after |
| --- | --- | --- |
| gem correct | 38.2 % | **52.2 %** |
| gem found the definition | 64.0 % | **84.0 %** |
| gem confidently wrong | 3.8 % | 4.2 % |
| gem residue, nothing known | 16.0 % | **1.2 %** |
| gem residue, truth absent | 12.5 % | **8.0 %** |
| app code (unchanged) | 54.8 % correct | 54.8 % correct |

**Predicted correct 54 % (accept 50–58) and found 77 % (accept 73–81).** Correct
landed inside the range at 52.2 %; found came in *above* it at 84.0 %. So the fix
converted more residue into offered-and-found answers than predicted. Confidently
wrong rose 0.4 points and stayed under the 5 % guard — more context did not buy
confidence in answers that should still be declined.

Measured in two steps, because the first was not the whole fix: answering from
the owning app alone gave 48.8 % / 84.5 %, and reading the tree's gem roots from
the store rather than re-locating them per query gave the 52.2 % above. The
second half mattered because a query whose `GEM_HOME` differed from the index's
lost every gem — the same silent degradation one level down.

### How much of ten sessions' gem figures was artifact

Gem residue was 28.5 % (16.0 nothing-known + 12.5 truth-absent) and is now
9.2 %. **The artifact accounted for 19.3 of those 28.5 points — about two thirds
of what was being reported as a coverage gap.** Every gem figure published from
session 12 onward understated found-the-definition by roughly **20 points** and
correct by roughly **14**.

The app-code numbers are unaffected and always were: widget_shop already owned
its own bundle, so app sites had the whole tree all along. That is why the app
and gem columns are reported apart, and it is the reason the artifact survived
ten sessions — the half of the measurement that was sound never disagreed with
the half that was not.

### A ranking number that got worse for a good reason

Gem ranking quality fell — truth at #1 from 61.5 % to 49.6 %, MRR 0.743 to
0.648. It is not a regression. The candidate pool grew: residue-hit went from
25.8 % to 31.8 % of sites, so the *denominator* is 127 where it was 103. In
absolute terms more truths rank first than before (≈63 against ≈63) while far
more are offered at all.

It does mean DEC-028's rejected ranking features were measured against a pool
that was missing most of its competitors, and deserve re-measuring now that the
pool is real.
