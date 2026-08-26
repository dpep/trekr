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

**Done in session 22**, though not by the predicate suggested here. "Failed to
place" is not observable — `place` always returns *something*, and its guess for
an unknown prefix is a legitimate answer. What is observable is when it can
*change*: `place` reads `self.names` in exactly one case, a **compact path**
(`class A::B`) whose prefix goes through constant lookup. A declaration written
with plain names, in scopes with plain names, is placed by string arithmetic and
its round-one answer is final.

So the predicate shipped is "mentions `::` anywhere" — a deliberate
over-approximation, coarser than "could actually still move" and far easier to
be sure of. On discourse it revisits 9,100 of 69,305 declarations.

**Fixpoint 97 ms → 46 ms** on discourse (predicted ~60), **22 ms → 9 ms** on
rails; assemble 143 → ~100 ms and 34 → 22 ms. The namespace is byte-identical on
rails, discourse and widget_shop — checked by dumping every name with its kind,
alias and sites, before and after.


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
| gem correct | 38.2 % | **48.8 %** |
| gem found the definition | 64.0 % | **84.5 %** |
| gem confidently wrong | 3.8 % | **3.0 %** |
| gem residue, nothing known | 16.0 % | **1.2 %** |
| gem residue, truth absent | 12.5 % | **8.0 %** |
| app code (unchanged) | 54.8 % correct | 54.8 % correct |

**Predicted correct 54 % (accept 50–58) and found 77 % (accept 73–81).** Correct
landed just *below* the range at 48.8 %; found came in well *above* it at
84.5 %. So the fix
converted more residue into offered-and-found answers than predicted. Confidently
wrong rose 0.4 points and stayed under the 5 % guard — more context did not buy
confidence in answers that should still be declined.

**Corrected in session 23, and pinned in session 24.** The figures above are
measured with `--context` pinned to `widget_shop-nosorbet`, the app whose bundle
the TracePoint run executed, and reproduce to the decimal across a reindex in
reverse order. This table first read 52.2 % / 84.0 %, from a
single unpinned run. Re-measured twice on rebuilt indexes it reads **48.8 % / 84.5 %**,
and that is the figure to quote. The 52.2 % was not wrong at the time — it was
one draw of a measurement that moves with **which app owns each shared gem**,
and that ownership shifts with reindex order and with what each lockfile
resolves (DEC-029). Roughly three points of spread, on this corpus.

The lesson is not about the number. It is that the gem floor is now conditioned
on a *choice trekr makes*, so any future comparison has to hold that choice
fixed — the way the corpus, seed and sample are already held fixed. Reporting a
gem figure without saying which store produced it is reporting one draw.

### How much of ten sessions' gem figures was artifact

Measured over the whole gold set rather than the 400-site sample: sites where
trekr never names the true definition fell from **1,104 of 2,987 (37.0 %) to 445
(14.9 %)** — a 60 % reduction. Every gem figure published from session 12 onward
understated found-the-definition by roughly **20 points** and correct by roughly
**11**.

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

**Re-measured in session 23, and the figures move again — upward, and for the
same good reason.** Directory affinity, rejected at +1.6 points against the old
pool, delivers **+3.2** against the real one and ships. Gem ranking quality is
now **#1 52.8 %, top-3 69.3 %, MRR 0.666**, against 49.6 % / 68.5 % / 0.648
immediately after gem context and 61.5 % / 81.5 % / 0.743 before it.

So the headline ranking numbers have moved twice and neither move is drift:
they *fell* in session 22 because the denominator grew by a third (far more
truths are offered at all — residue-hit 25.8 % → 31.8 %), and they *rose* in
session 23 because a signal that had been measured against the wrong pool got
measured against the right one. Read them against the pool size, never alone.


## The residue that survives the artifact fix (session 23)

`script/absent.py` re-run against gem-context trees, whole gold set:

| bucket | session 21 | session 23 |
| ------ | ---------- | ---------- |
| **truth never named** | **1,104 of 2,987 (37.0 %)** | **445 (14.9 %)** |
| not-reached | 960 | 301 |
|  · receiver typed, chain complete, owner absent | 318 | 130 |
|  · receiver never typed, `implicit` | 264 | **7** |
|  · receiver typed, chain truncated | 79 | **0** |
|  · never typed — `other` / `local` / `?` / `ivar` / `const` | 249 | 137 |
|  · extracted, but at another line | 27 | 27 |
| not-extracted | 110 | **110** |
| unindexed-source (Ruby stdlib) | 33 | 33 |
| core-stub | 1 | 1 |

Truncated ancestor chains went to **zero** and never-typed implicit receivers
from 264 to 7 — both were gems missing the rest of their bundle. `not-extracted`
is unchanged at 110 *in absolute terms*, exactly as it must be: gem context
changes what a tree contains, not what the extractor reads. It is now 24.7 % of
a much smaller problem.

### `define_method` extraction: built, measured, not shipped

