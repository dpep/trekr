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

**On the version.** PLAN §8's whole read on the other engines is against ruby-lsp 0.27,
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

**Both columns matter.** On a partially-typed monorepo — the design point, say
~30 % Sorbet-covered —
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


## Dead-code candidates, validated against git history (session 36)

DEC-037 predicted **60–75 %** precision for the `unreferenced` tier and set the
bar at 70 %. Measured, and the number that matters is not the one predicted.

**Method.** A discourse worktree at a commit **12 months old** (2025-08-26),
indexed on its own, `--dead app/models app/services` → **1,297 candidates**.
Ground truth: is a method of that name still defined anywhere in today's
checkout? Then the control nobody would have thought to run without it — the
same question asked of **400 randomly sampled methods from the same scope**,
candidate or not.

| tier | candidates | deleted since | vs base rate |
| --- | ---: | ---: | ---: |
| `unreferenced` | 248 | 19.8 % | **1.04×** |
| `convention-only` | 368 | 3.3 % | **0.17×** |
| `single-caller` | 681 | 25.3 % | 1.33× |
| **base rate (random methods)** | 400 | **19.0 %** | 1.00× |

### `unreferenced` has no measurable lift, and that is the finding

19.8 % against a 19.0 % base rate. Flagging a method `unreferenced` in this
corpus tells you **almost nothing** about whether a human will delete it. The
predicted 60–75 % was wrong by a wide margin, and it was wrong in the direction
that matters — toward overclaiming.

Two reasons, and the second is a limit rather than a bug. Discourse's models and
services are called from ERB templates, serializers and specs that a Ruby-only
engine does not index. And humans delete code when a *feature* is removed, not
when a method becomes unreachable — so 12-month deletion is a weak proxy for
deadness in the first place, and a genuinely dead method that nobody got around
to deleting scores here as a false positive.

**What the control changes.** Without it, 19.8 % reads as a poor but real
signal. With it, it reads as no signal at all. The base-rate run cost one
command and it is the difference between shipping a claim and shipping a
disclosure.

### `convention-only` is strong, in the direction nobody was looking

**3.3 % deleted against a 19.0 % base — those methods survive nearly six times
more often than average.** The tier is not finding dead code; it is finding
*genuinely used* code that looks dead to a naive search, which is exactly what
the symbol-reference fact was built to do. It validates the prerequisite
independently of whether the dead-code feature ever earns its keep.

### What this means for what shipped

Per DEC-038's reverses-if, `--dead` ships as **disclosure only**. The tiers stay
advisory, the output keeps saying what was looked for rather than what is dead,
and no precision claim is made anywhere. `single-caller` at 1.33× is the tier
with the best case, and it is the one Daniel asked for by name — an inlining
candidate is a judgement a human makes, and one caller is a fact rather than a
prediction.

**What would make `unreferenced` worth trusting**: indexing the call sites it
cannot see. ERB templates and the spec suite are where the missing references
are, and until those are read, "no references found" in a Rails app is a
statement about trekr's inputs at least as much as about the user's code.

## Why the chain misses a known owner (session 34)

Session 33 called this the largest thing left — "1,612 sites where the receiver
is resolved and the truth's owner is not in its chain" — and made it the lead.
**That number was wrong, and wrong in a way worth naming**: it was inferred from
the receiver's *expression shape* (implicit or `self`), never by asking whether
the owner was actually in the chain. Asked properly — build the receiver's
chain, look for the owner in it — the population is **492 sites, 15 % of the
3,202 declines**, not 50 %.

Same error as session 31's "`present?` ranks first ~99 %" (measured: 51.9 %):
a bucket labelled by proxy and then quoted as if measured.

`script/chainmiss.py` asks it directly. The first cut decides the rest — *is the
owner in the index at all?* — because a missing owner is coverage and a present
one with no path is ancestry.

| why the chain misses it | sites | share | offers nothing |
| --- | ---: | ---: | ---: |
| owner **is** in the chain; the method is not on it (singleton) | 323 | 65.7 % | 288 |
| owner known, no edge to it (plain) | 134 | 27.2 % | 54 |
| owner known, no edge to it (class-methods) | 24 | 4.9 % | 0 |
| owner absent from the index (plain) | 7 | 1.4 % | 0 |
| everything else | 4 | 0.8 % | 2 |

**The dominant bucket is not an ancestry gap at all.** In 323 sites the owner is
reachable and the *method* was never extracted from it. By what generated the
method:

| | sites | |
| --- | ---: | --- |
| `site_setting_extension.rb` | 291 | `define_method` over a YAML settings list — **stated limit** |
| `enum.rb:233` | 16 | the enum mapping accessor — **fixed this session** |
| `discourse_plugin_registry.rb` | 11 | an app's own registry DSL |
| `global_setting.rb` | 4 | as above |

The 134-site "no edge" bucket is **entirely `X::GeneratedAttributeMethods`** —
Rails' per-model attribute module, built at runtime by `class_eval` over a
heredoc. trekr models those from `db/schema.rb` instead (DEC-022), so a miss
here means the column is not in the schema file, not that an edge is missing.
The 24 class-method sites are all `ActiveModel::Validations::ClassMethods`
reached from a service contract — the `params do` anonymous-class limit recorded
in session 28.

### What was built, and what is stated

**Nothing here is both large and cheap**, which is the finding. The only
tractable slice was 16 sites, and it was worth taking because it completed a
macro we already own rather than adding a mechanism: `enum` generated the
members' predicates and never the attribute's mapping accessor.

**Checking a bucket by hand is what found it.** The classifier's own label —
"owner is in the chain, the method is not on it" — is an *extraction* gap
wearing an ancestry gap's clothes, and reading three sites turned a 323-site
"ancestry" bucket into 291 sites of a known limit plus one fixable macro.

The classifier also had a bug of exactly the kind it exists to catch: its
singleton regex captured `ActiveRecord::Base>` with the closing bracket, so
every such owner looked *absent from the index*. That bucket was an artifact of
the instrument until the sample was read.

## The typing ceiling, re-keyed (session 33)

