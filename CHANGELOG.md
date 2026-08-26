# Changelog

## 0.1.3 — 2026-08-25

- **A query keeps itself honest about freshness.** `--def` probes git in O(1) —
  one stat of `.git/index` — and when the checkout has moved it **re-reads the
  file you asked about** before answering, so a definition that shifted lines is
  found at its new line without any explicit reindex. The answer carries
  `index: {stale, refreshed, hint}`, and the text surface says it in a line.

  **Bounded on purpose**: one file, whatever the repo size. A full scan is
  145 ms on discourse and ~6 s on a 10M-line monorepo, and neither can sit on a
  query path — so the rest of the index is left alone and *disclosed* as
  possibly lagging rather than silently trusted. `trekr --index` remains the
  only thing that declares a whole checkout fresh.

  **What the probe cannot see**, stated rather than left to be discovered: an
  edit that nothing has told git about does not move `.git/index`. Both limits
  are pinned by tests.

  **Existing databases reindex once** — the schema gained a column.


- **A no-op `--index` no longer rewrites the file map.** Every index deleted and
  re-inserted one row per file whether or not anything had changed — O(files) of
  pure cost on a repeat run. The checkout now stores a `map_key` folded over
  (path, blob oid); an identical key means an identical map and the rewrite is
  skipped.

  Measured on discourse (11,301 files), steady state: the `store-write` phase
  falls from **42 ms to 1 ms**, and a no-op index from **185 ms to 145 ms**. On
  rails, store-write 78 ms → 0 ms. What is left of a no-op is the scan, ~94 % of
  which is git's untracked-file discovery.

  **Existing databases reindex once** — the schema gained a column.

## 0.1.2 — 2026-08-25

- **An unindexed repo now answers `not_indexed`, not residue.** A query into a
  checkout the store has never seen names the repo root, gives the `trekr
  --index` command, and exits 2 — instead of `status: residue` with "no indexed
  constant", which read as a finding about the code when it was a setup step.
  **Anything scripted against residue-as-missing-index must read the new
  status.**

- **`--def` snaps to the nearest name on the line, and says so.** An off-by-one
  column answers for the nearest identifier with `snapped_to: {name, col,
  alternatives}` in JSON and a stderr note in text; `--def FILE:LINE` (no
  column) now works. Exact positions answer exactly as before, and the LSP
  server keeps exact-position semantics — editors send real columns.

- **Human output shows `$HOME` as `~`.** Every text surface — results,
  `--explain`, `--status`, `--usage`, errors. `--json`/`--ndjson` keep absolute
  paths, and LSP URIs are untouched.

## 0.1.1 — 2026-08-25

- **`--serve` is now `--lsp`, with no alias.** The flag says what it does. **You
  must edit any hand-written editor or MCP config** that spawns `trekr --serve`;
  plugin users get it with the plugin update. Pre-1.0 this project takes the
  clean break over a compatibility shim, and the changelog line is the whole
  migration.

  The request log moves with it: `~/.local/share/trekr/serve.log` →
  `lsp.log`, which `--usage` reads. To keep your history, `mv` it — nothing else
  refers to the old name.

- **The skill checks for the binary before it tries to use it.** First report
  from a second machine: the plugin was installed, `/trekr` was run, and the
  skill neither noticed `trekr` was missing nor helped install it — while the
  plugin's LSP server, which is `trekr --serve`, failed silently for the same
  reason. The skill now opens with the install (`brew install
  dpep/tools/trekr`, or `cargo install trekr` without Homebrew — verified
  against crates.io), says the LSP comes up at the next session start, points at
  the per-repo `trekr --index`, and separates "no results" from "broken" with
  `trekr --status`.

  Skill only; the binary is unchanged, so there is no crate or formula release
  in this. It ships by bumping the **plugin** version in the myclaude
  marketplace, because `claude plugin update` compares versions and not content
  — which is exactly why the gap reached a second machine at all.