The 27 `define_method` sites looked like the tractable slice. Extracting them —
literal names only, with the block visited as a method body rather than a class
body — was built and measured, and **moved the gem sample not at all**: 48.8 %
correct with and without. Twenty-seven sites is under 1 % of 2,987, below what a
400-site sample can see.

Worth recording *how* that conclusion was nearly missed. The first measurement
appeared to show a 3.4-point **regression**, and the change was reverted on it.
Re-measuring the reverted build gave the same lower number — so the drop was
never the extractor at all. It was the ownership pick moving between reindexes
(above). A confounder that arrived on the same afternoon as the change, and
looked exactly like the change.

The rule that follows: a corpus-level A/B is only valid across builds if the
*store* is held fixed too. Rebuilding the index between arms silently changes an
input.

### Stated limits

* **Ruby's stdlib** (33 sites) — `set.rb`, `rubygems.rb`. In no indexed
  checkout, and indexing it is a setup question, not an engine one.
* **Methods with no knowable name** — `define_method(name)` over a variable, and
  the 75 sites with no `def` and no shape we recognise. A name that exists only
  at runtime is out of static reach, and inventing one is worse than the gap.
* **Monkey-patched core** — `require` really is Zeitwerk's `Kernel#require`.


## The owner-absent bucket, characterized (session 24)

"Receiver typed, chain complete, and the true owner is not in the chain" — the
largest remaining resolver bucket. Measured with the context pinned, whole gold
set: **269 sites.**

| what owns the method Ruby ran | sites | share |
| ----------------------------- | ----- | ----- |
| an ordinary module or class | 176 | 65.4 % |
| Ruby's `Kernel` | 35 | 13.0 % |
| the receiver's singleton class | 32 | 11.9 % |
| a concern's `ClassMethods` | 25 | 9.3 % |
| Ruby's `Object` | 1 | 0.4 % |

Top owners: `Kernel` (35), `ActiveRecord::QueryMethods` (23),
`ActiveRecord::Reflection::ThroughReflection` (11),
`Singleton::SingletonClassMethods` (9), `#<Class:ActiveRecord::Base>` (9).

**No slice here is both large and cheap**, which is the finding. The bucket is
not one mechanism, it is a long tail of them:

* **`Kernel` (35)** is almost entirely `require` — really Zeitwerk's or
  Bootsnap's replacement of `Kernel#require`. Monkey-patched core, already a
  stated limit.
* **`Concurrent::Map#delete`, `Set`, `Singleton`** and friends are *stdlib and
  concurrent-ruby internals* reached through instance variables typed to a
  framework class. The owner exists; the chain we build for the receiver does
  not include it, because the receiver's real class is decided at runtime.
* **`ActiveRecord::QueryMethods` (23)** and `CollectionProxy` are the relation
  chain — `Model.where(...).order(...)`, where each link's class is produced by
  a method call. DEC-020 declined to attack chained receivers on measured
  grounds, and this is that decision's bill arriving.
* **The singleton-class group (32)** and **`ClassMethods` (25)** are the same
  shape from two directions: a method installed on a class's singleton by an
  `included` hook or an `extend` that runs at load time.

The honest reading is that this bucket is what is left *after* the mechanical
wins, and it is dominated by things whose owner is only knowable by running the
program. Building for it would mean attacking chained receivers (declined,
DEC-020) or modelling `included` hooks' runtime effects — neither cheap, and
neither with a large enough slice to justify itself on these numbers.

**Stated as a limit**, not carried as a TODO.


## `workspaceSymbol` at 1.26 s — profiled, not fixed (session 24)

`--usage` put this at 1.26 s, the slowest operation an agent has. Profiled at
the SQL, against 508,991 definitions across 633 checkouts:

| query | matches | time |
| ----- | ------- | ---- |
| `%Widget%` | 93 | **1.15 s** |
| `%Account%` | 200 (capped) | 0.13 s |
| `%each%` | 200 (capped) | 0.11 s |
| `%new%` | 200 (capped) | 0.10 s |

**The intuition is inverted.** A query that *hits the limit* is fast, because
SQLite stops as soon as it has 200 rows. A query for a **rare** symbol is slow,
because proving there is no 94th match means reading all half-million names.
Rare symbols are precisely what an agent searches for when orienting, so the
p90 that matters is the slow one.

The cause is a leading-wildcard `LIKE`, which no B-tree index can serve, and a
plan that drives the join from `checkout` — every checkout, then every file,
then its definitions — rather than from the name. Stale statistics were part of
it and are now gathered after any index that read something (below), but they do
**not** fix this: the plan still starts at `checkout`, and the scan is the floor
regardless.

**This is a design question, not a bounded fix, and it is recorded rather than
attempted.** The honest options are a schema change — denormalising the
checkout root onto `def` so the name index can drive the join, or an FTS5 /
trigram index that can actually serve a substring search — and DEC-006 rules out
reaching for a planner override instead. Neither is an afternoon, and
`workspaceSymbol` is one call in thirty-five.

What did ship is the hygiene: `ANALYZE` now runs after an index that parsed
something. `PRAGMA optimize` on close only re-analyses a table whose size moved
since the *last analysis*, which never fired across 633 checkouts accumulated a
few at a time — the statistics were 13 rows old. That is DEC-006's own argument
applied to a database that had outgrown it.