Session 31 proposed re-keying the declined population on *what would have to be
known* to type the receiver, rather than on what the receiver looks like. Done,
over the 3,202 declined discourse app sites that remain once the extraction work
of sessions 28–32 is set aside. Promotion precision is what the slice would
score if its top candidate were promoted outright.

| what would have to be known | sites | share | precision |
| --- | ---: | ---: | ---: |
| **nothing — the receiver is known, the owner is not in its chain** | 1,612 | **50.3 %** | 75.6 % |
| a parameter type, or a return type for a self-call | 538 | 16.8 % | 30.1 % |
| a return type for the previous link (chained) | 502 | 15.7 % | 26.1 % |
| nothing — a constant receiver is already typed | 330 | 10.3 % | 3.9 % |
| an ivar's type, assigned in another method | 113 | 3.5 % | 5.3 % |
| a literal receiver is typed; the owner is elsewhere | 59 | 1.8 % | 33.9 % |
| other | 48 | 1.5 % | 16.7 % |

**The headline corrects the premise.** *Half the declined population needs no
typing at all.* In 1,612 sites the receiver is already known and the method
simply is not in its ancestor chain — a coverage question, not an inference one.
Add the 330 constant receivers and **60 % of what looks like a typing frontier
is not one**. Genuine typing gaps — parameters, return types, cross-method
ivars — are **36 %** of the declines, and the two big ones sit at 26–30 %
promotion precision, which is nowhere near shippable.

### The `present?` caution, now with a number — and it is worse than I said

Session 31 flagged that promoting core-extension receivers would run into
DEC-027, since `present?` is defined on `Object`, `NilClass`, `String` and
`Array` at once. It also guessed the truth ranked first "~99 %" of the time for
the `Object`-owned subset, and used that to call the slice tempting.

Measured over the whole family — 680 sites, the receiver being core or an
ActiveSupport core extension — **the truth ranks first 51.9 % of the time**. The
owners really are spread: `Object` 301, `Kernel` 112, `NilClass` 94, `Numeric`
70, `String` 54, `Array` 22. Promoting them would be wrong about half the time,
not occasionally.

So the caution stands and hardens: this slice cannot be promoted to `resolved`,
and by DEC-027 it could only ever be `ambiguous` — which the gold scorer already
counts alongside residue, so it would move disclosure and not `correct`. The
largest names in it are `Fabricator` (280, a spec DSL defined at top level),
`present?` (132), and `require` (112, the monkey-patched-core limit stated since
session 12).

**Read this before proposing a typing rung.** The frontier is real but smaller
than its reputation, and the part with the best ranking is the part the language
forbids us to commit to.

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


## Disclosure, and what it unlocked (session 30)

**Canonical, discourse app code, 498 scored sites, seed 12, context pinned, two
consecutive runs identical:**

| | session 28 | **session 30** |
| --- | ---: | ---: |
| correct | 62.4 % | **62.4 %** |
| found the definition | 87.8 % | **87.8 %** |
| confidently wrong | 0.2 % | **0.2 %** |
| ambiguous-wrong | 0.4 % | 0.4 % |
| declaration | 1.4 % | **2.0 %** |
| declaration-offered | 2.6 % | **2.8 %** |
| residue-truth-absent | 4.2 % | **3.6 %** |
| residue-nothing-known | 3.2 % | **3.0 %** |
| gem correct / found / wrong | 51.5 / 86.0 / 3.3 % | **51.8 / 86.0 / 3.3 %** |

Nothing moved that measures whether trekr is *right*. What moved is how much of
what it says is legible: 3 more sites answered as declarations, 3 fewer as
residue.

**The number worth pulling out**: on the 100 sample sites whose truth is a
**generated** method — a `define_method`, a heredoc `class_eval`, a macro —
`confidently wrong` is **0.0 %**. Every answer there is correct, a disclosed
declaration, or an honest residue. That whole class of site used to be where the
engine's answers and the scorer's categories disagreed most.

### What the disclosure is

The store has always recorded *what made* a method — a `def` row's `via` names
the macro — and the answer never said. So a caller could not tell `belongs_to
:supplier`, the line a reader wants, from the line Ruby runs. `--def` and
`--explain` now carry `kind: definition | declaration` with `defined_via`
naming the macro; hover carries it on the LSP side, which is the only place it
fits, because `textDocument/definition` is a bare list of locations.

The discriminator is **"is the body at this location"** rather than "was a macro
involved" (DEC-034), because `module_function` and `define_method` both point at
real bodies.

### The scorer stopped guessing, and the guess had been wrong twice

Session 15's `declaration` verdict identified these answers with an allowlist of
three Rails files plus a requirement that the answer be inside the app. The
verdict now reads trekr's own word, corroborated by a check that the truth's
line is not a written `def` — a fact about the gold entry that never looks at
the answer, which is what keeps it from excusing a wrong one.

**The single agreement run before deleting the old test found a bug in the new
one.** Testing for the `def` keyword called `def build_#{name}(*args)` a written
definition — Rails writes its constructors that way inside a `class_eval`
heredoc, with the interpolation in the middle of the name rather than the front
— and two honest declaration answers scored as errors until the test also looked
for `#{`. Overlapping the old and new instruments for exactly one run is what
caught it.

With that fixed, the swap moved **one site on each column**, both from
`residue-truth-absent` to `declaration-offered`. The gem column gained its first
`declaration-offered` ever: `in_app` had made a gem-side declaration
unrepresentable, which is precisely what DEC-033 ran into.

### And it unlocked the feature that was turned down

`define_model_callbacks` — 114 sites, built and reverted in session 29 —
now ships: **112 declaration, 2 residue, 0 confidently wrong**, against 112
`wrong` before. The extraction is unchanged. Only the answer's ability to
describe itself is new.

## Ruby-semantics fixes, measured against 0.2.0 (2026-09-27)

Seven findings from adversarial user testing (DEC-068 to DEC-071): `super`,
classes built by `Struct.new`/`Data.define`/`Class.new`, literal
`define_method`, local-receiver typing, `--def` on variables, alias binding,
Forwardable. Every comparison below runs each build against a store it indexed
itself.