- **A Sorbet stub answers `kind: declaration`, with `defined_via: rbi`.** An
  `.rbi` `def` is bodiless by construction — an ordinary definition by every
  syntactic test, and a description of a method that runs somewhere else.
  `Site::is_rbi` has said exactly that since DEC-019 ("an `.rbi` is a
  declaration, never an implementation"); `kind` shipped without asking it.

  Rare by design rather than by luck: DEC-019 makes real source win the whole
  ancestor chain before a stub wins any of it, so a stub only answers when it
  is all there is. Measured on widget_shop, the one corpus that commits
  `sorbet/rbi/`: **0 of 63 app sites and 9 of 400 gem sites** — nine answers
  that used to claim the body was there. No canonical figure moves; discourse
  is byte-identical.

## 0.1.0 — 2026-08-25

First public release.

trekr answers two questions about Ruby that grep cannot: **which method does
this call site actually run**, and **which call sites can actually reach this
method**.

- **`--def FILE:LINE:COL`** resolves a position by walking Ruby's own
  constant-lookup ladder — enclosing lexical scopes, the innermost scope's
  ancestors, then the top level. 82 % of rails constant references resolve
  (78 % on discourse), and every answer carries `status`, `confidence`,
  `resolved_via`, and whether the location is the code or the macro that
  declared it.
- **`--refs 'Owner#method'`** tiers call sites by whether the receiver can
  actually reach the method: **confirmed** (the receiver's type resolves and
  lookup lands here), **possible** (untyped receiver, nothing rules it out,
  ranked and never dropped), **excluded** (counted, not listed, auditable with
  `--include-excluded`). Across twelve heavy-collision names on rails —
  25,297 same-name call sites — 32 % confirmed, 43 % possible, 24 % excluded.
- **`--ancestors`**, **`--symbols`**, **`--status`**, **`--index`**, `--drop`,
  `--usage`, and `--explain`, all with `--json`/`--ndjson` and meaningful exit
  codes, because the intended caller is an agent.
- **`--serve`** speaks LSP over stdio: goToDefinition, findReferences,
  documentSymbol, workspaceSymbol, hover, goToImplementation, call hierarchy,
  and Prism syntax diagnostics.
- **`--completions <shell>`** prints a shell completion script, generated from
  the parser so it cannot drift from the flags.

A server whose binary has been replaced retires by exiting rather than by
closing its own stdin. Closing the descriptor turns the reader's blocking read
into EOF on macOS but not on Linux, where the process would log its retirement
and then hang holding the stale build.

Facts are keyed by git blob OID, so every worktree of a repo shares one index
and a reindex with nothing changed parses nothing: 1.5 s cold on rails, 61 ms
for a no-op reindex, ~0.2 s and zero parses for a second worktree. Ruby core
and the checkout's gems are indexed; gems are shared across every project
resolving the same `(name, version)`. No Ruby toolchain, no `bundle install`,
no bootable app.

## Pre-release development

What follows is the development log from before the first release — written as
deltas against the working tree of the day, not against any published version.
Kept because the reasoning is worth having; skip it if you want the shipped
behavior, which is above.

- **`define_model_callbacks` is modelled**, so `before_save`, `after_destroy`
  and the rest of ActiveRecord's model callbacks resolve. The macro sits in an
  `included do` block, which is `class_eval`'d into the includer, so its
  class-level methods are routed to the concern's `ClassMethods` — where
  Concern already puts an includer's class methods — and the module is emitted
  when the concern does not declare one itself. `only:` is honoured; a computed
  `only:` generates nothing rather than inventing the other two.

  Built and turned down in session 29 because the answers had to be scored as
  errors; they are now disclosed as declarations and score as such. 114
  discourse app sites, **112 answered, 0 confidently wrong**.

- **An answer says which kind of location it handed back.** `--def --json` and
  `--explain` carry `kind: definition | declaration`, and `defined_via` names
  the macro when it is a declaration (`belongs_to`, `enum`, `schema`,
  `delegate`, an alias, a bare `private :foo`). The store has always known this
  — a `def` row's `via` records what made it — and the answer never said, so a
  caller could not tell `belongs_to :supplier`, the line a reader wants, from
  the line Ruby runs.

  The test is **"is the body at this location"**, not "was a macro involved": a
  literal `def` is a definition, and so are `define_method`'s block, which *is*
  the body, and `module_function`'s copy, which points at the `def` it copied.
  Residue candidates carry their own `kind` too.

  On the LSP side it lives in **hover**: `textDocument/definition` is a bare
  list of locations and has nowhere to put it.

- **A mixin written inside a `def` is no longer an ancestry edge of the scope
  that lexically contains it.** It runs when the method runs, against whatever
  `self` is then, so recording it lexically does not merely miss an edge — it
  invents one. Rails writes `include ActiveModel::Validations` inside
  `has_secure_password`, in a `ClassMethods` body, which put that module's
  `alias_method :validate, :valid?` into the class-level lookup chain of every
  ActiveRecord model: a class-body `validate :thing` resolved, confidently, to
  the instance alias instead of `ClassMethods#validate`. It stays an ordinary
  call site, because `include` really is `Module#include`.

- **`class_methods do … end` opens the concern's `ClassMethods` module.**
  `ActiveSupport::Concern` creates `M::ClassMethods` from the block form and the
  nested-module form alike, and extends it into every includer. Leaving the
  block unmodelled put its methods on the concern as *instance* methods, where a
  class-body call cannot reach them — and a mixin written inside it became an
  instance-side edge of the concern rather than a class-side one of every
  includer. On discourse that single shape is **28 % of all declined app sites**;
  `correct` on real app code goes **43.4 % → 59.2 %** and residue-with-the-truth
  -offered **41.6 % → 25.3 %**.

- **An `includer`-rung answer that picked among competitors is `ambiguous`.**
  A call written inside a module is answered by asking the classes that mix it
  in; when two of them define the name in different places, the rung still
  reported `resolved` — DEC-027's rule applied to the receiver-name rung and
  never to this one. It now says `ambiguous` and lists the definitions it beat.
  Measured: discourse app confidently wrong **0.6 % → 0.2 %**, gem floor
  **4.0 % → 3.3 %**, with `correct` and `found` byte-identical on both.

- **A method whose name is computed from a literal array is extracted.**
  `[:before, :after, :around].each { |c| define_method "#{c}_action" … }` is how
  actionpack writes `before_action`, and how ActiveRecord writes its model
  callbacks — nothing that reads only the `def` keyword can see them. Scoped
  tightly: a literal array, `each`, one block parameter, and a name whose only
  interpolation is a bare read of that parameter. A constant array, a second
  interpolation, or `#{n.to_s}` all generate nothing, because a name
  half-guessed is worse than a name not offered — the lookup would find it and
  stop. The definition's location is the `define_method` call, which is where
  the method is written.

- **Existing databases reindex once**: the extractor's output changed.

- `--index` gathers full statistics (`ANALYZE`) after a run that actually read
  something. `PRAGMA optimize` on close only re-analyses a table whose size has
  moved since the last analysis, which never fired across hundreds of checkouts
  accumulated a few at a time — so the planner was working from statistics
  thirteen rows old. Reported as an `analyze` phase under `--profile`.

- **Fixed: retirement detected a replaced binary and then never left.** Breaking
  out of the request loop is not enough — `IoThreads::join` waits for the reader
  thread, which is parked in a blocking read on stdin, and an editor holds stdin
  open for as long as it is running. The server sat there having logged
  `retire`, still holding the old build: the exact symptom retirement was
  written to remove, now with a log line claiming it had worked. It closes the
  descriptor before leaving, which turns that read into EOF.

- `--def --context CHECKOUT` answers a position as if asked from that checkout.
  Only meaningful inside a **gem**, which is otherwise answered from whichever
  app most recently indexed it — a pick that follows your work, which is right
  for a person and wrong for a measurement (DEC-029).

- `--usage` reports the **first request of a session apart from the rest**. That
  request pays for a cold page cache and a tree build; blending it into the
  median made the headline a measure of the disk rather than of trekr. On the
  real log it moves `definition` from a reported **415 ms median to 88 ms**,
  with the five session-openers shown separately at 451 ms.

- Residue candidates are ordered by **directory affinity**: a definition that
  shares directories with the call site ranks above one across the tree — the
  "same file" signal, graded instead of binary. Measured against the candidate
  pool as it exists after gem context: truth ranked first **49.6 % → 52.8 %**,
  MRR 0.648 → 0.666. `correct` and `confidently wrong` are unchanged, which is
  what a ranking feature is allowed to do. `TREKR_RANK_OFF=affinity` switches it
  off for re-measurement.

- The namespace fixpoint revisits only the declarations whose placement can
  still change. A declaration written with plain names is placed by string
  arithmetic alone, so round one settles it forever; only a **compact path**
  (`class A::B`), whose prefix goes through constant lookup, can move. On
  discourse that is 9,100 of 69,305 declarations, and the fixpoint falls from
  **97 ms to 46 ms** (rails 22 → 9 ms). The assembled namespace is
  byte-identical on rails, discourse and widget_shop.

- **A position inside a gem is answered from an app that resolves it.** A gem
  is indexed as a checkout of its own, so on its own it is a tree of one gem
  plus Ruby core — and every method it gets from a sibling gem was unreachable
  by construction (DEC-029). `--index` now records which gems a bundle
  resolves, and a query inside gem source picks the **most recently indexed**
  app that has it. The answer carries `context` naming the checkout that
  answered, and `--explain` prints it; a gem no indexed app resolves keeps the
  one-gem tree and says so by naming itself.
- A tree reads its gem roots from the store instead of re-locating them on
  disk. Locating gems means a lockfile and ~200 stats against `GEM_HOME` and
  friends, and doing it per query made the tree depend on the environment the
  query ran in — a query with a different `GEM_HOME` than the index silently
  lost every gem. Gem roots are also canonicalized at index time now, like
  every other checkout root.
- **Existing databases reindex once**: the schema gained the gem-ownership map.

- `owner` now names where Ruby's lookup actually landed. A method reached
  through a `self.table_name` override reported the *carrier* class the
  convention invents (`LegacyPost`) — a name no code declares and no agent can
  look up. It reports the model. For every other method the two were already
  the same.

- `--serve` retires itself when its binary is replaced. After each request it
  checks whether the executable on disk is newer than the one it is running;
  if so it finishes the answer, logs a `retire` event and exits cleanly, so the
  editor spawns the new build. A server answering with a stale binary until
  somebody remembers to kill it is silent staleness, which is the bug class
  this engine hunts everywhere else. `--usage` counts the retirements.
- The serve log's `start` event records which binary it is running.

- **Methods are loaded on demand, by name.** A tree no longer fetches and
  indexes every method in the checkout and its gems — 84,052 of them on rails,
  which was 76 % of the build. It loads the names a query actually asks about.
  Measured: rails tree build **310 ms → 73 ms**, discourse **643 → 259**; a
  rails `--def` **0.31 s → 0.09 s**, `--refs` **~0.40 s → 0.13 s**, and the LSP
  first query **508 ms → 85 ms** (discourse 975 → 272). Accuracy is unchanged —
  the gold set is identical before and after.

- `tests/testbed/` — ten accumulated corner cases as drop-in fixtures with one
  iterating harness, so adding the next costs no Rust: the ancestor cycle that
  killed the process, a Sorbet stub shadowing real source, resolved-vs-ambiguous
  receiver-name pairs, the same-file path boundary, macro-as-call, the delegate
  prefix, finder typing, and the exclusion count `--refs` exists for.

- An `ambiguous` answer now lists the definitions it beat. Competitors are what
  made it ambiguous, so showing them is the disclosure, not a hedge — and only
  residue used to carry a candidate list. `--explain` renders them with the
  reason; this checkout's own code ranks before a dependency's.

- `--profile` now reports where a **query's** time went, not just an index's:
  the tree build's phases, on stderr. `TREKR_PROFILE=1` does the same for a
  process that cannot pass the flag. This is what showed that methods are 76 %
  of a rails tree build.

- `--def --explain` renders the disclosure `--json` has always carried: the
  rung that resolved the receiver, the confidence and what graded it, the
  ancestors that could not be seen, and the ranked candidates behind a residue
  with the reason each ranked where it did. Promised in PLAN and CLAUDE.md
  since the start; only the rendering was missing. Every line restates a field
  of the answer, so the two surfaces cannot drift.

- **`status` gains `ambiguous`**, the third value the docs always promised. A
  `receiver_name` answer says `resolved` only when the name is the whole story;
  when other classes define the same method it says `ambiguous` (DEC-027).
  `@account.local?` was `resolved` at 0.03 confidence and is now `ambiguous`;
  `@widget.supplier_region` stays `resolved · 0.5`. Exit codes treat
  `ambiguous` as a match.
- Confidence is rounded where it is built, to the precision two counts carry —
  `0.03`, not `0.03225806451612903`.

- `goToImplementation` on a **method** now answers with its overrides. It only
  ever understood class and module names, so standing on an abstract method
  returned nothing. Asked the way Ruby answers it — for every type carrying the
  owner, whose definition actually wins — so it finds an override in a *sibling
  module* (`SQLite3::DatabaseStatements` beside the abstract one), which a
  subclass search misses. `write_query?` on Rails' abstract adapter now returns
  the SQLite3, PostgreSQL and MySQL definitions.
- `callHierarchy/incomingCalls` names each caller by the method it sits in
  (`Job#run`, `Job.sweep`) instead of by the callee's owner, which was the same
  string on every row. It also asks about the method the item names rather than
  the bare name, which is what the `confirmed` tier needs — without an owner it
  could not confirm anything.

- `trekr --usage` summarizes what `--serve` has been asked, from its own log:
  calls per operation, how often the answer was empty, and median/p90 latency.
  Honors `--json`/`--ndjson` like every other command. The log was written to
  debug a defect; this is the other half of why it exists.

- A receiver named after its class is now **typed** by that name, not merely
  ranked by it: `@widget.supplier_region` resolves to `Widget`'s delegate when
  nothing else typed the receiver. Reported as `resolved_via: receiver_name`,
  with confidence graded by the ambiguity it resolved — never the 1.0 of a rung
  that read the answer out of the code. Three corroborations are required, and
  a competing definition in the enclosing scope's own chain blocks it.
  Measured: app-code sites promoted from offered to resolved **61 %**, with
  **no new confidently-wrong answers on either corpus**.

- **Fixed a crash.** `--def` aborted with a stack overflow on some positions in
  a Sorbet-covered checkout. Resolving a constant *path* asks for a name's
  ancestors, and that name can be one already being linearized — at which point
  `ancestors` began a fresh recursion and the per-call cycle guard could not see
  it. The real instance was `File` → `IO` → `IO::EAGAINWaitReadable` → `File`,
  from Ruby core plus committed RBIs. Re-entry now answers empty, as a
  visible cycle already did.

- Four path-comparison bugs fixed, all one shape — a prefix or suffix test with
  no boundary (DEC-026): a checkout claimed files in a sibling whose name
  extended it (`widget_shop` vs `widget_shop-nosorbet`, both present here); the
  same-file ranking signal matched `b.rb` against `ab.rb`; `checkout_containing`
  used SQL `LIKE`, where `_` is a wildcard; and a git-sourced gem claimed a
  checkout of any gem whose name extended its own. Comparisons now go through
  audited helpers whose tests use real absolute paths.

- Two named signals now order residue candidates: **the receiver's name**
  (`@widget.foo` ranks `Widget#foo` first — a convention, so it ranks and never
  promotes) and **this checkout before its dependencies**. Measured on the
  no-Sorbet corpus, where the true definition was offered: app-code first-place
  50 % → 67 % (6 of 9), gem 60 % → 64 % (34 of 53), MRR 0.62 → 0.76 and
  0.72 → 0.76. `--def` says which signal fired in each candidate's `why`.
- Fixed: the "same file" ranking signal compared a checkout-relative path
  against an absolute one and could never fire.

- Real source now wins the **whole ancestor chain** before an `.rbi`
  declaration wins any of it. Tapioca describes methods in owners that do not
  exist at runtime (`Widget::CommonRelationMethods`), and those sit early in
  the chain, so a stub won the lookup outright even after the same-owner rule.
  `Widget.find` answered from the RBI where Ruby dispatches to
  `ActiveRecord::Core::ClassMethods`. Measured on a Sorbet-covered app: correct
  42.9 % → 46.0 %, confidently wrong 7.9 % → 4.8 %.

- A local assigned from an ActiveRecord finder is now typed: `w = Widget.find(id)`
  makes `w` a `Widget`, so calls on it resolve. Reported as `resolved_via:
  finder`, not `sig` — it is a convention, not a declaration, and it is the last
  rung tried. `where`/`all`/`order` are excluded: they answer with a relation.
  Measured across rails, discourse and mastodon: 4,333 assignments of this shape
  would newly type **12,005 call sites**, about half the reach of the `.new`
  rung already shipped.

- An `.rbi` is a declaration, never an implementation: a method with real
  source and a Sorbet stub now answers with the source. An app that commits
  `sorbet/rbi/gems/` holds a stub for every gem method it calls, and those beat
  the gem itself — `belongs_to` landed on `activerecord@8.1.3.1.rbi` instead of
  `associations.rb`. Measured on app code: correct 19 % → 43 %, confidently
  wrong 32 % → 8 %.

- A Rails macro is now recorded as a **call site** as well as a generator of
  the methods it implies. `belongs_to`, `has_many`, `scope`, `delegate`,
  `after_save`, `attr_reader`, `private` — asking what any of them is used to
  answer "no name at this position", because consuming the macro swallowed the
  call with it. Measured on widget_shop's app code, that was the single largest
  miss: 12 of 63 call sites, in a Rails class body that is mostly macros.
- **Existing databases reindex once** (DEC-013): the extractor emits more.

- Fixed: a definition resolving into a **different checkout** — a gem, almost
  always — was handed back rooted on the repo being asked about, naming files
  that do not exist. `@account.local?` in mastodon offered
  `mastodon/lib/prism/string_query.rb`; the real definition is in the prism
  gem, and mastodon has no `lib/prism`. A tree spans a repo and every gem it
  resolves, so a site's path is now absolute from the store outward rather than
  relative to a checkout the caller has to guess.

- A position inside **gem source** now answers. Gems are indexed but are not git
  repositories, so `repo_root` could not place a file in one and `--def` (and
  the LSP surface) refused with "not a git repository" — one step after
  following a definition into a gem, which is where an agent routinely is. The
  checkout is now the file's git repo *or*, failing that, the longest indexed
  root containing it.
- An older trekr meeting a **newer** database now refuses instead of dropping
  it. A version mismatch reindexes (DEC-009), which is right in one direction
  only; in the other, a stale install silently destroyed a newer index and then
  looked like it had never been run.

- Fixed: a `--serve` session went on answering from a tree assembled **before**
  an edit that had been reindexed underneath it. The rebuild key was (schema
  version, file count), and editing a file moves neither — only adding or
  removing one did, which is what hid it. The key is now the checkout's
  *surface*: every file's path folded together with a digest of the
  tree-relevant facts of its blob, computed once at index time and read as one
  row per request.
- A blob now carries a `surface` digest — its definitions and ancestry, the
  only facts the tree layer reads. Measured over 5,158 modified blobs across
  500 commits of rails, discourse and CRuby, **71 % of edits leave it
  unchanged**, so most edits need no tree rebuild at all.
- **Existing databases reindex once**: the schema gained those two columns.

- `--symbols FILE` parses the file instead of querying the index. It was the
  one query verb that did not — `--def` and `--refs` both reparse so an
  unindexed edit still answers — so an outline could be stale, and on a repo
  nobody had indexed it printed `no symbols … (indexed? try --index)`. Now any
  readable Ruby file outlines, in a repo or not, matching the LSP surface
  (DEC-024). Exit 1 is reserved for a file that really defines nothing.

- `concerning :Name do … end` is a module definition and an `include`, so it
  now emits both: the block's methods own themselves as `Enclosing::Name`
  rather than landing on the class, and the class reaches them. Measured yield
  on the bench corpora is near zero — three occurrences across rails,
  discourse and mastodon, two of them inside Rails' own test for the feature —
  so this is correctness for apps that write the idiom, not a win here.
- `delegate … prefix:` now defines the prefixed name — `prefix: true` takes the
  `to:` target (`supplier_region`), a symbol is used as written. It used to
  refuse the whole delegation rather than guess; the rename is a rule Rails
  follows exactly, so there was nothing to guess. A *computed* prefix is still
  refused. 24 of the 301 delegations in rails, discourse and mastodon carry a
  prefix, and none of them was modelled before.
- **Existing databases reindex once** (DEC-013): the extractor changed.

- Fixed: `--serve` answered **nothing at all** for a file outside the client's
  workspace root — which is every file, when the client is Claude Code and its
  root is whatever directory the session started in. The session now holds a
  tree per checkout and finds the one a file belongs to (DEC-024). Outlining a
  file and reporting its syntax errors need no checkout at all now, and work on
  a loose `.rb` that is in no repository.
- `workspaceSymbol` searches every indexed checkout when the client's root is
  not one of them, rather than answering nothing.

- Fixed: `--def FILE:LINE:COL` resolved against the **current directory's**
  checkout rather than the file's own. Asking about another repo's file — which
  is what an agent does constantly — silently answered `residue`, because the
  tree it consulted had never heard of that file. The unit is now the file's
  enclosing repository, whatever directory the process is standing in.

- `trekr --serve` logs what it did, as ndjson: the client's `initialize` root,
  one line per request with the file, line, duration and **how much came back**,
  and the notifications. Default `~/.local/share/trekr/serve.log` (beside the
  database, so `$TREKR_DB` moves it too); `TREKR_LOG` takes a path, `-` for
  stderr or `off`, and `--serve --profile` (or `TREKR_LOG_LEVEL=debug`) adds the
  wire-level params. Never stdout — that is the LSP wire.

- `goToDefinition` returns ranked candidates when the receiver does not
  resolve, up to five, ordered by proximity — the answer the CLI always gave
  and the LSP surface was discarding. `hover` at the same position reports
  `Residue` and `confidence: 0.00`, so a guess is legible as one.
- Core definitions now have a location: `core.rb` is written beside the
  database, so `require` and `Array#each` land on a readable stub instead of
  answering nothing.
- A model overriding `self.table_name` gets that table's columns.
- Measured, with discourse's bundle installed: the chain-truncated bucket
  **disappeared** (24 of 120 samples → 0 of 60), `self` inside a class went
  52 % → 83 %, and overall resolution 31 % → 43 %. Session 5 could only confirm
  the gem hypothesis negatively; this confirms it positively.
- Measured: goToDefinition coverage on the baseline's 45 positions went
  **19/45 → 44/45**, against ruby-lsp's 33/45. Details and the hand-adjudicated
  losses in `docs/BASELINE.md`.

- `trekr --serve`: LSP over stdio. goToDefinition, findReferences (confirmed
  ordered before possible), documentSymbol, workspaceSymbol, hover,
  goToImplementation, call hierarchy, and Prism syntax diagnostics. The editor
  owns the process — no auto-spawn, no lockfile. Completion, rename,
  formatting and semantic tokens are deliberately not announced.
- Warm latency on rails: goToDefinition **0–1 ms** (463 ms first call, which
  builds the tree), documentSymbol and hover **0 ms**, references **25 ms**
  against ~245 ms for the same query on the CLI.

- Rails class macros now define methods in the index: `delegate` (including
  `delegate(*CONST, to: :x)` where the constant is a literal symbol array in
  the same file), the association family, `scope`, `class_attribute`,
  `mattr`/`cattr` accessors, `attribute`, `store_accessor`, `alias_attribute`.
  A singular association's reader carries a **type**, so `belongs_to :user`
  makes `user` a typed receiver.
- A concern's nested `ClassMethods` now reaches the class that includes it —
  `ActiveSupport::Concern` extends it with no `extend` ever written, so it is a
  tree fact by construction.
- Measured: on the same twelve heavy-collision names, `--refs` confirmed rose
  **32 % → 47 %** and the weak `no_such_method` exclusion reason fell from 82 %
  of exclusions to 42 %. `--def` on rails rose 39 % → 42 %.
- **Existing databases reindex once** (DEC-013): the extractor changed.

- `--refs 'Owner#method'` narrows references by receiver: **confirmed** (the
  receiver's type resolves and Ruby's lookup lands here), **possible** (untyped
  receiver, ranked by proximity, never dropped), and **excluded** — not listed
  but counted, because that count is what a grep cannot produce.
  `Owner.method` asks the class-method question instead, and a bare name keeps
  the whole-mention view with each call site now naming the owner it reaches.
- Measured on rails over twelve heavy-collision method names: of 25,297
  same-name call sites, **32 % confirmed, 43 % possible, 24 % excluded** —
  where `rg -w` returns all of them undifferentiated. A refs query costs
  360–400 ms, of which 210 ms is the tree build.
- `--refs --include-excluded` lists the ruled-out sites with their reason, so
  the count is auditable rather than asserted. Exclusions are reported by
  reason, because only one of the three is positive evidence (DEC-021).

- Three new receiver-typing rungs: `sig:param` (a parameter's declared class,
  from the `params(...)` half of a signature), `literal` (`out = []` is an
  Array), and `sig:step` (one call on an already-typed local, and only one).
- A method whose only definition is a Tapioca `sorbet/rbi/dsl/` file now
  answers with the **model**, not the `.rbi`, and reports `resolved_via:
  rbi_dsl`.
- Constants a declaration implies but nothing declares — `ActivityPub` in
  `class ActivityPub::TagManager`, which Rails' autoloader creates from the
  directory — now resolve, carrying no sites because nothing declares them.
- `make bench` gained mastodon and graph_weaver, excludes `sorbet/` from
  sampling, and splits method residue by whether the ancestor chain was
  complete. Measured: **0 of 71 chain-truncated call sites resolve**, which
  confirms the gem hypothesis without needing an installed bundle.

- Measured, after core and gems: **98 % of rails constant references resolve**
  (82 % before this session), 91 % discourse, 84 % CRuby. Method resolution
  reached 38 % on rails from 27 %, all of it from core — gems added nothing
  measurable, because the limit has moved from "is it in the index" to "can we
  type the receiver". Details and caveats in `docs/ARCHITECTURE.md`.
- Tree rebuild is now 202 ms on rails (was 120 ms), because it assembles the
  gems too. `--profile` and `make bench` both report it.

- Gems are indexed. `trekr --index` reads `Gemfile.lock`, locates each gem by
  convention, and indexes its `lib/` once per machine — shared by every project
  that resolves the same version. No `bundle`, no `gem`, no Ruby (DEC-016).
  `--no-gems` skips it.
- A gem the lockfile names but disk does not have is **reported**, in the text
  output and as `gems.missing` in `--json`. Path-sourced gems are not counted,
  because their code is inside the checkout already.

- Ruby core is now indexed. `puts`, `raise`, `block_given?`, `Foo.new`,
  a class body's `prepend`, `ArgumentError`, `ENV` and the rest resolve,
  because every class now carries its implicit `Object → Kernel → BasicObject`
  tail and singleton lookup continues into `Class → Module`. Core comes from a
  vendored Ruby stub read by the ordinary extractor (DEC-015), so no RBS gem
  and no Ruby toolchain.
- `--ancestors` output now ends in the core tail, which is real. A module
  still gets none, because a module has no superclass.

- `--jobs N` (and `TREKR_JOBS`; the flag wins) sets the parse worker count.
  `0`, the default, picks the machine's **physical** core count rather than
  rayon's default of logical cores.
- `--index --profile` reports where the time went — per-phase wall time, blobs
  parsed vs already known, bytes read, parse throughput, and the slowest files.
  Human-readable on stderr, and structured on stderr when `--json` is on, so
  `trekr --index --json --profile | jq` still sees only the answer.
- Calls written inside a module now resolve through the class that mixes the
  module in. `ActiveRecord::Transactions#destroyed?` finds `Persistence`
  because `Base` includes both; confidence is the share of mixing-in classes
  that agree, disclosed as `"1/3 includers"`.

- Fixed: a bare call in a class body was looked up as an instance method.
  `self` in a class body is the class, so `validates :name` and `prepend Foo`
  dispatch on it. "Is a `def` here a singleton method" and "what is `self` for
  a call here" are different questions and were sharing one flag.
- `--def` on a call now reports `receiver_kind` and `unresolved_ancestors`.
  A miss inside a **module** is expected rather than a failure — the module is
  not the real receiver, whatever includes it is — and a miss below an
  unindexed gem ancestor is a weaker "no" than one below a complete chain.
- The cache version now covers changes to **what the extractor emits**, not
  just the schema (DEC-013). Facts are keyed by blob OID, so an extractor fix
  otherwise ships dead against an already-indexed repo. **Existing databases
  reindex once.**

- Method resolution at `--def` (`resolve/`): the receiver ladder in
  measured-yield order — implicit/explicit `self`, constant receivers, locals
  and instance variables typed from their assignments, then inline Sorbet
  `sig` returns. An undetermined receiver returns ordered candidates with the
  receiver shape as the reason, never a bare list.
- Singleton chains in the tree layer: `def self.x`, `class << self`, and
  `extend` all feed one lookup that walks the *superclass* chain (included
  modules contribute no class methods) inserting each level's singleton
  methods and extended modules.
- Method tables, keyed by (owner, singleton, name), with arity.
- `call_site` gains a `singleton` column: the same source line means a
  different lookup inside `def self.x` than inside `def x`. **Existing
  databases reindex once** (DEC-009).

- Tree layer (`tree/`): a checkout's constant namespace and ancestor
  linearization, assembled from blob facts. Ruby's own lookup ladder — lexical
  scopes, then the innermost scope's ancestors, then the top level — with
  path segments descending through ancestors only. Constant aliases are
  followed wherever a namespace is wanted.
- `--def FILE:LINE:COL`: what is the name at this position and where is it
  defined. Reparses the one file, so it answers correctly on an unindexed edit.
  Constants resolve exactly; a method call is honest residue carrying its
  receiver shape.
- `--ancestors NAME`: the linearized chain, with anything unresolvable named
  rather than dropped.
- Measured: 82 % of rails constant references resolve (78 % discourse, 73 %
  CRuby), and every unresolved one names a gem or a core class that is not
  indexed — none is a resolver bug. A whole-checkout tree rebuild is 43 ms for
  rails, 73 ms for discourse. `make bench` reproduces both.
- A schema change now drops the database and reindexes instead of migrating
  (DEC-009). The store is a cache of a pure function; **existing databases will
  reindex once** on first use of this version.

- `--refs NAME`: every mention of a name in a checkout — definitions, constant
  references, and call sites — each disclosing what sort of mention it is and,
  for a call, the receiver's shape. Name-level; narrowing is the resolve
  layer's job.
- Fixed: `trekr … | head` panicked instead of exiting, because Rust ignores
  SIGPIPE.
- Fixed: `--refs` on a common name took 90 s. The store now runs
  `PRAGMA optimize` on close, without which SQLite plans the join as a nested
  scan.
- Measured: rails (3.3k files) indexes cold in 1.5 s and reindexes in 61 ms
  with nothing parsed; discourse (11.3k) in 3.2 s / 121 ms; CRuby (7.9k) in
  2.4 s / 98 ms. A second worktree costs ~0.2 s and zero parses. Reproduce with
  `make bench`; caveats in `docs/ARCHITECTURE.md`.
- Scaffolded the crate: single binary `trekr`, modules mirroring PLAN §4's
  layers, `script/check.sh` as the commit gate.
- Blob-layer extraction (`extract/`): Prism reads one blob's bytes into
  definitions, ancestry edges, constant references, and call sites carrying
  receiver shape. Semantics lifted from Shopify's Rubydex (MIT); the crate is
  not a dependency.
- Checkout scan (`scan/`): `git ls-files -s` for tracked blob OIDs, git's own
  `sha1("blob <len>\0" + bytes)` for anything the working tree has changed, so
  an uncommitted edit keys the same as it will once committed.