## A real-app corpus: discourse (session 25)

widget_shop was written *for* this evaluation — 137 lines of shapes the receiver
ladder has a rung for. That is a fair test of the rungs and an unfair test of
the engine. Discourse is 1,247 app files and 224 service objects, written by
strangers for their own reasons, with no Sorbet: **9,146 app-scope gold sites**
against widget_shop's 63.

Both columns pinned to their own app as context from run one (session 24), both
traced with the same harness, 499 app sites sampled from discourse at seed 12.

| | widget_shop (built for this) | **discourse (real)** |
| --- | --- | --- |
| app sites available | 63 | **9,146** |
| correct | 54.8 % | **42.3 %** |
| found the definition | 64.5 % | **82.8 %** |
| confidently wrong | 4.8 % | **1.6 %** |
| residue, truth offered | 9.7 % | **40.5 %** |
| ranking: truth at #1 | 66.7 % | **87.1 %** (MRR 0.907) |
| gem floor, correct / found | 45.6 % / 86.0 % | 52.8 % / 88.0 % |

**Predicted correct 44 % (accept 38–50) — landed at 42.3 %, inside.** Predicted
found 70 % (accept 63–77) — beaten at 82.8 %. Predicted confidently wrong 7 %
(accept 4–10) — **wrong, and wrong in the safe direction**: 1.6 %, a third of
widget_shop's rate.

The directional call held: **correct falls and found rises** going from built-
for-the-test code to organic code. What that says is that a real app gives the
engine *more to work with* and *less to be sure about*. Residue where the truth
is offered goes 9.7 % → 40.5 %: discourse's call sites are chained receivers,
concern-installed methods and service objects whose types are decided at
runtime, so the ladder declines to commit — and then the ranker puts the right
answer first **87 %** of the time.

That combination is the product working as designed. A confident answer is right
98.4 % of the time on real code, and when it declines it still hands over a list
whose first entry is usually correct. widget_shop's higher `correct` was the
easier question, not the better engine.

### Two measurement rules this cost to learn

**A gold corpus is only valid against a complete index of the app it was traced
in.** The first discourse run scored **18.0 % correct / 47.3 % found / 19.8 %
right-owner-wrong-site**. Nothing was wrong with trekr: `--index` had never seen
**152 of discourse's 300 gems**, so half the running code was absent from the
tree. Reindexing moved it to 42.3 % / 82.8 % / 0 %. A `right-owner-wrong-site`
rate in double figures is the signature — the owner resolves, the line cannot.

**A tracer must not touch the objects it traces.** `tp.self.method(id)` looks
like a read and is not: an object that collects attributes through
`method_missing` — Fabrication's schematics, and any builder DSL like them —
records an attribute called `method`. It measured discourse into an
`unknown attribute 'method' for User` before it measured anything else.
Replacing it with `tp.defined_class.instance_method(id)` removed the mutation
and introduced a subtler error: both re-resolve, so a **prepended** module wins
and the recorded location is a file that is not running — 201 disagreements in a
200-event sample. The harness now uses `tp.path` and `tp.lineno`, which for a
`:call` event *are* the method being entered. Asking the event what it already
holds resolves nothing and disturbs nothing.

widget_shop's app column is byte-identical before and after that change, which
is what makes the two columns comparable.


## The declined receivers, characterized (session 26)

Discourse's largest bucket is **residue with the truth offered** — 40.5 % of app
sites, where the ladder declines and the ranker then puts the right answer first
87 % of the time. `script/absent.py` cannot see this population: it asks why the
truth is *missing*. `script/declined.py` asks the opposite question — what are
the receivers we decline on when the answer is already in hand — and prices each
slice before anything is built.

Whole corpus, **9,056 app sites scored, 5,029 declined**, context pinned to
discourse, measured against the store as it stood before this session's two
fixes:

| the receiver expression | sites | share |
| ----------------------- | ----- | ----- |
| implicit — no receiver written | 3,431 | 68.2 % |
| bare name — a local, a parameter or a call on self | 539 | 10.7 % |
| **chained call** | **364** | **7.2 %** |
| constant | 334 | 6.6 % |
| instance variable | 110 | 2.2 % |
| receiver on a previous line | 89 | 1.8 % |
| numeric literal | 59 | 1.2 % |
| everything else (literals, `[]`, `self`, globals) | 103 | 2.0 % |

| what owns the method Ruby ran | sites | share |
| ----------------------------- | ----- | ----- |
| an ordinary class or module | 2,844 | 56.6 % |
| a singleton class | 1,042 | 20.7 % |
| a concern's `ClassMethods` | 729 | 14.5 % |
| Ruby's `Object` | 301 | 6.0 % |
| Ruby's `Kernel` | 112 | 2.2 % |

Truth lives in the app for 52.5 % of them and in a gem for 47.0 %. Only **3 of
5,029** have a truncated ancestor chain, so this is not a coverage gap wearing a
lookup gap's clothes.

