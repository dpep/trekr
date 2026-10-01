# How trekr compares

Each engine is asked `textDocument/definition` over LSP at call sites whose
answer Ruby itself recorded: the TracePoint gold set
([BASELINE.md](BASELINE.md)). `script/compare.py` produces every row. Latest
run: **2026-09-30**.

**correct@1** means the first location returned is the file and line Ruby ran
(±1). **wrong@1** means the engine answered and the first location is
something else. **found** means the truth appears anywhere in the answer.
**Ready** runs from launch to the end of indexing, either on a checkout the
engine has never indexed (**cold**) or launched again (**restart**). **Warm**
is the median latency of one request. **RSS** is peak memory, including child
processes.

## discourse — 500 app-code sites, public

| engine | answered | correct@1 | wrong@1 | found | ready, cold | ready, restart | warm | RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| trekr 0.8.2+main 24e3b72 | 96.0 % | **76.6 %** | 19.4 % | **82.4 %** | **8.6 s** | **0.01 s** | 10 ms | **380 MB** |
| ruby-lsp 0.26.9 | 63.0 % | 51.4 % | **11.6 %** | 56.8 % | 18 s | 18 s | **1.2 ms** | 1,300 MB |

trekr is right half again as often (76.6 % vs 51.4 %) in under a third of the
memory. Its index persists, so a restart costs nothing, while ruby-lsp
re-indexes on every launch. ruby-lsp is wrong less often because it declines
more: about 80 % of each engine's answers are right. Most of trekr's wrong@1
(74 of 97 sites) is the first candidate of an answer trekr marks as unresolved
(`residue`), and the wire has no way to say so. ruby-lsp is faster per request,
because trekr currently spends most of each request checking whether its tree
is stale.

## widget_shop — 63 sites, private

| engine | answered | correct@1 | wrong@1 | found | ready, cold | ready, restart | warm | RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| trekr 0.8.2+main 24e3b72 | **100 %** | **62 %** | 38 % | **62 %** | 3.5 s | **0.01 s** | 4.9 ms | 270 MB |
| ruby-lsp 0.26.11 † | 78 % | 49 % | 29 % | 54 % | 20 s | 20 s | 0.32 ms | 560 MB |
| ruby-lsp 0.27.0.beta5 (Rubydex) † | 56 % | 35 % | **21 %** | 38 % | 7.2 s | 7.2 s | 0.69 ms | 430 MB |
| Sorbet 0.6.13439, as configured | 13 % | 0 % | 13 % | 0 % | 1.3 s | 1.3 s | **0.17 ms** | **150 MB** |
| Sorbet 0.6.13439, `--typed=true` | 59 % | 19 % | 40 % | 19 % | **0.40 s** | 0.40 s | 0.48 ms | **150 MB** |

† Run on a worktree without `sorbet/` and with byte-identical app code. trekr
scores the same there, except found is 65 %.

Over a third of these sites call Rails-generated methods. trekr answers with
the macro (`belongs_to :supplier`), and the scorer wants the `define_method`
inside Rails, so trekr's wrong@1 is high here by construction. The Rubydex-based
beta answers less often than 0.26 and is also wrong less often. No file in this
app has a `# typed:` sigil, so Sorbet as configured resolves no method calls.

## Reading the numbers

* **Only the first location is scored.** trekr's `status`, `confidence` and
  `kind` are discarded. A declaration answer, meaning the macro that defined a
  method, counts as wrong@1. BASELINE.md scores trekr on its own terms.
* **A call through `delegate` answers the method it sends to first**
  (DEC-211), and the delegate second. Ruby's trace records the delegate, so
  `Model.where` scores wrong@1 and found: 6 of discourse's 500 sites.
* **Ready ends at the engine's own `$/progress` end.** Sorbet reports no
  progress, so for Sorbet it ends at its last message before 25 s of silence.
  trekr's cold run starts from an empty store and indexes the app plus 300
  gems. Before that finishes, it already answers from what it has read and
  says those answers are partial, at 0.25–0.74 s on discourse (DEC-322). That
  early window is not counted here. ruby-lsp's bundle was already composed in
  `.ruby-lsp/`; a first-ever run that must install gems takes minutes.