### Gold set, retraced with `super` sites

The tracer now keeps a `super` site when the calling frame is the method Ruby
entered and the line holds exactly one `super`, so the widget_shop trace grew to
**3,243 sites, 168 of them `super`**. Every site scored, context pinned to
widget_shop, `VERDICTS=` diffing each build against the one before it.

| | 0.2.0 | now |
| --- | ---: | ---: |
| gem floor, correct | 1,502 | **1,619** |
| gem floor, confidently wrong | 93 | **92** |
| gem floor, `no-name` | 152 | 23 |
| `super` sites scored | 128 (all `no-name`) | 163 |
| `super` sites correct | 0 | **118** |
| `super` sites confidently wrong | 0 | **0** |
| app code | 33 correct, 1 wrong | unchanged |

Every verdict that moved, by commit: `super` 162 sites to scored verdicts and
ten gem residues gaining their truth as a candidate, nothing worse; literal
`define_method` one residue-hit → correct; local typing by reaching writes
three correct → residue-hit and one wrong → residue — parameters that the old
file-wide vote typed from a same-named write in another method, right by
coincidence three times and wrong once (DEC-071); alias binding one
residue-hit → residue-truth-absent, a ranking side effect of the alias now
taking its body's arity. The other commits moved nothing.

### CLI differential on rails and discourse

520 `--def` positions sampled with Prism from 600 files per corpus, seed 7 —
150 calls, 30 constants, 40 local reads, 15 ivar reads, 25 `super` per corpus
— asked of 0.2.0 and of this build. **186 answers changed, and every one was
read: 165 fixed, 21 neutral, 0 regressed.**

| | changed | classified |
| --- | ---: | --- |
| local reads | 80 | fixed: 0.2.0 snapped to a neighbouring name (76) or found nothing (4); now the variable with the writes it can see — ten sampled, all ten sites right |
| ivar reads | 29 | fixed: 25 answered with their writes in the file, 4 honestly `residue` (set in another file) |
| `super` | 50 | fixed: 36 resolved, 2 ambiguous, 12 residue (a `let` block, a `def` inside a block, `method_missing` after the owner) where 0.2.0 snapped to `merge`, `Date`, `join`… or found nothing |
| calls | 20 | 6 fixed — three locals typed from the right write (`firm = Firm.first`, not `DependentFirm`), `MigrationProxy = Struct.new … do` resolving its own `filename`, `I18n.t` landing on the `translate` body its alias copied, a parameter no longer typed from another method; 14 neutral — core stub lines shifted (11), the same answer at higher confidence (2), and one resolved-at-0.1 guess from another method's write now `residue` |
| constants | 7 | neutral: core stub lines shifted |

### `--dead`, rerun against a year of discourse history

DEC-038's check: a discourse clone at 2025-08-26 (ef503f2f8f9), `--dead
app/models app/services`, each candidate scored by whether its owner's last
segment still defines the name in today's checkout (6,825 commits later).
**This is not session 36's instrument** — its base rate here is 1.2 % (5 of
400 random methods in scope) against their 19.0 % — so only the two builds
compare, not the eras.

| tier | 0.2.0 | now |
| --- | ---: | ---: |
| `unreferenced` | 246, 22 deleted | **199**, 13 deleted |
| `single-caller` | 654, 13 | 712, 15 |
| `convention-only` | 287, 16 | 290, 17 |
| `super-only` | — | 2, 0 |

Six of 0.2.0's `unreferenced` had no owner at all — methods in a `Struct.new`
block — and score as deleted under any owner-keyed check, so its precision there
is 16 of 240 (6.7 %) against 13 of 199 (6.5 %) now: the same, on 41 fewer
candidates. Those 41 mostly moved to `single-caller` because `--dead` now asks
about a method's qualified owner (a call resolving to `Alpha::Helpers` had been
ruled out for `Helpers`), and that is where discourse's `deprecate_column`,
called from two models, stopped reading unreferenced. The two `super-only`
candidates are `ReviewableActionBuilder#perform_delete_user` and
`#perform_delete_and_block_user`, reached from the overrides in
`ReviewableFlaggedPost` and `ReviewableQueuedPost`.

## Precision fixes before 0.2.1 (2026-09-27)

A docs pass ran the README's examples on rails and found DEC-069 had merged
rails' test fakes into its models. Three related bugs came with it: a `super`
counted against its own method, and `--refs` that listed sites for a method
that does not exist. DEC-072 and DEC-073 cover them, and DEC-068 is amended.
Each build below ran on a store it indexed itself.

### rails, the queries that caught it

| | 0.2.0 | a411be2 | now |
| --- | ---: | ---: | ---: |
| `--refs 'ActiveRecord::Querying#where'`, confirmed | 1,197 | 860 | **1,216** |
| … excluded as `no_such_method` | 65 | 400 | 44 |
| `--refs 'ActiveRecord::ConnectionHandling#lease_connection'`, confirmed | 1,024 | 972 | **1,024** |
| `--def activerecord/test/cases/batches_test.rb:20:12` | `Querying#find_each` | residue | `Querying#find_each` |

`where` against 0.2.0: 19 `Cpk::Book` sites went from excluded to confirmed.
`Cpk::Book` was split between a model and actionview's fake in 0.2.0 too, and
the fake had won. Two `Author` sites moved from `no_such_method` to arity
exclusions, because DEC-071 left their receiver untyped. One site is new.
`lease_connection` reaches 1,024 by a different set. Four railties
`Post.lease_connection` sites are equally near the model and the fake, and the
naming tiebreak (`post.rb`) is what keeps them confirmed. Two
`pool.lease_connection` sites went from excluded to possible, because DEC-071
stopped typing a block parameter from another method's write.

### Gold set

widget_shop, 3,243 sites, context pinned, every site scored:

| | 0.2.0 | a411be2 | now |
| --- | ---: | ---: | ---: |
| gem floor, correct | 1,502 | 1,618 | **1,618** |
| gem floor, confidently wrong | 93 | 92 | **92** |
| `super` sites correct / confidently wrong | 0 / 0 | 118 / 0 | **118 / 0** |
| app code | 33 correct, 1 wrong | same | same |
| gem residue with the truth offered: top-3 | 66.6 % | 64.9 % | **66.8 %** |