### DEC-020 is not overturned, and its bill is a third of what was claimed

Session 25 read the 40.5 % as "chained receivers, concern-installed methods and
service objects". Two of those three are right. **Chained receivers are 7.2 % of
the declined population, not the bulk of it** — 364 sites out of 5,029, where
promoting the top candidate would be right 28 % of the time. DEC-020 stands, and
the price tag it has carried since the ruby-lsp head-to-head ("1 in 3 positions")
is a figure about a *sample of positions chosen to include them*, not about what
real app code declines on.

### Promotion is not the rung — the ceiling says so

If a rung promoted the top candidate for the dominant slice, 2,634 sites become
`correct` and **797 become confidently wrong**: 76.8 % precision, which on this
corpus would take confidently-wrong from 1.6 % to roughly 10 %. The product's
whole claim is that a confident answer is right 98 % of the time. So the useful
question is never "which slice ranks well" but "which slice has a *mechanism* we
can model", and the ranked list stays the honest answer for the rest.

### The mechanisms, named, from a 2,500-site subsample

The same classification with every row kept (1,401 declined), grouped by the
owner Ruby actually dispatched to:

| mechanism | declines | share | reach |
| --------- | -------- | ----- | ----- |
| `class_methods do … include StepsHelpers` — discourse's service DSL | 396 | 28.3 % | **modelled this session** |
| `params do … end`, class_eval'd into an anonymous `ContractBase` subclass | 172 | 12.3 % | out of static reach — the receiver is a runtime-created anonymous class |
| ActiveModel validation macros | 98 | 7.0 % | partly the above, partly fixed by the mixin rule below |
| `SiteSetting.foo` | 92 | 6.6 % | out of reach — `define_method` over a YAML settings list |
| `before_action` family | 77 | 5.5 % | out of reach — `define_method` over a computed name |
| Rails-generated attribute and association readers | 78 | 5.6 % | answered with the declaration by design (session 15) |
| `Kernel#require` | 40 | 2.9 % | stated limit — Zeitwerk and Bootsnap replace it |

**Half of the declined population is a handful of named mechanisms, and most of
them are honestly out of static reach.** A name that exists only after a YAML
file is read, or only inside a block `class_eval`'d into a class created at
runtime, is not something a parser can be improved into finding. What is left
after those is the first row, and it was worth building.


## Two extractor rules, measured (session 26)

Both arms are discourse app code, 498 scored sites, seed 12, context pinned to
discourse. The store is rebuilt between arms because a schema bump forces it
(DEC-009), so each arm is also checked **site by site** against the one before —
a corpus-level total cannot tell a fix from a store difference (session 23).

| | baseline | + mixin rule | + `class_methods` |
| --- | --- | --- | --- |
| correct | 42.0 % | 43.4 % | **59.2 %** |
| found the definition | 82.5 % | 84.9 % | 84.5 % |
| confidently wrong | 1.6 % | **0.2 %** | 0.6 % |
| residue, truth offered | 40.6 % | 41.6 % | **25.3 %** |
| ranking: truth at #1 | 87.1 % | 87.9 % | 80.2 % (MRR 0.855) |
| gem correct / found / wrong | 51.5 / 84.6 / 4.0 % | 51.5 / 86.0 / 4.0 % | 51.5 / 86.0 / 4.0 % |

### A mixin inside a method invents an ancestor

`include Extra` written inside a `def` runs when the method runs, against
whatever `self` is then. Recorded against the lexically enclosing scope it does
not merely miss an edge — it invents one, and an invented edge wins lookups.

Rails writes `include ActiveModel::Validations` inside `has_secure_password`, in
a `ClassMethods` body. That single line put the module's instance methods into
the class-level lookup chain of every ActiveRecord model, so a class-body
`validate :thing` resolved — confidently — to `alias_method :validate, :valid?`
instead of `ClassMethods#validate`.

**Predicted correct 43.2 % (accept 42.2–44.4), wrong 0.4 % (accept 0.0–0.8),
found 83.7 % (accept 82.5–85.0). All three inside range**, with `found` at the
top of its range: the invented edge was swallowing residue candidates as well as
producing wrong answers.

Site by site against the baseline run: **12 sites fixed, 0 newly broken, 0
changed verdict for another reason.** Seven of the eight confidently-wrong app
sites were this one shape.

### `class_methods do` is the block form of `module ClassMethods`

`ActiveSupport::Concern` creates `M::ClassMethods` from either form and extends
it into every includer. Session 13 recorded the methods inside the block without
the module and pinned that as deliberate (testbed 010); the classification above
is what it costs. Discourse's `Service::Base` writes

```ruby
class_methods do
  include StepsHelpers
  def call(context = {}, &actions) …
end
```

so `step`, `model`, `policy`, `params` and `only_if` — the whole surface of 224
service objects — were instance methods of the concern, where a class-body call
cannot reach them. **396 of 1,401 declined sites (28.3 %) are that one shape**,
and the truth ranked first in **100 %** of them, which is what made it worth
building rather than promoting.

