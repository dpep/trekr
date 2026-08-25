# The competitive series

**Are we closing or stalling?** Session 9's head-to-head could not answer that.
It hand-picked 45 positions and adjudicated the disagreements by eye, which
answered "who is better today" and could never be run again the same way.

This file is the replacement: a **dated, append-only series**, one row per
engine per run, produced by one command. Every engine is driven over the **LSP
protocol** — the surface an agent actually uses — against the **TracePoint gold
set**, so the instrument is the same for everyone and the next run is comparable
to this one.

```sh
script/compete.py --engine trekr --engine ruby-lsp \
  --root ~/code/lib/ruby/discourse --sample 500 --out /tmp/compete.ndjson
```

Re-run it when ruby-lsp 0.27 ships, when Rubydex grows a call graph, or when
trekr changes something that should move a column.

## What is scored, and what had to be given up to score it

Competitors return locations with no status and no confidence: an answer they
are sure of and a guess look identical on the wire. So the columns are the ones
every engine can produce.

| column | is |
| ------ | -- |
| `answered` | the engine returned at least one location |
| `correct@1` | its **first** location is the file and line Ruby dispatched to |
| `wrong@1` | it answered, and its first location is not that |
| `found` | the truth is anywhere in the locations it returned |

**trekr is scored the same way, by its top location**, whether that came from a
`resolved` answer or the first candidate of a `residue`. That deliberately
throws away trekr's entire disclosure story — the thing the product is *for* —
so that one number means one thing across four engines. The status breakdown
lives in [BASELINE.md](BASELINE.md), and the two files answer different
questions: this one asks who points at the right line, that one asks who knows
when they don't.

Read `correct@1` and `wrong@1` together. An engine that answers everything and
is right half the time scores the same `correct@1` as one that answers half as
often and is always right, and they are not the same tool.

## Fairness notes, which matter more than the numbers

* **Setup is timed apart from serving.** ruby-lsp composes a bundle before it
  can start — it writes a `.ruby-lsp/Gemfile` into the checkout and resolves it.
  Hiding that in a first-query number would flatter it; counting it as latency
  would condemn it. The `setup` column is `initialize` round-trip **+** the time
  the server then spent indexing before it would answer. The **first ever** run
  against a checkout pays much more than the number here: composing discourse's
  bundle took **164 s** once, and 1.4 s on every run after.
* **Every server is allowed to finish indexing.** ruby-lsp returns `initialize`
  in a second or two and then indexes in the background; Sorbet does the same.
  An early first draft of this harness asked immediately and recorded **0 %
  answered for ruby-lsp** — the harness's fault, not the engine's, and exactly
  the kind of artifact this project exists to catch before publishing.
* **ruby-lsp runs the version the project pins.** Discourse's own Gemfile names
  `ruby-lsp`, so the composed bundle runs **0.26.9** there and 0.26.11 where the
  project pins nothing. The version column reports what the server said about
  itself in `serverInfo`, not what is installed here.
* **Sorbet is measured inside the project's own bundle**, because `srb` shells
  out to `srb-rbi`, which materializes the whole Gemfile. An isolated gem home
  cannot serve it.
* **Sorbet gets two rows.** widget_shop carries `sorbet/rbi/` and a
  `sorbet/config`, but **no file in it has a `# typed:` sigil**, so every file
  is `typed: false`, where Sorbet does not resolve method calls at all. The
  second row passes `--typed=true`, which raises the floor for every file
  without editing one — "Sorbet if the app were annotated" beside "Sorbet as
  this app configures it". Writing sigils in would have shifted every line by
  one and invalidated the gold set.
* **`correct@1` penalizes an answer that is arguably better.** Where Rails
  generates a method, runtime truth is the `define_method` inside
  `attribute_methods.rb`; trekr answers with `belongs_to :supplier` or the
  schema column, which is what a reader wants (BASELINE, session 15). On
  widget_shop that is 24 of 63 sites, and every one of them scores `wrong@1`
  here. Sorbet pays the same tax for pointing at an RBI declaration.
* **The warm median is one definition request per position across hundreds of
  distinct files**, each preceded by a `didOpen`. It is not the same figure as
  DEC-007's 0.2 ms, which repeated one position.

## The series

| engine | version | date | corpus | sites | answered | correct@1 | found | wrong@1 | setup | cold | warm median | RSS |
| --- | --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| trekr | 0.1.0 (session 27) | 2026-08-25 | discourse | 500 | **93.4 %** | **79.4 %** | **82.0 %** | **14.0 %** | 0.01 s | 0.3 ms | 0.56 ms | **227 MB** |
| ruby-lsp | 0.26.9 | 2026-08-25 | discourse | 500 | 65.0 % | 34.4 % | 34.8 % | 30.6 % | 1.4 s + 28.7 s index | 1.1 ms | 0.70 ms | 935 MB |
| trekr | 0.1.0 (session 27) | 2026-08-25 | widget_shop-nosorbet | 63 | **98.4 %** | **58.7 %** | **61.9 %** | 39.7 % | 0.01 s | 0.2 ms | 0.51 ms | **97 MB** |
| ruby-lsp | 0.26.11 | 2026-08-25 | widget_shop-nosorbet | 63 | 77.8 % | 20.6 % | 22.2 % | 57.1 % | 26.9 s + 15.5 s index | 0.5 ms | 0.21 ms | 414 MB |
| ruby-lsp | **0.27.0.beta5** | 2026-08-25 | widget_shop-nosorbet | 63 | 55.6 % | 23.8 % | 23.8 % | **31.7 %** | 4.2 s + 12.9 s index | 0.3 ms | 0.73 ms | 306 MB |
| trekr | 0.1.0 (session 27) | 2026-08-25 | widget_shop | 63 | **100.0 %** | **58.7 %** | 58.7 % | 41.3 % | 0.01 s | 0.3 ms | 0.49 ms | 114 MB |
| sorbet (as configured) | 0.6.13439 | 2026-08-25 | widget_shop | 63 | 12.7 % | 0.0 % | 0.0 % | 12.7 % | 0.6 s + 12.3 s index | 0.5 ms | 0.26 ms | 171 MB |
| sorbet `--typed=true` | 0.6.13439 | 2026-08-25 | widget_shop | 63 | 58.7 % | 19.0 % | 19.0 % | 39.7 % | 0.5 s + 12.2 s index | 0.3 ms | 0.23 ms | 138 MB |