No verdict moved against a411be2. (This run of a411be2 scores 1,618, one fewer
than the 1,619 recorded above. The same binary on another store gives the
other figure, and nothing in this change is involved.) The top-3 gain is the
`super` fix: a residue `super` no longer ranks its own method first. The first
cut split names on `.rbi` superclasses too. It turned six gem sites wrong or
residue, all in concurrent-ruby, whose `class Map < MapImplementation` Tapioca
records as `< MriMapBackend`. That is why an `.rbi` never splits a name.

### CLI differential

The same 520 `--def` positions on rails and discourse, against a411be2. **One
answer changed, and it is a fix:** `Cpk::Book.all` in `calculations_test.rb`
was residue and now resolves to `Scoping::Named::ClassMethods#all`. Four rails
`super` residues changed only their candidates, because each dropped the
method the `super` is written in. Discourse moved nothing. Its one split name,
`User` (a benchmark script's `Data.define`), resolves to `app/models/user.rb`
from everywhere but the script's own directory.

### `--dead`, `super-only`

`--dead activerecord/lib activemodel/lib actionpack/lib`, rails-only stores:

| | a411be2 | now |
| --- | ---: | ---: |
| `super-only` | 88 | 72 |
| … with an empty `super_from` | 39 | **0** |
| … naming only itself in `super_from` | 6 | **0** |
| `unreferenced` / `single-caller` / `convention-only` | 651 / 1,559 / 131 | 663 / 1,556 / 134 |

Thirteen moved from `super-only` to `unreferenced` and three to
`convention-only`. For each, the only `super` had been its own. Four
candidates dropped out. `reflect_on_all_aggregations` is a fix: it gained its
three confirmed calls through `Customer`, whose fake had won the merge. The
other three are the gap DEC-072 records. They are the `HttpAuthentication`
`authenticate` methods, and each now has three possible calls on
`@user.authenticate` in activemodel's tests. That `User` is a
superclass-less class in a third program, and it joins both variants. The
merge had excluded those sites by luck.

**`--dead`'s candidates depend on what else is in the store.** Its cheap
pre-filter, `Store::written_calls`, counts a name's calls across every blob,
not only the checkout's. The same a411be2 binary finds 88 `super-only`
candidates on a rails-only store and 38 on one that also holds discourse and
widget_shop. Those names reach the "plainly used" cap on other repos' calls.
This is not fixed here, and until it is, compare `--dead` runs only on
matching stores.

## Precision gaps after 0.2.1 (2026-09-27)

Six items from 0.2.1's known gaps: delegated methods' arity, `--dead`'s
evidence scope (DEC-074), a split name's `unresolved`, a plain class in
another gem (DEC-075), constants in a split class's body, and path forms
(DEC-076). Each build ran against a store it indexed itself. The first commit
bumps the store to v30, so 0.2.1 has stores of its own.

### rails, the tracked queries

| | 0.2.1 | now |
| --- | ---: | ---: |
| `ActiveRecord::Querying#where` confirmed / possible / excluded | 1,216 / 105 / 523 | **1,216 / 531 / 97** |
| … excluded on arity | 426 | **0** |
| `ActiveRecord::Querying#find_each` confirmed / possible / excluded | 12 / 6 / 8 | 12 / 13 / 1 |
| `ConnectionHandling#lease_connection` confirmed / possible / excluded | 1,024 / 84 / 87 | same |

All 426 arity exclusions of `where` became `possible`, and nothing else moved.
They were untyped receivers, mostly a chain (`Topic.where(…).where(…)`,
`mgr.where …`), excluded because `delegate(*QUERYING_METHODS, to: :all)` had
been read as taking no arguments. `possible` is the honest tier for an untyped
chain. Typing the chain is the next lever, not this one.

### Gold set

widget_shop, 3,243 sites, context pinned, every site scored:

| | 0.2.1 | now |
| --- | ---: | ---: |
| gem floor, correct | 1,618 | **1,618** |
| gem floor, confidently wrong | 92 | **92** |
| gem residue-hit / residue-truth-absent | 689 / 406 | 686 / 409 |
| `super` sites correct / confidently wrong | 118 / 0 | 118 / 0 |
| app code | 33 correct, 1 wrong | same |

Three verdicts moved, all from delegate arity, all residue-hit →
residue-truth-absent. They are `klass.scope …` in `enum.rb` (twice) and
`serialize` in `store.rb`. Delegated methods of the same names, such as the
reflections' `delegate :scope`, now fit on arity. They crowd the true `def`
out of the eight candidates shown. That is recorded as the cost. No other
commit moved a verdict.

### CLI differential

The same 520 `--def` positions on rails and discourse, against 0.2.1, now
compared on status, owner, site, confidence *and* the top three candidates.
**No answer changed status, owner or site.** Sixteen residues reordered their
candidates, all from delegate arity, with the truth in the top three neither
before nor after. Discourse's `expect`/`eq` (14) now list
`EndpointsCompliance`, whose `delegate`s fit, first. Two rails residues
(`from`, `join`) gained a delegator in the top three. The constant fix
changed none of the 60 sampled constants. Over the 2,200 bare constants read
inside rails' split class bodies, three went residue → resolved
(`ActiveRecord::TestCase::InTimeZone` twice, `SQLSubscriber`) and none
changed otherwise.

### Split names

rails declares 48 top-level names with superclasses written differently. Five
changed variants: `User`, `Post`, `Person`, `Session`, `CallbacksTest`. In
each, a plain declaration in a gem that declares no variant had been merged
into two or three other gems' classes. activemodel's test `User` had joined
both activerecord's model and railties' template, and its `include
ActiveModel::SecurePassword` with them. Each such declaration now stands
alone. Discourse's one split name, `User`, loses a dry-initializer rake task's
`class User`, which had joined the app model.

### `--dead`

rails, `--dead activerecord/lib activemodel/lib actionpack/lib`:

| | 0.2.1 | now |
| --- | ---: | ---: |
| rails-only store | 2,425 | **2,443** |
| same checkout, store also holding two discourse checkouts | 2,175 | **2,443** (byte-identical output) |
| `super-only` on those two stores | 72 / 33 | 74 / 74 |
| `super-only` with an empty `super_from` | 0 | 0 |

The three `HttpAuthentication` `authenticate` methods are back as
`single-caller`. That is the cost the 0.2.1 section recorded for the gap
DEC-075 closes.

DEC-038's history check: discourse at 2025-08-26 (ef503f2f8f9), `--dead
app/models app/services`, candidates scored against today's defs (same
instrument as the 0.2.1 run above, candidates / deleted since):

| tier | 0.2.1, discourse-only store | 0.2.1, store with rails too | now, either store |
| --- | ---: | ---: | ---: |
| `unreferenced` | 198 / 13 | 194 / 13 | 199 / 13 |
| `single-caller` | 711 / 15 | 688 / 14 | 712 / 15 |
| `convention-only` | 290 / 17 | 283 / 16 | 290 / 17 |
| `super-only` | 2 / 0 | 2 / 0 | 2 / 0 |

The candidates no longer depend on what else is indexed (1,201 against 1,167
for 0.2.1), and precision per tier is unchanged: `unreferenced` 13 of 199
(6.5 %).

## Chains typed from core's return types (2026-09-27)

DEC-077 and DEC-078, against main at cfba0f1. Each build ran on a store it
indexed itself (widget_shop, rails, discourse).

### Gold set

widget_shop, 3,243 sites, context pinned, every site scored:

| | cfba0f1 | now |
| --- | ---: | ---: |
| gem floor, correct | 1,618 | **1,630** |
| gem floor, confidently wrong | 92 | **92** |
| gem floor, right-owner-wrong-site | 12 | 16 |
| gem floor, ambiguous-wrong | 9 | 10 |
| gem floor, residue with the truth offered | 686 | 676 |
| `super` sites correct / confidently wrong | 118 / 0 | 118 / 0 |
| app code | 33 correct, 1 wrong | 34 correct, 1 wrong |

Twenty verdicts moved. Thirteen residues became correct: nine through
`chain:name` (ActiveSupport's `x.to_s.singularize`, `pluralize`, `camelize`,
`humanize`, `demodulize`, `presence`, `foreign_key`, at 0.18 to 0.5), four
through `chain` or `literal`. Four became right-owner-wrong-site: `Set` and
`Hash` methods landing on core's stub (which stands in for stdlib `set.rb`) or
on another gem's reopening of `Hash`. One residue became ambiguous-wrong
(`o.source.empty?` at 0.06), and two app declarations offered became
declarations.

### CLI differential

The same 520 positions. **11 answers changed: 7 fixed, 3 neutral, 1
confidently wrong.**

| answer | now | read |
| --- | --- | --- |
| `("0".."9").to_a` | `Range#to_a` | fixed, literal |
| `1.minute` | ActiveSupport's `Numeric#minute` | fixed, literal |
| `50.times` | `Integer#times` | fixed, literal |
| `[…].flatten.to_set` | `Enumerable#to_set` | fixed, chain |
| `changed.map(&:inspect).join` | `Array#join`, ambiguous 0.09 | fixed |
| `info[:compatible].map { … }` | `Enumerable#map`, ambiguous 0.01 | fixed |
| `reply.extract_quoted_post_numbers` (a `let`) | `Post#…`, ambiguous 0.14 | fixed |
| `end.load(&block)` | residue; `Marshal.load` no longer fits a call with no arguments | neutral |
| `post.user.trust_level = 3` | residue, now naming `User` as the receiver | neutral |
| `step[:end_time].nil?` | `Kernel#nil?`, ambiguous 0.00 | neutral |
| `{ … }.to_json` | json's `GeneratorMethods#to_json` | wrong: ActiveSupport prepends its encoder from a loop |

### References

rails, `--refs`, confirmed / possible / excluded:

| | cfba0f1 | now |
| --- | ---: | ---: |
| `String#strip` | 3 / 213 / 0 | 109 / 107 / 0 |
| `String#downcase` | 1 / 116 / 2 | 25 / 92 / 2 |
| `String#gsub` | 10 / 149 / 0 | 57 / 102 / 0 |
| `ActiveRecord::Querying#where` | 1,216 / 531 / 97 | unchanged |
| `ActiveRecord::ConnectionHandling#lease_connection` | 1,024 / 84 / 87 | unchanged |

## Gold sets from the user's own gems (2026-09-27)

widget_shop was written for this evaluation; these were not. `script/gold_gem.sh`
copies a gem, runs its own RSpec suite under the TracePoint tracer
(`script/exercise_rspec.rb`), indexes the copy into a store of its own, and
scores it with `script/gold.py` (`make gold-gem GEM=…`). Every suite passes
offline. Scored with `APP_SAMPLE=600 SAMPLE=300 SEED=12`, build ff6f12a.

"App" is the gem's own checkout, so it includes its specs; the split by call
site is the part to read, because a spec's calls are mostly RSpec's DSL.

| gem | traced app sites | scored | correct | found | confidently wrong |
| --- | ---: | ---: | ---: | ---: | ---: |
| graph_weaver, not a spec | | 78 | 60 (76.9%) | 64 (82.1%) | 2 (2.6%) |
| graph_weaver, a spec | | 482 | 58 (12.0%) | 290 (60.2%) | 1 (0.2%) |
| graph_weaver, all app | 25,937 | 560 | 118 (21.1%) | 354 (63.2%) | 3 (0.5%) |
| accord, all app | 5,023 | 600 | 56 (9.3%) | 435 (72.5%) | 1 (0.2%) |
| accord, not a spec | | 64 | 48 (75.0%) | 57 (89.1%) | 1 (1.6%) |
| accord, a spec | | 536 | 8 (1.5%) | 378 (70.5%) | 0 |
| polyid, all app | 1,900 | 595 | 68 (11.4%) | 391 (65.7%) | 13 (2.2%) |
| polyid, not a spec | | 56 | 29 (51.8%) | 50 (89.3%) | 0 |
| polyid, a spec | | 539 | 39 (7.2%) | 341 (63.3%) | 13 (2.4%) |

`found` is `correct` plus `residue-hit` (the truth among the ranked guesses).
Gem floors: graph_weaver 53.7% correct / 1.2% wrong of 246, accord 50.8% /
3.3% of 244, polyid 48.1% / 6.1% of 297.

**What the numbers say.** In library code trekr is right three times in four
and wrong a few times in a hundred. In specs it is almost never *right* —
`expect`, `eq`, `it`, `describe`, `to` and every `let` are implicit calls in a
block whose `self` is an RSpec example group, and nothing types that — but it
still offers the truth among its guesses about two times in three. The same
gap is 40% of all misses in the editor-click replay (`script/clicks.py`).

**Every confidently wrong app site, read.**

- polyid, 7: `RSpec.describe` resolves to minitest's `Kernel#describe` at
  confidence 1. RSpec's `describe` is made by `define_singleton_method`, so the
  lookup falls through to `Kernel`, where minitest (bundled through
  activesupport) monkeypatched one in.
- polyid, 6: `User.find` / `User.find_by` resolve to ActiveRecord's, not
  `PolyId::Model`'s override, which reaches every model through
  `ActiveSupport.on_load(:active_record) { include PolyId::Model }`.
- accord `field.rb:166` `nested_schema`, graph_weaver
  `install_generator.rb:164` `append_to_file` and `railtie.rb:566` `env`: a
  call on `self` answered with the class's own method, where the object at
  runtime was a subclass (in accord, `Fields::Array`; in graph_weaver, a spec's
  stand-in) that overrides it.

**Harness fix.** sorbet-runtime replaces each `sig`'d method with a wrapper,
and the trace records the wrapper's line, so 7.6% of graph_weaver's app truths
pointed at `call_validation_2_7.rb` or `_methods.rb`. They are excluded as
`wrapped`, like a column mismatch, and reported beside the table; before that
they were four of graph_weaver's six "confidently wrong".

## RSpec (2026-09-27)

DEC-084 to DEC-089, against main at 7b3238c. Each build ran on stores it
indexed itself. The gold sets are the ones `make gold-gem` traced for the
section above, rescored (`APP_SAMPLE=600 SAMPLE=300 SEED=12`); widget_shop's
is a 3,075-site trace, every site scored, context pinned.

### Click replay

`script/clicks.py`, the same 12 repositories and 20,290 definition clicks:

| | 7b3238c | now |
| --- | ---: | ---: |
| library clicks missed (empty or unsure) | 2,130 of 8,431 (25.3%) | 2,114 (25.1%) |
| spec clicks missed | 7,942 of 11,859 (67.0%) | 3,326 (28.0%) |
| all misses | 10,072 | 5,440 |
| … "spec DSL and let names" bucket | 4,015 | 246 |

What is left in specs is mostly the dynamic buckets: chained receivers,
untyped locals, symbol arguments. The implicit-receiver remainder is
flipper, whose checkout indexes no rspec-core (129, answered as such), a
bare top-level `describe`, predicate matchers, custom `Matchers.define`
matchers and FactoryBot's `create` where no `config.include` names it.

### Gold sets

| | correct | declaration | found | confidently wrong |
| --- | ---: | ---: | ---: | ---: |
| graph_weaver, a spec, before | 58 of 482 (12.0%) | 0 | 290 (60.2%) | 1 |
| graph_weaver, a spec, now | 372 (77.2%) | 57 | 392 (81.3%) | 1 |
| graph_weaver, not a spec | 60 of 78 → 60 | 0 → 0 | 64 → 64 | 2 → 2 |
| accord, a spec, before | 8 of 536 (1.5%) | 0 | 378 (70.5%) | 0 |
| accord, a spec, now | 297 of 531 (55.9%) | 110 | 373 (70.2%) | 0 |
| accord, not a spec | 48 of 64 → 48 | 0 → 0 | 57 → 57 | 1 → 0 (ambiguous) |
| polyid, a spec, before | 39 of 539 (7.2%) | 14 | 341 (63.3%) | 13 |
| polyid, a spec, now | 281 of 535 (52.5%) | 121 | 342 (63.9%) | 7 |
| polyid, not a spec | 29 of 56 → 29 | 0 → 0 | 50 → 50 | 0 → 0 |

A `let` answers with its line, a declaration (`defined_via: let`), where the
trace saw the method rspec-core generates, so the `declaration` column is
the lets. A few sites (accord 5, polyid 4) moved to `column-mismatch`: the
name at the traced column is now a different one than the trace saw. polyid's seven are the `on_load` `find` the
entry above records; `RSpec.describe`'s seven are gone.

Gem floors: confidently wrong graph_weaver 3 → 2, accord 8 → 1, polyid 18 →
8, the rest moving to `ambiguous-wrong` (DEC-081's amendment); correct
unchanged at 132, 124, 143.

widget_shop, 3,075 sites: app code unchanged (34 correct, 20 declarations, 1
wrong of 61). Gem floor: correct 1,514 → 1,521 — six chains typed through a
Tapioca `.rbi`'s return type (DEC-087) and one more — confidently wrong 92 →
21, ambiguous-wrong 8 → 79. Every one of the 71 is a call on `self` whose
method a subclass or a later-mixed module overrides, now named.

### References

rails, the 40 `--refs` queries: 37 unchanged; three sites move from possible
to excluded, `ex.inspect` and `e.to_s` in `rescue … => e` (twice), now typed
as the exception (DEC-089).

### Measured and not kept

The example group first vouched for every nested block in an example. The
gold sets found two confidently wrong answers that way (`boolean` in a
helper's `schema { }`, `output` in a `GraphWeaver.graph do`), and a core
stub for `Dir.mktmpdir` made four `mktmpdir` calls wrong against the real
`tmpdir.rb`. Both are in DEC-084. The RSpec stub first typed `expect(x)` as
`ExpectationTarget`; 71 of graph_weaver's, 68 of accord's and 64 of polyid's
`.to` and `.not_to` answers were
confidently wrong until it said `ValueExpectationTarget`.


## RSpec, the second pass (2026-09-28)

DEC-090 to DEC-096, against main at eb84162, each build on stores it indexed
itself; the same 12 click-replay repositories, the same three gem gold sets
(`APP_SAMPLE=600 SAMPLE=300 SEED=12`), widget_shop's 3,075-site trace, and
the rails `--refs` queries.

### Click replay

| | eb84162 | + matchers (DEC-090, 091) | + shared groups, symbols (092, 093) | + `&:`, Minitest, lets (094–096) |
| --- | ---: | ---: | ---: | ---: |
| library clicks missed | 2,114 of 8,431 (25.1%) | 2,114 | 2,088 (24.8%) | 2,088 |
| spec clicks missed | 3,326 of 11,859 (28.0%) | 3,294 | 3,282 | 3,106 (26.2%) |
| all misses | 5,440 | | 5,370 | 5,194 |
| … "chained receiver" bucket | 1,251 | | 1,251 | 1,037 |
| … "symbol naming a method" | 59 | | 30 | 26 |

246 clicks that missed now answer, and no click that answered misses. The
custom matchers are most of the second column (`include_attrs` 19,
`match_attrs` 10); the lets are most of the last, the chained-receiver
bucket's drop of 214, out of the 575 spec misses on a `let` or `subject`. Predicate matchers moved 38 misses to 36: the rule answers
when the subject is typed, and in these suites the subject is mostly a `let`
whose block is a chain (`subject { [].pluck(0) }`) or an untyped local, so
the rest are residue that now names the predicate.

### Gold sets

| a spec call site | correct | declaration | found | confidently wrong |
| --- | ---: | ---: | ---: | ---: |
| graph_weaver, before | 372 of 482 | 57 | 392 | 1 |
| graph_weaver, now | 374 | 58 | 395 | 1 |
| accord, before | 297 of 531 | 110 | 373 | 0 |
| accord, now | 323 | 113 | 374 | 0 |
| polyid, before | 281 of 535 | 121 | 342 | 7 |
| polyid, now | 288 | 124 | 342 | 7 |

Not-a-spec call sites and every gem floor are unchanged, correct and
confidently wrong alike (graph_weaver 60/2 and 132/2, accord 48/0 and 124/1,
polyid 29/0 and 143/8). widget_shop: app 34 correct, 1 wrong; gem floor 1,521
correct, 21 confidently wrong — unchanged. The declarations are custom
matchers (DEC-091); the correct answers, calls on a typed `let` (DEC-096).

### References

rails, the 40 `--refs` queries: no site changes tier. 244 sites are new, all
`possible`, all `&:name` block-passes that were not recorded (DEC-094) —
117 of them `&:to_s` under `ActiveSupport::TimeWithZone#to_s`, 34 `&:first`,
12 `&:strip`.

### Measured and not kept

The predicate-matcher rule first asked whether the nearest `method_missing`
in the example's chain was RSpec::Matchers'. rspec-core's ExampleGroup
defines its own, which comes first and calls `super`, so the rule passed its
testbed and fired in no real spec; the checkpoint's replay showed the
residue reason unchanged on all 38 sites.

## Runtime ancestry (2026-09-28)

DEC-097 to DEC-099, against main at eb84162 and again after rebasing (the
last table). Each build ran on stores it indexed
itself: the three gem gold sets (`APP_SAMPLE=600 SAMPLE=300 SEED=12`, the
traces the RSpec section used), widget_shop's 2,987-site trace with every site
scored and context pinned, rails' 40 `--refs` queries, `--dead
activerecord/lib activemodel/lib actionpack/lib` on rails and `--dead lib` on
a store holding only activerecord, and `script/clicks.py` over the same 13
repositories (21,154 definition clicks).

### Mixins sent to a constant (DEC-097)

No gold set's confidently wrong count moved; one verdict moved in all four,
accord's gem floor gaining a `super` (sorbet-runtime's
`singleton_method_added`, residue-hit → correct). Clicks: definition misses
5,953 → 5,940, "module never mixed in" 151 → 138. rails `--refs`: one site
moved, possible → excluded, `super` in `BigDecimalWithDefaultFormat#to_s`
now landing on `BigDecimal`'s. rails `--dead`: 2,366 candidates either way,
six tiers moved and each toward referenced (`Relation#exec_queries` is
reached by `super` from `RecordFetchWarning`, which activerecord prepends to
it); the activerecord-only store swaps two tiers the same way.

Measured and not kept: recording every sent mixin, conditional or in a method.
accord's spec `let`s (four sites) went correct → confidently wrong through
sorbet-runtime's `if defined?` prepend, widget_shop's `find_by` and `where`
(two) through activerecord's encryption `install_support`. And 531 rails
`--refs` sites moved excluded → possible while `Object.prepend(self)` in
activesupport's `require_dependency.rb` put an unresolvable `self` into every
chain; `self` in a module's body is now the module.

### `on_load` hooks (DEC-098)

Against the DEC-097 build (the three-build comparison, base → DEC-097 →
DEC-098, is in each row where it moved):

| | base | DEC-097 | DEC-098 |
| --- | ---: | ---: | ---: |
| polyid, a spec: correct / confidently wrong of 535 | 281 / 7 | 281 / 7 | **302 / 0** |
| polyid, not a spec: correct of 56 | 29 | 29 | **35** |
| accord gem floor: correct of 244 | 124 | 125 | 125 |
| widget_shop gem floor: correct / confidently wrong of 2,838 | 1,521 / 21 | 1,521 / 21 | 1,521 / 21 |
| clicks, definition misses of 21,154 | 5,953 | 5,940 | **5,890** |
| … "known type, method not found" | 284 | 284 | 256 |
| … "module never mixed in" | 151 | 138 | 116 |
| rails `--dead` candidates | 2,366 | 2,366 | 2,365 |

graph_weaver's and accord's app gold sets did not move. The polyid sites the
dogfood log carried (`cache_spec.rb:81:12` `id_for`, `user_spec.rb:54:33`
`find`) resolve to `PolyId::Model::ClassMethods`.

Measured and not kept: recording hooks registered inside a block. The
DEC-098 build that did moved widget_shop's gem floor to 22 confidently wrong:
`action_methods` in actionmailer's `respond_to_missing?`, traced before the
Railtie's `initializer` ran its `on_load(:action_mailer)` include of
`AbstractController::UrlFor`. The same rule now covers sent mixins, and
every move DEC-097 measured is still there.

### A concern's `included do` (DEC-099)

widget_shop's gem floor: correct 1,521 → 1,525, confidently wrong 21 → 19,
the two being `def self.` methods of ActiveRecord::Core's `included` block
that a caller reached on a model. Nothing else in the gold sets, the 40
`--refs` queries or the click replay moved. rails `--dead`: 2,365 → 2,364
candidates; five change owner to `ActiveRecord::Core::ClassMethods`, and
`strict_loading_violation!` and `asynchronous_queries_session` go
unreferenced → single-caller.

### Rebased onto the second RSpec pass

The three commits were written and measured against eb84162, the tables
above; rebased onto 89b2f50 they were measured again, both builds on stores
of their own:

| | 89b2f50 | now |
| --- | ---: | ---: |
| polyid, a spec: correct / confidently wrong of 535 | 288 / 7 | **309 / 0** |
| polyid, not a spec: correct of 56 | 29 | **35** |
| accord gem floor: correct of 244 | 124 | 125 |
| widget_shop gem floor: correct / confidently wrong of 2,838 | 1,521 / 21 | **1,525 / 19** |
| graph_weaver, accord app; the other gem floors | | unchanged |
| clicks, definition misses of 21,154 | 5,701 | **5,638** |
| … "known type, method not found" | 303 | 275 |
| … "module never mixed in" | 181 | 146 |
| rails `--refs`, 40 queries | | 2 sites moved, as above |
| rails `--dead` candidates | 2,366 | 2,364 |

No confidently wrong count rose in any gold set.

## Rails macros and the RSpec leftovers (2026-09-28)

DEC-110 to DEC-116, against main at 0961cd6, each build on stores it indexed
itself: the three gem gold sets (`APP_SAMPLE=600 SAMPLE=300 SEED=12`),
widget_shop's trace with every site scored and context pinned, rails' 40
`--refs` queries, `script/clicks.py` over the 13 dogfood repositories (21,154
definition clicks), and a macro fixture: a copy of widget_shop with an
`Account` model writing every macro the lane added (`has_secure_password`,
`has_secure_token`, both attachment macros, three enums with and without
affixes, `store`, `store_accessor`, `attribute`, `alias_attribute`,
`class_attribute`, `cattr_accessor`, `mattr_accessor`, scopes,
`accepts_nested_attributes_for`, `delegate_missing_to`) and a service calling
them, traced under TracePoint (4,462 sites, 114 of them the app's) and click-
replayed with `LIBRARY=app,lib`.

### The macro fixture

| | main | now |
| --- | ---: | ---: |
| app sites: correct / declaration / confidently wrong, of 112 → 114 | 55 / 20 / 2 | **63 / 47 / 0** |
| … whose truth is generated (50): correct / declaration / declaration-offered / residue-nothing-known | 3 / 20 / 9 / 18 | 3 / **47** / 0 / 0 |
| gem floor: correct / confidently wrong | 2,137 / 33 | 2,198 / 33 |
| clicks: definition misses of 533 | 134 | **78** |
| … "known type, method not found" | 42 | 2 |

The last two app misses of that kind are `has_one_attached` and
`has_many_attached` themselves: Active Storage's `Attached::Model` reaches
models through an `on_load` inside an `initializer`, which DEC-098 does not
follow.

### Gold sets

| | main | now |
| --- | ---: | ---: |
| widget_shop app: correct / confidently wrong | 19 / 2 | **21 / 1** |
| widget_shop gem floor: correct / confidently wrong, of 2,838 → 2,899 | 1,525 / 19 | 1,586 / 19 |
| graph_weaver, a spec: correct / declaration of 482 | 374 / 58 | 375 / 60 |
| accord, a spec: declaration of 531 | 113 | 120 |
| polyid gem floor: correct of 297 → 300 | 143 | 146 |

No confidently wrong count rose anywhere. The gem floors' gains are
column-mismatches made correct: a `class_attribute` or `scope` inside a
concern's `included do` declared its routed `ClassMethods` at the macro's own
name, so a click on the macro answered the module; the routing now declares
it at `do` (DEC-110's refactor, DEC-099's rule). widget_shop's app `where` in
`scope :affordable` is DEC-116; graph_weaver's `http_response` in `serving`'s
block DEC-113; accord's seven declarations are its bare top-level
`describe`s (DEC-115).

### Click replay

| | main | now |
| --- | ---: | ---: |
| library clicks missed of 8,630 | 2,137 | 2,137 |
| spec clicks missed of 12,524 | 3,501 | **3,268** |
| … "spec DSL and let names" | 308 | 264 |
| … "chained receiver" | 1,137 | 957 |

233 definition clicks that missed now answer, and none that answered miss:
meddleware's `subject.use` (79, DEC-114), bare `describe` (43, DEC-115), and
the rest calls on an implicit subject. The macros moved nothing here: these
are gems, not Rails apps.

### References

rails, the 40 `--refs` queries: 30 `where` calls in scope bodies move from
`ActiveRecord::Querying#where` (confirmed → excluded) to
`ActiveRecord::QueryMethods#where` (excluded → confirmed) (DEC-116); five
sites move excluded → possible: `reload` on `comments(:greetings)`, whose
fixture accessor had been guessed an Integer by a name vote the new
declarations dissolved, and `fetch` on `EncryptedConfiguration`, which
`delegate_missing_to :options` (DEC-112). graph_weaver's
`RSpec::SharedExampleGroups::RawHttpServer#http_response` went 0 confirmed,
11 possible, 15 excluded to 26 confirmed (DEC-113).

### Store

rails: 88,292 → 101,906 definition rows, 65.3 → 67.1 MB, almost all the
schema's dirty-tracking declarations (DEC-111).