**Predicted correct 52–61 % — landed at 59.2 %, near the top. Predicted found
84–89 % — 84.5 %, just inside and slightly *down*. Predicted ranking #1 70–85 %
— 80.2 %, and the fall is the point**: the population that left the residue is
the one that ranked first every time, so the remaining pool is harder by
construction. Read it against the denominator, which went 207 offered → 126.

**Confidently wrong 0.2 % → 0.6 %, against a bar of ≤ 0.7 % set before the
run.** Both new sites are the same shape and worth naming: a call inside
`StepsHelpers` itself, where the `includer` rung now has five candidate includers
instead of one and promotes the wrong one at **confidence 0.2**. Widening a
module's includer set is exactly what this change does, so the rung's weakest
case got more exercise. That is DEC-027's rule — a convention-based pick among
competitors is `ambiguous`, not `resolved` — never having been applied to this
rung.

Site by site against the previous arm: **0 sites newly missed except those two,
0 verdicts changed for another reason.**


## `workspaceSymbol`: the read-side finding was a measurement artifact (session 26)

Session 24 profiled this and concluded that **rare** symbols are slow and common
ones fast — "the intuition is inverted", because proving there is no 94th match
means reading all half a million names. The write-side cost of the remedy it
named was this session's second task. Measuring it first meant re-measuring the
motivation, and the motivation does not survive.

Four queries, each run **first in a fresh process** against the same store
(509,151 definitions, 634 checkouts, statistics current):

| query, run first | matches | wall |
| ---------------- | ------- | ---- |
| `%each%` | 200 (capped) | **0.78 s** |
| `%Widget%` | 93 | 0.10 s |
| `%zzznope%` | 0 | 0.10 s |
| `%Account%` | 200 (capped) | 0.12 s |

**Whichever query runs first pays ~0.7–1.1 s; every query after it costs ~0.10 s,
rare or common, hit or miss.** Session 24's table put `%Widget%` in the first
slot and read its cold-cache second as selectivity. The plan is unchanged — it
still drives from `checkout`, and a leading-wildcard `LIKE` is still a scan — but
the scan costs 0.10 s warm, not 1.15 s.

That also re-reads the `--usage` figure that started it: `workspaceSymbol` at
1.26 s is a **session's first request**, which is the same cold start every
operation pays and which `--usage` now reports separately.

### The denormalisation, priced anyway

Prototyped on a copy of the store rather than shipped: one `def_search` row per
(definition, file) with the checkout root and path carried on it, and a name
index.

| | |
| --- | --- |
| rows | 601,623, against 509,151 `def` rows — **1.18×** |
| … on the 785-checkout store this session started with | 696,562 — **1.37×** |
| database | 392 MB → 478 MB, **+22 %** |
| population | 1.31 s for 601k rows (**458k rows/s**), + 0.26 s to index the name |
| marginal cost of one discourse index (~77k definitions) | **~0.17 s on a 2.9 s index, +6 %** |
| warm query | 0.10 s → **0.036–0.045 s**, a 2.6× on a scan that stays a scan |

**Two things make this a bad trade, and the second is structural.**

The win is ~60 ms on a warm query, bought with a fifth more rows and a fifth
more disk. Against session 24's premise — 1.15 s — it was a different
proposition.

And the phrasing "denormalise `checkout.root` onto `def`" cannot be implemented
as written. `def` is keyed by blob, a blob is shared by every checkout that
contains those bytes, and ARCHITECTURE's layer-1 rule says nothing below `blob`
may mention a path or a checkout — *"N worktrees of one repo cost one index"*.
Carrying a root means one row per (definition, checkout), which is precisely the
sharing being given up: the blow-up factor measured **1.18× on a 634-checkout
store and 1.37× on the 785-checkout one**, growing with exactly the sharing the
design exists to exploit.

**Recommendation to session 27: do not build it.** If substring search ever does
need to be fast, the honest instrument is an FTS5 or trigram index over the
**168,718 distinct names** — a third of the rows, and no second place where a
path lives.


### The rung that got more exercise, and DEC-027 applied to it

`class_methods do` widened every concern's includer set, which gave the
`includer` rung — *"a call inside a module is answered by asking the classes
that mix it in"* — five candidates where it had one, and it promoted at
confidence **0.2** while reporting `resolved`. DEC-027 had settled that a pick
among competitors is `ambiguous`; the rule had only ever been applied to the
receiver-name rung.

The scorer gained the matching verdict in the same change. `confidently wrong`
has always meant *resolved, and pointed elsewhere*, and an `ambiguous` answer
was being counted in it — a different failure, and not the one the product's
headline claim is about.

| | + `class_methods` | + `ambiguous` on the includer rung |
| --- | --- | --- |
| app correct | 59.2 % | 59.2 % |
| app found | 84.5 % | 84.5 % |
| app **confidently** wrong | 0.6 % | **0.2 %** |
| app ambiguous-wrong | — | 0.4 % |
| gem confidently wrong | 4.0 % | **3.3 %** |
| gem ambiguous-wrong | — | 0.7 % |