All rows: app-scope gold sites, seed 12, Apple M2, quiet machine, warm OS page
cache. trekr's index was already on disk (that is the product); the competitors'
setup and indexing are reported because they are not.

### What the first series says

**On real app code, trekr answers 1.4× as often, is right 2.3× as often, and is
wrong less than half as often, in a quarter of the memory.** 79.4 % against
34.4 % on discourse is the largest gap this project has measured, and most of it
arrived this week: session 26's two extractor rules moved trekr's own `correct`
from 42.0 % to 59.2 %.

**ruby-lsp's failure mode is the one PLAN §1 predicted.** It answered 65 % of
discourse's positions and its first location was wrong on **30.6 %** — nearly
half of what it answers. Nothing in the response says which half. That is the
argument for confidence-graded answers, now with 153 wrong locations behind it
rather than one hand-checked example.

**The Rubydex rewrite is going the right way, and it is not close yet.**
0.27.0.beta5 against 0.26.11 on the same corpus: answers less often (55.6 % vs
77.8 %), is right slightly more often (23.8 % vs 20.6 %), and is **wrong far
less often (31.7 % vs 57.1 %)**, in 26 % less memory and a sixth of the setup
time. Trading coverage for precision is the trade this project argues for, so
the direction is a compliment. The level is 23.8 % against trekr's 58.7 % on
that corpus.

**Sorbet is gated on annotation nobody writes.** As widget_shop configures it —
Tapioca-generated RBIs committed, not one `# typed:` sigil — Sorbet answers
12.7 % of positions and gets **none** of them right. Forced to `typed: true` it
answers 58.7 % and gets 19.0 % right. This is DEC-018's finding from the other
side: a repo can be full of RBIs describing its *dependencies* and still have
almost no typed call sites of its own.

**Where trekr is weakest is the corpus built for it.** 58.7 % on widget_shop
against 79.4 % on discourse, because a third of widget_shop's sites are
Rails-generated methods where trekr deliberately answers the macro and this
scoring calls that wrong. The number to watch is discourse's.

## Historical: session 9, a different method

Kept because it is the only "before" this project has, and marked because it is
**not comparable to the series above** — 45 hand-picked positions, hand
adjudication, and an "answered" count rather than scoring against runtime truth.

| | trekr (session 9) | ruby-lsp 0.26.11 |
| --- | ---: | ---: |
| `initialize` → response, cold | 6 ms | 96 s |
| warm `goToDefinition` median | 1.0 ms | 9.6 ms |
| peak RSS after 45 queries | 176 MB | 631 MB |
| positions answered | 19/45, later 44/45 | 33/45 |

The full account, including the three hand-adjudicated disagreements trekr won
and the four it lost, is in [BASELINE.md](BASELINE.md).

## Rubydex, as a library: not scorable, and here is why

The `rubydex` gem now ships (0.4.0, prebuilt `arm64-darwin`), which it did not
when PLAN §8 read the competition. It was examined for a cheap definition API
and does not have one:

* Its ruby-lsp addon is a **linter and formatter**, and it refuses to load
  against anything below `ruby-lsp 0.27.0.beta4`.
* Its `rdx query` Cypher schema has **no call-site node and no call→declaration
  edge**. `REFERENCES` runs `Document → Declaration` and covers **constants**.
  There is no way to ask what a method call at a position dispatches to.

So PLAN §8's read — *"Rubydex does not attribute method calls at all"* — is now
verified against the shipped gem rather than inferred. Rubydex is measured here
only through ruby-lsp 0.27.0.beta5, which is built on it.

## Reproducing

```sh
# one-time: an isolated gem home per engine, so no corpus bundle is touched
GEM_HOME=~/.local/share/trekr-compete/gems      gem install ruby-lsp sorbet sorbet-runtime
GEM_HOME=~/.local/share/trekr-compete/gems-beta gem install --prerelease ruby-lsp:0.27.0.beta5

script/compete.py --engine trekr --engine ruby-lsp \
  --root ~/code/lib/ruby/discourse --sample 500 --out /tmp/compete.ndjson

script/compete.py --engine trekr --engine sorbet --engine sorbet-typed \
  --gold /tmp/trekr-gold-widget.ndjson \
  --root ~/code/lib/ruby/widget_shop --rewrite-root ~/code/lib/ruby/widget_shop \
  --sample 0 --corpus widget_shop
```

The gold sets come from `script/trace_gold.rb` (see BASELINE.md). `--rewrite-root`
moves a gold set between **worktrees of one repo** — widget_shop and
widget_shop-nosorbet differ only by a `sorbet/` directory their app code never
mentions — and moves a truth with it only when that truth lives in the app.

ruby-lsp writes a `.ruby-lsp/` directory into whatever checkout it is pointed
at. It is a tool cache, ignored by git and never indexed by trekr (which reads
`git ls-files`), and deleting it costs the next run a bundle composition.
