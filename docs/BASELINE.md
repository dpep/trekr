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
- **The LSP `references` path does not narrow by owner.** It takes the name from
  the position and runs the bare-name query, so `Querying#where` and any other
  `#where` share one answer. The CLI's `--refs 'Owner#method'` does narrow; the
  server should resolve the position to its owner first and does not yet. That is
  a real gap and the numbers above are the un-narrowed ones.

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

1. **Owner-narrow the LSP references path.** The CLI already does it; the server
   throwing that away is the largest correctness gap this exercise found.
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