`correct` and `found` are identical to the decimal on both columns, which is the
check that a disclosure change disclosed and nothing else.

### Canonical, from here on

**discourse app code, 498 scored sites, seed 12, context pinned to discourse:
59.2 % correct / 84.5 % found / 0.2 % confidently wrong / 0.4 % ambiguous-wrong
/ 25.3 % residue-with-truth-offered / truth-at-#1 80.2 % (MRR 0.855).** Gem
floor from the same run, same pin: 51.5 % correct / 86.0 % found / 3.3 %
confidently wrong.

Two consecutive runs on the untouched store agree to the decimal, as the
session-24 rule requires. The gold set is a fresh trace of the same exerciser —
9,146 app sites, the same count as session 25 — so the 42.3 % that column
carried is directly comparable to the 42.0 % this session started from.

### The widget_shop gem floor, re-measured — and why it is not a comparison

The pinned gem floor was 48.8 % correct / 84.5 % found / 3.0 % confidently wrong
(session 24, context `widget_shop-nosorbet`). Re-traced and re-scored on this
session's store, 398 gem sites, same seed, same pin, two consecutive runs
byte-identical:

**46.2 % correct / 85.9 % found / 3.5 % confidently wrong (+ 0.5 %
ambiguous-wrong).** App column 54.1 % correct on 61 sites, against 54.8 %.

**That 2.6-point fall is not attributable to this session's changes, and saying
otherwise would break the session-23 rule.** Nothing was held fixed between the
two figures: the gold set is a fresh trace, the store was rebuilt twice by
schema bumps, and the checkout population went 785 → 634 as the rebuild dropped
gems no current corpus resolves.

What *is* controlled says the changes did nothing here. Discourse's gem column
was measured on one fixed gold set across all three arms and reads **51.5 %
correct at every step** — baseline, after the mixin rule, after `class_methods`.
Neither extractor change moved a single gem site. Both are about class-body
macro calls and `ClassMethods` modules, which is app-code grammar.

So the honest statement is that **the widget_shop-pinned floor now reads
46.2 / 85.9 / 3.5 on a store nobody can compare to the one that produced 48.8**,
and that a controlled re-measurement of it — pre-change binary, this store, this
trace — is a job for session 27 if the number matters. The discourse column is
the better instrument regardless: 398 sites of one small app's bundle against
2,989 of a real one's.


## The declined receivers, re-measured after the fixes (session 27)

Session 26's classification was of the store *before* its own two extractor
rules landed, and 28 % of that table was the mechanism it then removed. Re-run
on the post-fix store, same corpus, same pin, whole 9,056 app sites:

**Declined sites fell 5,029 → 3,566, down 29.1 %**, which is the shape the
`class_methods` measurement predicted from the other direction.

| the receiver expression | pre-fix | post-fix | share now |
| ----------------------- | ------- | -------- | --------- |
| implicit — no receiver written | 3,431 | 1,973 | 55.3 % |
| bare name — a local, a parameter or a call on self | 539 | 538 | 15.1 % |
| chained call | 364 | **364** | 10.2 % |
| constant | 334 | 330 | 9.3 % |
| instance variable | 110 | 110 | 3.1 % |
| everything else | 251 | 251 | 7.0 % |

**Every row except the first is unchanged in absolute terms.** The fix removed
one mechanism and touched nothing else, which is the check that it did what it
claimed. Chained receivers are *the same 364 sites* and now 10.2 % of a smaller
problem — DEC-020's bill did not grow, the denominator shrank.

Where the truth lives inverted: **app 52.5 % → 33.0 %, gem 47.0 % → 66.3 %.**
What is left is mostly other people's code.

### The mechanisms that remain

Grouped by the owner Ruby dispatched to, over all 3,566:

| mechanism | sites | share | truth at #1 | reach |
| --------- | ----- | ----- | ----------- | ----- |
| `params do` — a block `class_eval`'d into an anonymous `ContractBase` subclass | 879 | 24.6 % | ~97 % | **out of reach** — the receiver is a class created at runtime |
| core / ActiveSupport core-ext on an untyped receiver (`present?`, `presence`, `blank?`) | 678 | 19.0 % | high | needs receiver typing, not extraction |
| **computed-name `define_method`** — `before_action` family, `define_model_callbacks`, Sidekiq | 368 | 10.3 % | **12 %** | **the candidate** |
| Rails-generated attribute and association readers | 320 | 9.0 % | — | answered with the declaration by design (session 15) |
| `SiteSetting.foo` | 296 | 8.3 % | 0 % | out of reach — `define_method` over a YAML settings list |
| everything else | 1,025 | 28.8 % | — | the long tail |

Five mechanisms are **71.3 %** of what remains.

`Service::Base::StepsHelpers`, which was 28.3 % of the pre-fix declines and the
largest single entry, does not appear in the post-fix top fifteen at all.

### The lead for session 28, with its post-fix number