* **A gem or stdlib file counts as the same file in any install** of that
  version, because ruby-lsp runs from its own gem home.
* **ruby-lsp runs the version the project pins**: 0.26.9 on discourse. The 0.27
  beta can't run there honestly, because `--beta` re-resolves all 300 gems with
  pre-releases allowed, which moves the files the gold set points into.
* **Sorbet runs in the project's own bundle**, frozen.
* **The machine was busy**: Apple M2, 8 cores, load average 6–19. Each timing
  is the median of 4 interleaved runs, and single runs varied by up to ±50 %.
  Compare timings within a table. Accuracy was identical in every run.

## Since the 2026-08-25 run

* ruby-lsp went from 34.4 % to 51.4 % correct. This came from the scorer, not
  the engine. The old scorer compared real paths, so a correct answer into
  ruby-lsp's own copy of a gem counted as wrong. Under the old rule, today's
  answers score 33.0 %.
* trekr went from 82.6 % to 76.6 % correct. Most of the drop comes from the
  gold set, which was retraced under discourse's declared Ruby 3.4.10 and now
  records `super` sites: the August build scores 77.8 % on today's sites.
  Against that same build, 0.8.2 is 1.2 points less correct, 2.2 points more
  wrong, and slower per request (10 ms vs 1.0 ms, measured in the same hour).
  Six of those sites are the delegate answer above. After this run, DEC-350
  and DEC-351 fixed seven more (`@user.id` beside a script's `User`, and a
  `delegate` line ranked ahead of the method it sends to): the same sites
  then score 78.0 % correct. `posts.order` offering `OptionParser#order`
  first is still open.
* RSS used to be one pid read once. It now includes child processes: trekr's
  index process, and the Rails app that ruby-lsp-rails boots.
* Earlier rows, and session 9's hand-picked comparison, are in git history.

## Rubydex as a library

It still can't be scored on its own (checked against `rubydex` 0.4.1). A
`MethodReference` has a name, a location and sometimes a receiver, but no
declaration it resolves to. The only `REFERENCES` edge in `rdx query` runs from
a document to a constant. The ruby-lsp add-on needs ruby-lsp ≥ 0.27.0.beta4,
so Rubydex is measured through that beta.

## Reproduce

```sh
TREKR=~/code/lib/rust/trekr     # this repo
GEM_HOME=~/.local/share/trekr-compare/gems      gem install ruby-lsp -v 0.26.11
GEM_HOME=~/.local/share/trekr-compare/gems-beta gem install ruby-lsp -v 0.27.0.beta5
cargo build --release --manifest-path $TREKR/Cargo.toml

# Gold: discourse on its own Ruby (3.4.10) and bundle, with Postgres, Redis,
# and `pnpm install` run under Node ≤ 24.
cd ~/code/lib/ruby/discourse
BUNDLE_FROZEN=true TREKR_GOLD=/tmp/gold-discourse.ndjson \
  TREKR_EXERCISE=$TREKR/script/exercise_discourse.rb \
  bin/rails runner $TREKR/script/trace_gold.rb

# A fresh TREKR_DB is a cold start; run again on it for a restart.
TREKR_DB=/tmp/trekr-compare/store.db $TREKR/script/compare.py \
  --engine trekr --engine ruby-lsp \
  --gold /tmp/gold-discourse.ndjson --root ~/code/lib/ruby/discourse --sample 500
```

widget_shop works the same way: use `script/exercise_widget_shop.rb`,
`--sample 0`, `--engine sorbet --engine sorbet-typed`, and `--rewrite-root
<worktree>` for the copy without `sorbet/`. widget_shop isn't published. A
ruby-lsp run writes a git-ignored `.ruby-lsp/` and, through ruby-lsp-rails,
bootsnap cache under `tmp/`.
