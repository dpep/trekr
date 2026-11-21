# How trekr compares

Each engine is asked `textDocument/definition` over LSP at call sites whose
answer Ruby itself recorded: the TracePoint gold set
([BASELINE.md](BASELINE.md#runtime-truth-the-tracepoint-gold-set-session-12)). `script/compare.py` produces every row. Latest
run: **2026-10-02**, trekr after 0.8.4 (then unreleased: DEC-440–445); the other
engines' rows are the 2026-10-01 run's. Not re-run since, so trekr's rows are
that build's, not the current release's.

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
| trekr, unreleased | **88.0 %** | **80.8 %** | **7.2 %** | **82.4 %** | **4.8 s** | **0.01 s** | **0.72 ms** | **390 MB** |
| ruby-lsp 0.26.9 | 63.0 % | 51.4 % | **11.6 %** | 56.8 % | 17 s | 17 s | 1.4 ms | 1,300 MB |

trekr is right more than half again as often (80.8 % vs 51.4 %), wrong less
often (7.2 % vs 11.6 %), in under a third of the memory, and its index
persists, so a restart costs nothing while ruby-lsp re-indexes on every
launch. ruby-lsp is right at 7 sites where trekr is not; trekr at 154 where
ruby-lsp is not. Of the 36 sites where trekr's first location is wrong,
ruby-lsp answers nothing at 27 and is also wrong at 8. trekr declines a call
it could not resolve when its best guess is a weak one (DEC-443); with every
guess shown (`unresolved: peek`), it answers 96.0 % and is wrong 14.4 %.

## mastodon — 500 app-code sites, public

| engine | answered | correct@1 | wrong@1 | found | ready, cold | ready, restart | warm | RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| trekr, unreleased | **83.8 %** | **63.8 %** | **20.0 %** | **67.6 %** | **3.5 s** | **0.01 s** | **0.65 ms** | **290 MB** |
| ruby-lsp 0.26.11 | 72.6 % | 51.2 % | **21.4 %** | 57.0 % | 13 s | 13 s | 0.86 ms | 740 MB |
| ruby-lsp 0.27.0.beta5 (Rubydex) | 67.4 % | 45.2 % | 22.2 % | 52.6 % | 5.5 s | 5.5 s | 1.1 ms | 660 MB |

The same lead, a narrower one, and about the same wrong@1. mastodon calls
far more through untyped receivers — serializer `object`, controller
`params`, association readers — and declares far more of its methods through
Rails (`scope`, `belongs_to`, columns in `db/schema.rb`), which this scorer
counts against an answer that names the declaration: 37 of trekr's 100
wrong@1 are such declarations. ruby-lsp is right at 19 sites where trekr is
not; trekr at 82 where ruby-lsp is not. The Rubydex-based beta answers less
often than 0.26 and is right less often.

## widget_shop — 63 sites, private

| engine | answered | correct@1 | wrong@1 | found | ready, cold | ready, restart | warm | RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| trekr, unreleased | **100 %** | **67 %** | 33 % | **67 %** | 2.0 s | **0.01 s** | 0.43 ms | 260 MB |
| ruby-lsp 0.26.11 † | 78 % | 49 % | 29 % | 54 % | 16 s | 16 s | 0.48 ms | 560 MB |
| ruby-lsp 0.27.0.beta5 (Rubydex) † | 56 % | 35 % | **21 %** | 38 % | 5.4 s | 5.4 s | 0.48 ms | 420 MB |
| Sorbet 0.6.13439, as configured | 13 % | 0 % | 13 % | 0 % | 0.87 s | 0.87 s | 0.32 ms | **140 MB** |
| Sorbet 0.6.13439, `--typed=true` | 59 % | 19 % | 40 % | 19 % | **0.48 s** | 0.48 s | **0.28 ms** | 150 MB |

† Run on a worktree without `sorbet/` and with byte-identical app code. trekr
0.8.4 scored the same there, except found was 65 %.

Over a third of these sites call Rails-generated methods. trekr answers with
the macro (`belongs_to :supplier`), and the scorer wants the `define_method`
inside Rails, so trekr's wrong@1 is high here by construction. No file in this
app has a `# typed:` sigil, so Sorbet as configured resolves no method calls.

## Where trekr's wrong@1 comes from

Every wrong@1 and unanswered site, by root cause (`wrong@1 / none`):

| cause | discourse | mastodon | fix |
| --- | ---: | ---: | --- |
| a Rails-generated method, answered by its declaration (column, `belongs_to`, `scope`, `after_save`) | 16 / 1 | 37 / 0 | by construction |
| untyped receiver, and the first candidate is another class's method | 3 / 29 | 33 / 58 | partly a general rule |
| a call through `delegate`, answered with the method it sends to first (DEC-211) | 6 / 0 | 13 / 0 | by choice |
| the receiver's self or ancestors incomplete (`include Singleton`, `routes.draw do`, `on_load` includes, `db/structure.sql`) | 6 / 13 | 8 / 15 | a general rule, not yet |
| metaprogramming trekr does not model (SiteSetting, route helpers, `enum`) | 3 / 17 | 4 / 8 | by construction |
| resolved, to the wrong method (a `devise` include, a nil receiver) | 2 / 0 | 5 / 0 | mostly by construction |

A residue — trekr unsure — now carries a confidence counted on these gold
sets (DEC-442): 0.7 when the call is on `self` or the name has at most three
definitions, 0.2 otherwise. The LSP returns a residue's guesses only at 0.5
or above (DEC-443), which is why "none" grew: most of it is residue trekr
holds back, of which 36 in 40 on discourse and 59 in 75 on mastodon
would have been wrong.

## Reading the numbers

* **Only the first location is scored.** trekr's `status`, `confidence` and
  `kind` are discarded. A declaration answer, meaning the macro or column that
  defined a method, counts as wrong@1. BASELINE.md scores trekr on its own
  terms.
* **discourse's row predates reading `db/structure.sql`** (DEC-480), and is
  not re-run here. With it, `compare.py --engine trekr` on the same 500 sites
  measured correct@1 81.2 % unchanged and wrong@1 7.0 → 8.8 %: the nine sites
  newly answered are attribute calls answered with their column in
  `db/structure.sql`, which `compare.py` counts as wrong and `gold.py` calls a
  `declaration`. The move is the scorer's, not a regression.
* **A call through `delegate` answers the method it sends to first**
  (DEC-211), and the delegate second. Ruby's trace records the delegate, so
  `Model.where` scores wrong@1 and found.
* **Ready ends at the engine's own `$/progress` end.** Sorbet reports no
  progress, so for Sorbet it ends at its last message before 25 s of silence.
  trekr's cold run starts from an empty store and indexes the app and its
  gems; before that finishes it already answers from what it has read and says
  those answers are partial (DEC-322), which is not counted here. ruby-lsp's
  composed bundle was already installed; a first-ever run that must install
  gems takes minutes.
* **A gem or stdlib file counts as the same file in any install** of that
  version, because ruby-lsp runs from its own gem home.
* **ruby-lsp runs the version the project pins**: 0.26.9 on discourse.
  mastodon pins none, so it gets the newest stable, 0.26.11. 0.27 is still a
  beta. It can't run on discourse honestly, because `--beta` re-resolves all
  300 gems with pre-releases allowed, which moves the files the gold set points
  into. On mastodon its composed bundle adds only ruby-lsp, ruby-lsp-rails and
  rubydex.
* **Sorbet runs in the project's own bundle**, frozen.
* **The gold sets are traced differently.** discourse's comes from a script
  that drives the app (`script/exercise_discourse.rb`); 14 of its 500 sites are
  in `spec/fabricators`. mastodon's comes from 5,850 examples of its own suite
  (models, services, workers, `requests/api`, controllers, serializers,
  policies, validators, presenters and `lib`), and excludes call sites under
  `spec/`, in templates, and calls that landed in an RSpec stub. 178 examples
  failed, about 170 of them because no frontend was built; a failing example
  still records what it ran.
* **mastodon is upstream `2f40549d4`.** The bench corpus at
  `~/code/lib/ruby/mastodon` is a copy of its Ruby files with no `config/*.yml`,
  `bin/` or `.ruby-version`, so it cannot boot. Every one of its files is
  byte-identical to that commit. The engines ran on a full checkout of it.
  trekr scores the same on the corpus copy, within one site.
* **The machine was busy**: Apple M2, 8 cores, load average 5–15. Each timing
  is the median of 4 interleaved runs. Single runs varied by up to ±50 %, and
  ruby-lsp's warm latency by 3×. Compare timings within a table. Accuracy was
  identical in every run.

## Since the 2026-10-01 run

* correct@1 went from 78.0 % to 80.8 % on discourse, 64.2 % to 63.8 % on
  mastodon, and 62 % to 67 % on widget_shop; wrong@1 from 18.0 % to 7.2 %,
  34.6 % to 20.0 %, and 38 % to 33 %.
* Most of the wrong@1 that went is residue the LSP no longer shows (DEC-443):
  with `unresolved: peek`, as before, this build is 96.0 / 81.6 / 14.4 on
  discourse and 98.8 / 67.0 / 31.8 on mastodon (answered / correct@1 /
  wrong@1).
* The rest is typing: a hook's nested include runs its own hook first
  (DEC-440, `sidekiq_options`), a top-level call runs on `main` (DEC-445,
  `require`), and a relation chain is typed step by step (DEC-444,
  `.order`, `.pluck`, `.exists?`). Every site those moved went to correct
  but two, `.where` and `.includes` on a `has_many` reader, where trekr
  names the method CollectionProxy's computed `delegate` sends to.
* Timings are one cold and one restart run, at load 4–7, so compare them
  within a table only.
* Earlier rows are in git history.

## Rubydex as a library

It still can't be scored on its own (checked against `rubydex` 0.4.1). A
`MethodReference` has a name, a location and sometimes a receiver, but no
declaration it resolves to. The only `REFERENCES` edge in `rdx query` runs from
a document to a constant. The ruby-lsp add-on needs ruby-lsp ≥ 0.27.0.beta4,
so Rubydex is measured through that beta.

## `--dead` against Spoom and sorbet-deadcode — discourse, by git history

Latest run: **2026-11-20**, trekr 0.10.2 (`323d7cc`), Spoom 1.8.13,
sorbet-deadcode at `~/code/lib/ruby/sorbet-deadcode`'s HEAD, all on Ruby
3.4.10. `script/dead_compare.py` produces every row. The method is
[BASELINE's session 36](BASELINE.md#dead-code-candidates-validated-against-git-history-session-36):
each tool runs on discourse at `5f3ffdc4a` (2025-08-27), scope `app/models
app/services`; a candidate counts as deleted when today's checkout
(`d55f1dcce56`, 2026-08-25, 6,802 commits later) no longer defines it. The
control is every method in the scope. Candidates are joined to an
inventory of `def`s by `path:line`, so no tool's naming matters. Only
methods are scored.

Two ground truths: **by owner**, the same owner, name and side are still
defined (a method moved to another class counts as deleted); **by name**,
session 36's, a method of that name is defined anywhere. The interval is
Wilson's 95 %.

| set | methods | deleted, by owner (rate, 95 % CI) | lift | deleted, by name | lift |
| --- | ---: | ---: | ---: | ---: | ---: |
| **base rate: every method in scope** | 3,530 | 90 (2.5 %, 2.1–3.1) | 1× | 37 (1.0 %, 0.76–1.4) | 1× |
| trekr: every tier | 1,225 | 49 (4.0 %, 3.0–5.2) | 1.6× | 24 (2.0 %, 1.3–2.9) | 1.9× |
| trekr: `unreferenced` | 156 | 3 (1.9 %, 0.66–5.5) | 0.75× | 3 (1.9 %, 0.66–5.5) | 1.8× |
| trekr: `unreferenced`, `clear` | 32 | 1 (3.1 %, 0.55–16) | 1.2× | 1 (3.1 %, 0.55–16) | 3× |
| trekr: `convention-only` | 285 | 22 (7.7 %, 5.2–11) | 3× | 6 (2.1 %, 0.97–4.5) | 2× |
| trekr: `single-caller` | 704 | 22 (3.1 %, 2.1–4.7) | 1.2× | 13 (1.8 %, 1.1–3.1) | 1.8× |
| trekr: `test-only` | 74 | 2 (2.7 %, 0.74–9.3) | 1.1× | 2 (2.7 %, 0.74–9.3) | 2.6× |
| Spoom | 212 | 8 (3.8 %, 1.9–7.3) | 1.5× | 4 (1.9 %, 0.74–4.8) | 1.8× |
| sorbet-deadcode | 44 | 3 (6.8 %, 2.3–18) | 2.7× | 1 (2.3 %, 0.40–12) | 2.2× |
| sorbet-deadcode: `safe_delete` | 39 | 2 (5.1 %, 1.4–17) | 2× | 1 (2.6 %, 0.45–13) | 2.4× |
| Spoom and sorbet-deadcode | 42 | 3 (7.1 %, 2.5–19) | 2.8× | 1 (2.4 %, 0.42–12) | 2.3× |
| all three (trekr `unreferenced`) | 21 | 1 (4.8 %, 0.85–23) | 1.9× | 1 (4.8 %, 0.85–23) | 4.5× |

**No tool's candidates are told apart from the base rate by this proxy.**
Every interval but two overlaps the base rate's. Discourse deleted 2.5 % of
these methods in a year, so a set of 44 holds about one deletion by chance,
and one more moves its rate by two points. The two that clear the base
rate are both trekr's and neither is a precision claim: `convention-only`
is *more* often deleted by owner (7.7 %), mostly service-object steps
moved between classes (by name, 2.1 %, inside the base interval), and
every tier together, 4.0 %, rides on it.

**Session 36's 19.0 % base rate does not reproduce.** The same commits and
scope give 2.5 % by owner and 1.0 % by name; spot-checked deletions
(`Group#imap_mailboxes`, `ApiKey#at_least_one_granular_scope`) are real.
Its 19.8 % `unreferenced` and 3.3 % `convention-only` came from the build
of `577c996` (2026-08-26) and an instrument that is not in the repo, so which side
differs is not known; BASELINE is left as it stands, and this table is the
one `script/dead_compare.py` reproduces.

### Where the three disagree

Rows matched by `path:line`, every kind; trekr's set is its `unreferenced`
tier (the only one that says nothing names it).

| | trekr | Spoom | sorbet-deadcode |
| --- | ---: | ---: | ---: |
| candidates in scope | 245 (156 methods, 59 modules, 25 constants, 5 classes) | 238 (212 methods, 20 constants, 4 `attr_writer`s, 2 classes) | 60 (44 methods, 16 constants) |
| that tool alone | 89 | 59 | 2 |

All three agree on 35; trekr and Spoom on 121 more; Spoom and
sorbet-deadcode on 23 more; trekr and sorbet-deadcode on none apart.

* **Where Spoom and sorbet-deadcode are wrong and trekr is not.** 57 of
  Spoom's methods and 18 of sorbet-deadcode's 44 (16 of them
  `safe_delete`) are what trekr grades `convention-only`: discourse's
  service DSL (`step :request_challenge`, `policy :invalid_access`) and
  Forwardable's `def_delegator :featured_users, …` name them, and both
  tools miss it. `UserActionManager.topic_rows` and its siblings (4 for
  Spoom, 3 for sorbet-deadcode) are called from a `class_eval` string,
  which trekr reads (testbed 122) and neither other does. `Category::DEFAULT_TEXT_COLORS` is interpolated into a
  rake task's heredoc: trekr does not report it, Spoom calls it dead and
  sorbet-deadcode says `review`.
* **Where trekr is wrong.** Its 59 modules are every concern's
  `ClassMethods`, which ActiveSupport::Concern extends onto its includer
  by name (testbed 913). `PostAnalyzer#has_oneboxes?` is `unreferenced`,
  `clear`, but `Post` sends it from `%i[… has_oneboxes?].each { |attr|
  define_method(attr) { post_analyzer.public_send(attr) } }` (testbed 912);
  sorbet-deadcode reads the literal list. Spoom's 4 `attr_writer`s are a
  kind trekr never reports (testbed 909).
* **Where name-matching hides a candidate.** 8 of trekr's 32 `clear`
  `unreferenced` methods Spoom calls alive only because a method of the
  same name is called somewhere (`UserStat#update_distinct_badge_count`
  beside the class method of that name; `SpamRule::AutoSilence
  #flagged_post_ids` beside `TrustLevel3Requirements`'). sorbet-deadcode,
  verifying by `rg` for the name, keeps 19 of the 32 for the same reason.

### Mapping `--dead --json` onto sorbet-deadcode's `--intersect`

sorbet-deadcode intersects on `[full_name, kind]`, with `Owner#method` for
every method (`spoom/converter.rb` rewrites Spoom's `Owner::method`).
trekr's `owner` and `name` map onto it as `"#{owner}##{name}"`, and `class`,
`module` and `constant` as `"#{owner}::#{name}"`. Matched by location on
this run, the converted name and kind agree with sorbet-deadcode's on 54
of 55 shared rows and with Spoom's on 179 of 226. Almost every miss is one
case: a method in `class_methods do`, which trekr names
`Reports::Bookmarks::ClassMethods#report_bookmarks` (the module Rails
makes) and both others `Reports::Bookmarks#report_bookmarks`. A converter
would lose:

* the side: neither tool tells `def self.x` from `def x` (312 of trekr's
  1,225 method rows are singleton; 4 owners have both);
* `let`, `subject` and `shared_group`, which have no kind there, and
  `attr_reader`/`attr_writer`, which trekr has no rows for;
* the grading: an intersection keeps membership only, so a converter must
  pick the tiers that mean "dead" (only `unreferenced` does) and drops
  `confidence`, `caveat`, `reason` and the entry-point marks with them.

Spoom's own `--spoom` path in sorbet-deadcode does not run against Spoom
1.8.13 (`Definition::Kind#serialize` is gone; it is `to_s` now), so
`script/spoom_dead.rb` calls Spoom's API itself.

```sh
GEM_HOME=/tmp/compare-gems gem install spoom -v 1.8.13      # on Ruby >= 3.4
git -C ~/code/lib/ruby/discourse worktree add --detach /tmp/discourse-old 5f3ffdc4a
COMPARE_RUBY=~/.rbenv/versions/3.4.10/bin/ruby COMPARE_GEM_HOME=/tmp/compare-gems \
  $TREKR/script/dead_compare.py --old /tmp/discourse-old --new ~/code/lib/ruby/discourse \
  --out /tmp/dead-compare
git -C ~/code/lib/ruby/discourse worktree remove /tmp/discourse-old
```

mastodon could not be scored this way: the bench corpus is a one-commit
copy with no history.

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

mastodon runs on its own Ruby, 4.0.6, so its engines need gem homes built for
that ABI, named by `TREKR_COMPARE_GEMS`. Its bundle goes in a path of its own,
and the suite runs against a test database and Redis database of its own
(`TEST_ENV_NUMBER` picks both; the suite deletes every key in its Redis
database). Native gems need `BUNDLE_BUILD__CHARLOCK_HOLMES`, `BUNDLE_BUILD__IDN___RUBY` and
`BUNDLE_BUILD__PG` pointed at Homebrew's icu4c, libidn and Postgres. libvips
comes from the prebuilt `@img/sharp-libvips-darwin-arm64`
npm package, linked as `libvips.42.dylib` on `DYLD_FALLBACK_LIBRARY_PATH`;
`ruby -S bundle` keeps that variable, which `/usr/bin/env` would strip.

```sh
git clone https://github.com/mastodon/mastodon /tmp/mastodon && git -C /tmp/mastodon checkout 2f40549d4
export BUNDLE_PATH=/tmp/mastodon-bundle BUNDLE_FROZEN=true RBENV_VERSION=4.0.6 \
  RAILS_ENV=test DB_NAME=mastodon_trekr_gold TEST_ENV_NUMBER=7
cd /tmp/mastodon && ruby -S bundle install && ruby -S bundle exec ruby bin/rails db:create db:schema:load
TREKR_GOLD=/tmp/gold-mastodon.ndjson TREKR_MAX=200000 \
  TREKR_EXERCISE=$TREKR/script/exercise_rspec.rb \
  TREKR_SPECS="--seed 12 --exclude-pattern spec/lib/mastodon/cli/**/*_spec.rb spec/models spec/services
    spec/workers spec/requests/api spec/controllers spec/serializers spec/policies spec/validators
    spec/presenters spec/lib" \
  ruby -S bundle exec ruby $TREKR/script/trace_gold.rb

GEM_HOME=~/.local/share/trekr-compare/ruby-4.0/gems gem install ruby-lsp -v 0.26.11
GEM_HOME=~/.local/share/trekr-compare/ruby-4.0/gems-beta gem install ruby-lsp -v 0.27.0.beta5
TREKR_RUBY_BIN=~/.rbenv/versions/4.0.6/bin TREKR_COMPARE_GEMS=~/.local/share/trekr-compare/ruby-4.0 \
  TREKR_DB=/tmp/trekr-compare/mastodon.db $TREKR/script/compare.py \
  --engine trekr --engine ruby-lsp --engine ruby-lsp-beta --gold /tmp/gold-mastodon.ndjson \
  --root /tmp/mastodon --sample 500 --exclude spec/
```

widget_shop works the same way as discourse: use
`script/exercise_widget_shop.rb`, `--sample 0`, `--engine sorbet --engine
sorbet-typed`, and `--rewrite-root <worktree>` for the copy without `sorbet/`.
widget_shop isn't published. A ruby-lsp run writes a git-ignored `.ruby-lsp/`
and, through ruby-lsp-rails, bootsnap cache under `tmp/`.