**Computed-name `define_method`: 368 sites, 10.3 % of declines.** Actionpack
writes `[:before, :around, :after].each { |c| define_method("#{c}_action") … }`
and ActiveRecord's `define_model_callbacks` is the same shape — the name is
computed, but from a **literal array**, which a parser can read.

What makes it the candidate rather than another 10 % slice is the third column:
**the truth is offered for only 32 % of them and ranks first for 12 %.** Every
other large slice is one the ranker already nails, where the engine's list is
useful even though it declines. This one is the slice where trekr hands over
*nothing* — it is the bulk of the `residue-nothing-known` bucket, the only
bucket with no consolation prize. Extraction would create answers rather than
promote them, which is also why it cannot be measured by the promotion ceiling
that governs the other slices.

Session 23 built literal-name `define_method` extraction and shelved it for
moving nothing; the names here are computed, which is precisely the gap that
measurement left open.


## Computed method names, extracted (session 28)

### First, a correction to session 27's count

That session sized this slice at **368 sites, 10.3 % of declines**, by grouping
declined sites on the **owner** Ruby dispatched to. Grouping by owner bundled
three mechanisms that need three different things. By the truth's *file and
line*:

| | sites | trekr today |
| --- | ----- | ----------- |
| actionpack `callbacks.rb:231` / `:245` — `define_method "#{callback}_action"` | **250** | offered **0**, #1 **0** |
| activemodel `callbacks.rb:55` / `:88`, sidekiq `job.rb:359` — **plain `def`s** | 118 | offered 118, 44 at #1 |

Only the first is an extraction gap. The other 118 are already offered and
mostly ranked; they are a typing problem wearing an owner's name. **The honest
target was 250 sites, 7.0 % of declines** — and, unusually, **38.3 % of the
653-site `residue-nothing-known` bucket**, the one place where trekr hands over
nothing at all.

That is what made it worth building over larger slices: everywhere else the
ranker already puts the truth first and the engine's list is useful even when it
declines. Here there is no list.

### The measurement

Discourse app code, 498 sites, seed 12, context pinned, two consecutive runs.

| | baseline | predicted | accept | **actual** |
| --- | --- | --- | --- | --- |
| `residue-nothing-known` | 6.4 % | 3.2 % | 2.6–4.0 | **3.2 %** |
| correct | 59.2 % | 62.4 % | 61.0–63.6 | **62.4 %** |
| found the definition | 84.5 % | 87.7 % | 86.3–88.9 | **87.8 %** |
| confidently wrong | 0.2 % | ≤ 0.4 % (hard bar) | — | **0.2 %** |
| ranking: truth at #1 | 80.2 % | ~80 % | 78–83 | **80.2 %** |
| gem correct / found / wrong | 51.5 / 86.0 / 3.3 % | unchanged | ±2.0 | **51.5 / 86.0 / 3.3 %** |

**Three of them landed on the predicted decimal**, which is what a coverage gap
with a counted population should do — unlike a ranking feature, the arithmetic
is knowable in advance: 16 sites in the sample, all currently answering nothing.

Site by site against the previous run: **16 sites fixed, all of them
`before_action` (10) or `skip_before_action` (6), 0 newly broken, 0 verdicts
changed for another reason.** The gem column is byte-identical.

`before_action` in a discourse controller now answers
`abstract_controller/callbacks.rb:231`, owner
`AbstractController::Callbacks::ClassMethods`, `resolved · confidence 1` — the
file and line runtime truth reports.

### The blast radius is 297 rows

Across 634 checkouts, definitions went **509,151 → 509,448**. The scope is
deliberately the narrowest thing that covers the shape: a literal array where
*every* element is a literal, `each`, exactly one required block parameter, and
a name whose only interpolation is a bare read of that parameter. `CONST.each`,
`"#{a}_#{b}"`, `"#{n.to_s}"`, and a `define_method` inside a `def` (DEC-031's
rule again) all generate nothing.

That last set is the half of testbed 016 that matters. **A name half-guessed is
worse than a name not offered**, because a lookup finds it and stops — the same
argument as DEC-031's invented ancestor, one layer down.

### An operational near-miss worth recording

The first reindex after the schema bump reported **`0 parsed`** for every
corpus. The bump was in the source; the *release binary* had been built before
it, so `store::init` compared 14 against 14, found no mismatch, kept every blob
as "already known" — and the measurement would have scored the **old** extractor
against a store that looked freshly built.

This is DEC-013's exact failure mode ("a stale cache that looks fresh is worse
than a slow one") arriving from the operations side rather than the code side.
The tell is free and should be looked for every time: **a reindex that follows a
schema bump and parses nothing has not happened.**

### What this does not reach, and the lead it leaves

ActiveModel's model callbacks — `after_save`, `after_destroy`, `after_update`,
112 sites and now the largest remaining block of `residue-nothing-known` — are a
different mechanism and correctly out of scope here. They are written

```ruby
def _define_after_model_callback(klass, callback)
  klass.define_singleton_method("after_#{callback}") do |*args, …|
```

which three separate guards reject: the receiver is a parameter rather than
`self`, the call is inside a `def`, and `callback` is a parameter rather than a
bound literal. Nothing about the defining file states those names.

What *does* state them is the **call site**: activerecord's
`define_model_callbacks :save, :create, :update, :destroy`. That is the shape
trekr already models for `belongs_to`, `enum` and the rest — an entry in the
macro expansion table, whose arguments name the methods it implies. One wrinkle
for whoever takes it: the call sits inside an `included do` block, so the owner
the extractor records is the concern, while the methods land on every includer's
**singleton**.

### The distribution after it, and the next candidate — counted properly this time

`script/declined.py` re-run on the post-change store, whole 9,056 app sites:

| | before | after |
| --- | ---: | ---: |
| declined app sites | 3,566 | **3,316** |
| `residue-nothing-known` | 653 | **403** (−38.3 %) |
| sites whose truth is `callbacks.rb:231`/`:245` | 250 | **0** |

Every other receiver-expression row is unchanged in absolute terms again —
`chained call` is still the same 364 sites, now 11.0 % of a smaller problem.

What is left in the bucket with no candidate at all is dominated by one thing:

| truth | sites | share of the 403 |
| ----- | ----- | ---------------- |
| `site_setting_extension.rb` (three lines) | 274 | **68 %** |
| `attribute_methods.rb:273` | 85 | 21 % |
| everything else | 44 | 11 % |

Both are heredoc `class_eval` or a `define_method` over a YAML settings list:
Rails writes `def #{name}` **inside a Ruby string**, and discourse's site
settings do not exist until a YAML file is read. Neither is reachable by reading
`define_method` calls.

**The session-29 candidate, and its number checked the way session 27's was
not.** ActiveModel's model callbacks — `after_save`, `before_save`,
`after_destroy` and kin — are **114 declined sites where the truth is never
named** (offered 0, ranked first 0; 17 of them offer nothing at all). Session
28's earlier note said 112 from a looser count; 114 is the figure from grouping
on the truth's file and line, which is the grouping that survived this session.

They are written `klass.define_singleton_method("after_#{callback}")` inside a
`def`, which this session's three guards correctly reject. What states the names
is the **call** — `define_model_callbacks :save, :create, :update, :destroy` —
which is the macro-expansion shape trekr already models for `belongs_to` and
`enum`. The wrinkle for whoever takes it: that call sits inside an `included do`
block, so the extractor's recorded owner is the concern while the methods land
on every includer's singleton.

Checked while in the extractor, and reported so nobody re-checks it: an
interpolated `attr_reader` / `alias_method` would reach **none** of this
population. The remaining extraction-shaped gaps are the two above; the rest of
the declines are plain `def`s that trekr already extracts and offers, which
makes them typing and ranking problems rather than coverage ones.


## The state of the residue (session 29)

Read this instead of the session history if you are picking the project up.

On discourse's 9,056 app-scope gold sites, trekr **resolves 62.4 % to the exact
line Ruby ran**, names the definition one way or another **87.8 %** of the time,
and is **confidently wrong 0.2 %** — one site in five hundred. It declines on
3,316 sites, and for two thirds of those it still hands over a ranked list whose
first entry is right 80 % of the time.

What is left divides into four, and only the first is engineering:

**1. Typing and ranking on ordinary methods — the majority.** The definition is
extracted, indexed, and offered; trekr will not commit because the receiver's
type is not settled. Chained relations (`Model.where(…).order(…)`), ivars filled
from untyped constructor parameters, block parameters. These are the ladder's
limit, not the extractor's. DEC-020 declined to attack chained receivers on
measured grounds and the re-measures since have not overturned it — they are
**364 sites, 11 % of declines**, and promoting their top candidate would be
right 29 % of the time.

**2. Names that do not exist until something runs — a stated limit.**
Discourse's `SiteSetting.foo` (274 sites, 68 % of the bucket where trekr offers
nothing) comes from a `define_method` over a YAML file. Rails' attribute methods
(85 sites) come from `def #{name}` inside a heredoc `class_eval`. Neither is
reachable by reading source, and inventing the names would be worse than the
gap.

**3. Answers where "what runs" and "what a reader wants" differ.** Rails
generates a method; runtime truth points at the `define_method` that made it,
and trekr points at the `belongs_to`, the `enum` or the schema column — which is
the answer a person wants. Reported in its own bucket since session 15 rather
than blended. **DEC-033 is where this bucket stopped being free**: the same
shape in a *gem* would have converted 112 declines into 112 confidently wrong
answers, because nothing in the response says "this is a declaration, not the
line that ran". Teaching the answer to say so is the open idea.

**4. Monkey-patched core.** `require` really is Zeitwerk's. Stated limit since
session 12.

**The extraction-gap arc is essentially finished.** Sessions 26–28 closed the
three mechanisms worth closing — an invented ancestor, `class_methods do`, and
computed names over a literal array — and moved `correct` on real app code from
42.0 % to 62.4 % with `confidently wrong` *falling* from 1.6 % to 0.2 %. Session
29 looked for a fourth and found that it costs more than it buys. What remains
is receiver typing, disclosure, and two limits that are honest to state.
