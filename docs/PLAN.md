
# Ruby code intelligence for agents — research + plan

Drafted 2026-08-23 from five research tracks (rq docs, rwr docs, Ruby LSP/Rubydex,
Sorbet + Rust Ruby parsing, resolution precedents + agent tooling). Sources are linked
inline; **[I]** marks inference.

## 1. The short answer

**Possible: yes. Worth it: yes, but only on three axes — and one of them has a clock on it.**

The problem you describe (huge legacy Rails, many worktrees, metaprogramming, some
Sorbet, agents as the main consumer) is exactly the set of open issues on the incumbent's
roadmap. Shopify rewrote Ruby LSP's indexer in Rust as
[Rubydex](https://github.com/Shopify/rubydex) (ruby-lsp 0.27 beta, Aug 2026;
[announcement](https://railsatscale.com/2026-05-12-one-engine-many-tools/)). It has
parsing, a definitions→declarations graph, constant resolution, ancestor linearization,
RBS, and an MCP server. It does **not** have: a persisted index
([#957](https://github.com/Shopify/rubydex/issues/957)), a real incremental story
([#960](https://github.com/Shopify/rubydex/issues/960)), cross-worktree sharing, DSL /
`define_method` modeling ([#583](https://github.com/Shopify/rubydex/issues/583),
[#958](https://github.com/Shopify/rubydex/issues/958)), receiver inference, or method
references worth the name. A funded team is working on all of these.

So the durable edges are the ones structurally hard for them, not the ones on their list:

1. **Zero-setup, Ruby-free, bundle-free.** Ruby LSP runs *inside the project's Ruby and
   bundle* (composed `.ruby-lsp/` Gemfile, one process per workspace). A static binary
   that indexes any tree — no `bundle install`, no bootable app, trees that don't parse
   under the project's Ruby version — is rq's pitch and Shopify can't easily match it.
2. **Content-addressed index shared across worktrees.** Key per-file facts by git blob
   OID; N worktrees of one repo cost ~1× the index, a new worktree is a `git ls-files -s`
   diff. Research found nobody doing this for a *symbol* index (Zoekt does it for
   trigrams via branch bitmasks; Glean/Kythe cache per-file facts and confirm that
   cross-file *resolution* is the invalidation problem, so keep it a separate layer).
3. **Ranked, confidence-graded answers.** Ruby LSP's fallback for an unknown receiver is
   "the first 10 methods with that name" ([definition.rb](https://raw.githubusercontent.com/Shopify/ruby-lsp/main/lib/ruby_lsp/listeners/definition.rb)); Rubydex "has no
   concept of types." For an agent, a ranked list with `confidence` and `why` is the
   product. This is rq's DNA and rwr's measurements (below) say how far the ladder goes.

**The clock:** persistence and DSL modeling are Rubydex's stated next steps. Twelve to
eighteen months from now edge #2 may be partly closed; #1 and #3 stay.

What *not* to compete on: type checking, completion, formatting, editor rename, semantic
tokens. Claude Code's `LSP` tool exposes exactly nine operations — goToDefinition,
findReferences, hover, documentSymbol, workspaceSymbol, goToImplementation,
prepareCallHierarchy, incomingCalls, outgoingCalls — plus diagnostics pushed after edits.
That is the whole surface an agent-first server needs.

## 2. What the research settled

### Ruby LSP / Rubydex

- Old indexer (≤0.26): in-memory, never persisted; Shopify core ≈ 90 s and 2.2 GB
  ([sabbatical post](https://railsatscale.com/2025-07-01-a-ruby-open-source-sabbatical/)).
  Issues: 1 h / 3–5 files/s on gem-heavy apps ([#1316](https://github.com/Shopify/ruby-lsp/issues/1316)),
  50–80 GB during find-references ([#3258](https://github.com/Shopify/ruby-lsp/issues/3258)).
- Indexes all gems by default; one process per workspace folder; worktrees ⇒ N indexes.
- Resolution: `TypeInferrer` handles literals, `self`, constants, `.new`, else
  **GuessedType** from the variable name (`user` → `User`, first unqualified match).
- References/rename: **constants only**; methods open since 2025
  ([#3111](https://github.com/Shopify/ruby-lsp/issues/3111)). rwr's source read (Q9):
  `ReferenceFinder` matches methods by bare name, no `SymbolNode`/string visitors so
  `send`/`define_method`/`delegate`/`alias_method` are invisible, `*_spec.rb` silently
  excluded, ~35 s per find-references on a 40k-file repo.
- Sorbet interplay: detected via `sorbet-static` in the bundle; `typed: true+` files skip
  parts of definition resolution in favour of Sorbet.
- ruby-lsp-rails: spawns `rails runner` for columns/associations/routes — exact, but needs
  a bootable app + DB per worktree, and no scopes.
- Rubydex MCP (`rdx mcp`): `search_declarations`, `get_declaration`, `get_descendants`,
  `find_constant_references`, `get_file_declarations`. Note the absence of method
  references.

### Sorbet LSP

- C++ whole-program; fast path / slow path. At Stripe, ~10% of edits still go slow path
  and **class/module definition edits always do** ([jez](https://blog.jez.io/making-sorbet-more-incremental/)).
- `typed: false` files: hover/def/refs/rename "disabled in most cases"
  ([docs](https://sorbet.org/docs/lsp-typed-level)). Method references need a typed
  receiver in a `typed: true+` file. Metaprogramming only via Tapioca RBIs — go-to-def
  on an AR attribute lands in `sorbet/rbi/dsl/*.rbi`, not the model.
- Single input directory only, `sorbet/config` from cwd, multi-root open since 2020
  ([#2496](https://github.com/sorbet/sorbet/issues/2496)). `--cache-dir` is an LMDB cache
  of parse output keyed by path, not designed for cross-worktree sharing.
- `scip-ruby`: alive (last commit Jul 2026) but experimental, inherits every limit above.

### Parsing from Rust

- **`ruby-prism` 1.9.0** is the answer: vendored C, no Ruby toolchain to consume, 200+
  node `Visit` trait, diagnostics, comments, magic comments, **local-variable scope
  tracking during parse** (identifier vs call disambiguation tree-sitter guesses at).
  Used by Rubydex, rubyfmt, rwr. `tree-sitter-ruby` is stale (Nov 2024, nested-heredoc
  and regex bugs open); `lib-ruby-parser` archived; stack-graphs archived Sep 2025.
- RBS: `ruby-rbs` 0.3.0 (C bindings, soutaro). Consume opportunistically; never require.
- LSP framework: `lsp-server` (rust-lang, Jul 2026, sync loop you own) — recommended.
  `tower-lsp` is dead; `async-lsp` if tower middleware is wanted. `salsa` usable (ty is
  built on it) but churning; not needed if the tree layer is cheap to rebuild.

### What rwr already measured (docs/internal/decisions.md D61, D62)

Across rails, discourse, mastodon:

- Chained receivers are 15.8–27.4 % of call sites, but `X.new` is under 4 % of chains.
- Only **2.3–4.5 %** of method definitions have a syntactically resolvable return type;
  70 % end in another call. Syntax alone does not reach chained receivers.
- 20–25 % of chains are `expect(...)` — spec DSL, not navigation targets.
- Implicit self is the largest slice at **53–66 %** and needs no inference at all: the
  enclosing class is the receiver.
- **Sorbet `sig`s name a usable class for 64 % of signatures** vs 3.9 % from syntax —
  16× the yield, read straight from the Prism tree, zero cost on repos without them.

That is the precision ladder, with numbers: implicit self → locals from constructors /
identity methods → ivars / constants / `self` → sigs → RBI → residue. rwr already
implements the first four (`src/pattern/matcher.rs` `resolve_type`,
`src/hierarchy/`, `src/sigs.rs`).

### Agent-side evidence

- Agents use definition, references, symbol overview, structure-addressed edits.
  Completion/formatting/semantic tokens: never. ([Serena](https://github.com/oraios/serena),
  [engines.dev](https://www.engines.dev/blog/code-navigation),
  [CODESTRUCT](https://arxiv.org/abs/2604.05407) +1.2–5 % SWE-Bench Verified and 12–38 %
  fewer tokens from AST-addressed read/edit; weak models gain most.)
- Nobody publishes precision or latency numbers for Ruby. That gap is a measurement we
  can own (§5).
- Claude Code plugin config: `lspServers` with `command`, `args`, `extensionToLanguage`,
  `startupTimeout` (default 5 s — blocks routing until the server responds), `diagnostics`
  (default on). One server per extension. Official `ruby-lsp` plugin exists.

## 3. rq, rwr, or a new repo?

**New repo.** Three reasons, each sufficient:

- rq's first principles are *language-agnostic core*, *no daemon*, *fork-per-query under
  50 ms*. This engine is Ruby-only by construction (Prism, Rails DSLs, Sorbet), needs a
  cross-file semantic layer, and has to be resident to speak LSP. Putting it in rq
  breaks three of rq's rules; rq's ROADMAP already declined `--refs` and LSP on exactly
  this reasoning, and it was right *for rq*.
- rwr is a rewrite tool with no index by design (D5). It is the **seed**, not the home:
  `src/pattern/generated.rs` (3.8k lines of generated per-node Prism traversal),
  `hierarchy/`, `sigs.rs`, `resolve_type`, the mmap + rayon + gitignore walker, the
  "total account" discipline (every occurrence ends up labelled).
- The question shapes differ. rq answers **name → definition** (fuzzy, ranked,
  polyglot). This answers **position → meaning** and **definition → references**
  (exact, Ruby, semantic). Keep both; let each call the other where it helps
  (`workspaceSymbol` can delegate to rq's scorer; rq's Ruby plugin can later consume
  this engine's facts).

Mechanics: single crate to start (rq's own rule), copy rwr code with a pointer to the
source file rather than extracting a shared crate until a second consumer exists. Name
and path are open questions (§7).

## 4. Architecture sketch

```text
                 ┌──────────── fronts ─────────────┐
   rbi CLI --json │ LSP (lsp-server, stdio) │ skill │     ← 9 ops + syntax diagnostics
                 └───────────────┬─────────────────┘
        ┌───────────────────────▼─────────────────────────┐
        │ 3. resolve + rank   position→node (reparse 1 file)│  receiver ladder, candidates,
        │                     confidence, --explain, residue│  learned picks (maybe)
        ├───────────────────────────────────────────────────┤
        │ 2. tree layer       per checkout @ tree-hash      │  path→blob map, constant
        │                     rebuilt lazily from facts     │  namespace, ancestors (MRO),
        │                     memoized per namespace        │  method tables, DSL expansion
        ├───────────────────────────────────────────────────┤
        │ 1. blob layer       blob_oid → facts (SQLite WAL) │  defs, params/arity, visibility,
        │                     pure function of content,     │  singleton, mixins, superclass,
        │                     shared by every worktree      │  const refs, call sites with
        │                     Prism via rayon               │  receiver shape, macros, sigs
        └───────────────────────────────────────────────────┘
```

- **Blob layer.** `git ls-files -s` gives path→OID for 100k files in ~100 ms; dirty files
  are hashed the way git does (`sha1("blob <len>\0" + bytes)`) so uncommitted content keys
  consistently. Facts are a pure function of bytes, so branch switches, rebases, and new
  worktrees only parse what is genuinely new. Prism parses ~40 MB/s/core; a 500 MB tree
  is ~15 s cold on 8 cores, and cold happens once per machine. Gems are blobs too,
  keyed by `(gem, version)` and shared across every project on the machine.
- **Tree layer.** The Glean/Kythe lesson: per-file facts cache perfectly; the cross-file
  graph is where invalidation bites (and where Sorbet's slow path lives). Keep it cheap
  to rebuild rather than clever to patch: a class's ancestors depend only on the blobs
  that declare it and its ancestors, so memoize per namespace on the set of contributing
  blob OIDs. No salsa unless measurement demands it.
- **Resolution.** The rwr ladder, extended: implicit self → local (constructor /
  identity / literal) → ivar / cvar / constant / `self` → inline `sig` → Tapioca
  `sorbet/rbi/dsl` and `sorbet/rbi/gems` → static Rails (`db/schema.rb` columns by
  `table_name`; `has_many`/`belongs_to` families; `scope`, `enum`, `delegate`,
  `alias_attribute`, `store_accessor`, `attribute`; `included do` / `class_methods do`)
  → **ranked residue**: name + arity + same-file / same-namespace / ancestor proximity,
  with `confidence < 1` and a `why`. Never the bare "first 10."
- **Tapioca is the metaprogramming shortcut.** In any repo with Sorbet, `sorbet/rbi/dsl/`
  already enumerates generated AR attributes, associations, enums, and more — Ruby
  syntax, Prism-parseable, zero runtime. Attribute those definitions to the model file
  (Sorbet itself lands you in the `.rbi`). Static `schema.rb` covers the non-Sorbet case.
- **Runtime never serves, only measures.** No `rails runner` in the request path. The
  app may be booted once, offline, to *label* a gold set (§5).
- **Daemon policy.** The engine stays daemon-free (state on disk, any process can answer).
  The LSP process is a thin resident front holding open-file overlays and the tree-layer
  cache; one per workspace as Claude Code requires, all sharing the blob DB over WAL.
  This preserves rq's principle without fighting the protocol.

## 5. Measurement gate — before building the interesting part

The go/no-go is a number, not a feeling (measure-first).

- **Corpus.** Public proxies with test suites: discourse (11k Ruby files), mastodon, and
  GitLab (~40k, the largest OSS Rails monorepo). Plus the real target if it can be
  benchmarked locally (§7 Q1).
- **Gold set for free.** Run the corpus test suite once under `TracePoint(:call)` with
  `caller_locations` and record (call site → resolved method definition). Tens of
  thousands of labelled pairs, no hand labelling, and it captures metaprogrammed
  dispatch by construction.
- **Metrics.** top-1 / top-3 accuracy on go-to-definition; find-references precision +
  recall against the same pairs; unresolved rate; p50/p95 latency; index size and time
  per 10k files; cost of adding a second worktree.
- **Baselines.** ruby-lsp 0.26.x and 0.27 (Rubydex), Sorbet where the repo is typed,
  `rq` and `rg` as the floor.
- **Gate.** Proceed past Phase 1 only if the ladder beats Ruby LSP top-1 on the gold
  set by a margin that survives noise, or Ruby LSP is unusable on the target (memory /
  time) and ours is not. Either is a go; neither is a stop.

## 6. Phases

Each ends in something runnable; earlier phases don't assume later ones.

**Phase 0 — Spikes and the gate (1–2 weeks)**
- Harness: corpus checkout, TracePoint labeller, baseline runner for ruby-lsp 0.26/0.27.
- Spike A: can the `rubydex` crate (MIT, crates.io) be driven from Rust without a Ruby
  runtime, and does its model admit persistence? If yes, it may replace half of Phase 1.
- Spike B: blob-OID indexing on the largest corpus — parse throughput, DB size, cost of
  `git ls-files -s` diff, cost of a second worktree.
- Decide: new repo name/path, rubydex-as-library or not, copy-vs-extract from rwr.

**Phase 1 — Blob layer + CLI (2–3 weeks)**
- Prism extraction of the fact set above; SQLite store keyed by OID; rq identity model
  (`github.com/org/repo` dedupe) and store conventions.
- CLI with `--json`/`--ndjson`: `outline FILE`, `def FILE:LINE:COL`, `refs NAME`
  (name-level, honest confidence), `status`. Exit codes as rq.
- Exit: whole-corpus index, incremental reindex after a branch switch touches only
  changed blobs, second worktree ≈ free.

**Phase 2 — Tree layer + resolution (3–5 weeks)**
- Constant resolution (lexical nesting + ancestors), MRO, method tables, singleton
  classes; the receiver ladder through `sig`s; ranked residue with `--explain`.
- References narrowed by receiver; `incomingCalls`/`outgoingCalls` from call-site facts.
- Exit: beats the Ruby LSP baseline on the gold set (the gate).

**Phase 3 — Rails + Sorbet sources (2–3 weeks)**
- Tapioca `rbi/dsl` + `rbi/gems` ingestion; static `schema.rb`; the Rails DSL family;
  gem indexing keyed by `(gem, version)`.
- Exit: metaprogrammed AR attributes and associations resolve to the model; measured
  lift on the gold set's dynamic-dispatch pairs.

**Phase 4 — LSP front + Claude Code (1–2 weeks)**
- `lsp-server` over stdio mapping the nine operations; Prism syntax diagnostics;
  `lspServers` plugin entry; skill for the CLI path; `startupTimeout` respected by
  answering from the persisted index before the tree layer is warm.
- Exit: Claude Code's `LSP` tool answers on the target repo; goToDefinition on a
  `has_many` accessor lands in the model.

**Phase 5 — Scale hardening (ongoing)**
- The 100k-file target: watch `.git` for ops, overlay dirty buffers, memory ceiling,
  contention with a concurrent indexer, multi-session worktrees.
- Learned ranking only if rq's 2026-10-01 kill-criterion says the signal is real.

Rough total to a usable, measurably-better engine: ~3 months of focused solo work,
front-loaded on measurement. rwr's seed and rq's store/identity code are what make the
middle phases weeks rather than months.

## 7. Open questions

1. **The target repo.** File and gem counts, Ruby/Rails versions, share of `typed: true`
   files, whether `sorbet/rbi/dsl/` exists (Tapioca), and whether it can be benchmarked
   locally or only via public proxies. This sets Phase 0's corpus and decides whether
   Tapioca ingestion is a Phase 3 shortcut or moot.
2. **Consumer priority.** The plan builds CLI-first (rq-style, works everywhere agents
   run) and adds the LSP front in Phase 4 because Claude Code's `LSP` tool and post-edit
   diagnostics need it. Reverse the order if the LSP tool is the primary surface.
3. ~~**Rubydex: compete or build on it?**~~ **Answered by the spike (§8): lift, don't
   depend, don't wait.**
4. **Residue policy.** Ranked candidates with `confidence < 1` (recommended for agents)
   versus rwr's strict refuse-and-report. Both can coexist behind a flag; which is the
   default?
5. **Gems in Phase 1 or Phase 3?** Legacy repos answer "where is this defined" from
   gems constantly; indexing them is also most of Ruby LSP's 2.2 GB. Content-addressing
   makes them cheap after the first time; the question is whether they gate the first
   usable version.
6. **Runtime for measurement only** — confirm booting the app offline to label the gold
   set is acceptable, and that nothing in the serving path may depend on a runtime.
7. **Name and location** (`~/code/lib/rust/<name>`), and whether to extract rwr's Prism
   pieces into a shared crate now or copy first (recommended: copy, extract at the
   second consumer).
8. **Relationship to rq going forward.** Does rq's Ruby plugin eventually consume this
   engine's facts, or stay tree-sitter and independent? Affects whether the blob store
   schema should be rq-compatible from day one.

## 8. Rubydex spike (2026-08-23)

Cloned `Shopify/rubydex` at `d7c7656` (0.2.6 crate, 0.4.0 gem), built the Rust CLI, and
wrote a 50-line Rust driver against the crate — **no Ruby toolchain involved**. Machine:
8-core Apple Silicon, warm cache, release build.

### What it is

- Pure-Rust `rlib` (`ruby-prism`, `ruby-rbs`, a Cypher parser); the Ruby gem is a thin
  FFI over it (`rdx_graph_new / resolve / free` is the whole `-sys` surface). The MCP
  server, linter, `index_workspace` (Bundler gem discovery), and the fuzzy search all live
  on the **Ruby** side.
- Two phases: per-document **indexing** into a `LocalGraph` (a pure function of
  `(uri, source)`), then a global **resolution** worklist that mints declarations, resolves
  constants, linearizes ancestors. IDs are deterministic hashes (`DeclarationId` from the
  FQN, `DefinitionId` from uri+offset+name) — persistence-friendly by construction.
- Indexes: class/module/singleton, `def` (params, visibility, `def self.`/`def Foo.`),
  constants, ivars/cvars/gvars, `attr_*`, `alias`/`alias_method`, include/prepend/extend,
  `module_function`, `private_constant`, constant aliases; RBS core/stdlib/workspace.
  **Not**: `define_method`, `delegate`, `scope`, `has_many`, `Struct.new`, `Class.new`.
- `MethodRef` = name + location + receiver **only if it is a constant**. Method
  references are collected and never resolved (`resolution.rs` has no code path for
  them). No position→node query; no ranking; no Sorbet anywhere (zero issues mention
  sigs or RBI — their direction is RBS).

### Measurements

| corpus | files | lines | cold index | cold resolve | RSS | edit one file → re-resolve |
|---|---:|---:|---:|---:|---:|---|
| rails | 3.3k | 0.5M | 0.66 s | 0.69 s | 144 MB | `AR::Base` +class: 43 ms + 380 ms |
| discourse | 11.3k | 1.3M | 1.75 s | 0.63 s | 315 MB | `User` +class: 156 ms + 87 ms |
| all `~/code/lib/ruby` | 34k | — | 3.8 s | 1.6 s | 846 MB | — |
| GitLab | 31.4k | 3.2M | 8.0 s | 3.6 s | 706 MB | `User` +class: **687 ms + 980 ms** |

Two things the table hides:

- **A no-op re-index still costs a resolve pass** — 177 ms on rails, 845 ms on GitLab for
  an unchanged file. The maintainer's own issue [#960](https://github.com/Shopify/rubydex/issues/960)
  calls the invalidation "fundamentally flawed" (no no-op skipping, full traversal per
  change, cascades). Confirmed.
- **Reference volume**: GitLab holds 3.08M method refs and 664k constant refs for 168k
  definitions — 18 refs per def. rq's "refs are ~10× defs" estimate was low.
- Indexing runs on all cores but wall ≈ user time: the serial main-thread merge is the
  bottleneck, so more cores won't help the cold path much.

Linear extrapolation to the target (10M lines ≈ 3.1× GitLab, ~100k files): **~35 s cold,
~2.2 GB resident per process, ~5 s per edit of a central model, ~10M reference rows** —
per worktree, per tool, rebuilt at every boot. Shopify's own persistence prototype
([PR #1009](https://github.com/Shopify/rubydex/pull/1009), rkyv snapshot) came in at a
2.4 GB archive with a 14 s write and was sent back as "too broad to review."

### Roadmap state

The three issues that matter — persistence [#957](https://github.com/Shopify/rubydex/issues/957),
incremental redesign [#960](https://github.com/Shopify/rubydex/issues/960), DSL IR
[#958](https://github.com/Shopify/rubydex/issues/958) — were opened together on
2026-07-28/29 as a post-LSP-migration reset. All labelled `hard`, all unassigned, zero
discussion. Ten releases in 105 days, but the recent ones are linter/Cypher/config, not
these. Worktrees: no issue. Ranking/confidence: no issue. Sorbet: no issue.

### Decision: lift, don't depend, don't wait

- **Don't wait.** What is plausibly 12 months out is per-workspace snapshot persistence
  and some DSL modeling. What is not on any list: cross-worktree sharing, ranked or
  confidence-graded answers, Sorbet sig/RBI ingestion, Ruby-free operation (their
  consumers are the gem, Ruby LSP, and a Ruby MCP server that needs `bundle install`).
  Those are the actual requirements.
- **Don't depend on the `Graph`.** It is the in-memory whole-program model that a 10M-line
  repo punishes, its API is `pub` but 0.2.x-experimental, and every change we need
  (content-keyed facts, receiver shape on `MethodRef`, DSL expansion, persistence) is a
  change to *their* model — i.e. a fork with extra steps.
- **Lift, with attribution (MIT):**
  - `docs/ruby-behaviors.md` — an 1,800-line spec of Ruby's naming, scoping, mixin,
    visibility, alias, and singleton semantics. This is the conformance document we'd
    otherwise spend weeks rediscovering.
  - `ruby_indexer_tests.rs` (5.4k lines) + `resolution_tests.rs` as a conformance corpus
    to port; `graph.rs`'s incremental tests as the invalidation edge-case list.
  - The **Name** model (`str + parent_scope + nesting`) and the worklist constant
    resolution in `resolution.rs`; the definition/declaration split; deterministic hashed
    IDs; ancestor linearization order `[prepends, self, includes, superclass]`.
  - Optionally, `indexing::index_source` / `LocalGraph` as a **Phase 1 prototype** of the
    blob layer to validate the content-addressed design in days rather than weeks — then
    replace it with our own extraction that carries receiver shape and DSL facts.
- **Contribute upstream where cheap** and off the critical path (e.g. receiver shape on
  `MethodRef`) — goodwill, and it makes a future re-convergence possible.

## 9. Cost–benefit: should we build it?

### Costs

- **Build**: ~3 focused solo months to a measurably-better engine (Phases 0–4), with the
  usual stretch for hardening. Opportunity cost: that time doesn't go to rq/rwr.
- **Maintenance**: a semantic engine tracks Ruby grammar (delegated — Prism is Shopify's
  parser and CRuby's default), Rails DSL drift, and Sorbet/RBS formats. Bounded but real,
  and it lands on one person.
- **Competitive**: Shopify has a funded team on Rubydex; the honest risk is not that they
  ship our tool but that "good enough" per-workspace persistence shrinks the pain gap for
  ordinary repos. The 10M-line/many-worktree case stays outside their model regardless.
- **Capped downside**: Phase 0 is 1–2 weeks and its main artifact — the TracePoint gold
  set + baseline harness — is independently useful (it can benchmark *any* Ruby nav tool,
  including ruby-lsp 0.27 against 0.26 on the target repo). If the gate fails, that's the
  total spend.

### What it buys over existing tools, on the actual target

(10M lines, ~30 % Sorbet, many worktrees, agents as primary consumers)

| Incumbent | On the target repo | Ours |
|---|---|---|
| ruby-lsp 0.27 / Rubydex | ~35 s cold + ~2.2 GB **per worktree, per tool, per boot** (extrapolated §8); ~5 s per central-file edit; method refs unresolved; needs project Ruby + `bundle install` | index once per machine (blob-addressed), then ms-scale; refs receiver-narrowed with confidence; static binary, no bundle |
| Sorbet LSP | covers the typed 30 %; class edits → slow path; one workspace; refs need typed receivers; metaprog answers land in `.rbi` files | whole repo incl. `typed: false`; consumes the same sigs/RBIs but answers *at the model*; per-worktree cheap |
| rq / rg | name-level only — no resolution, no references, no hierarchy | position→meaning, refs, MRO; rq stays the fuzzy name front |
| Serena / MCP wrappers | bounded by the server underneath (= ruby-lsp) | is the server underneath |

The agent-side payoff is the quantified one: structure-addressed navigation cuts tokens
12–38 % and lifts SWE-Bench 1.2–5 pts (CODESTRUCT); every wrong goto-definition an agent
follows costs a file-read-and-retry loop. At monorepo agent volume that is a daily,
compounding tax — and precise references on *untyped* Ruby (impact analysis, safe rename,
"who calls this") exist in **no** current tool.

**Recommendation: yes, gated.** Run Phase 0 against the real repo (or GitLab as proxy):
if ruby-lsp 0.27 is unusable there (memory/boot × worktrees) or our ladder beats its
top-1 on the gold set, proceed; otherwise stop having spent two weeks and keep the
harness. The two failure modes this guards against: building on momentum (rq's own
ROADMAP discipline), and discovering post-hoc that "good enough" already existed.

## 10. Dead-code candidates (Rubydex) — assessment

`dead_code_candidates` merged 3 days ago ([#1010](https://github.com/Shopify/rubydex/pull/1010)):
every constant-like declaration (class/module/constant/alias) with **zero resolved
constant references** — methods excluded *by design* (rubydex can't attribute method
calls, so called and uncalled both read zero). Working-set semantics: delete, re-index,
call again until empty.

Measured on discourse (11k files): 3,580 candidates in **0.7 ms** after the 1.3 s
index+resolve. Composition: 315 anonymous (`Class.new`), and of 3,265 named, **2,442
(75 %) are migrations** — invoked by runtime convention, never referenced. Spot-checking
the 787 non-migration survivors: scheduled jobs (directory discovery), settings
validators (`"#{setting}_validator".camelize` — the constant is *constructed*, `rg`
finds nothing either), scorables (registry/inheritance). So the reference precision is
fine; what's missing is **Rails-convention knowledge**, and that's a filter layer, not
an engine change.

Read: genuinely useful primitive, same shape as the reaper skill's bucketing but computed
in milliseconds. Two implications:

- **For other projects today**: the 50-line Rust driver from the spike + a convention
  filter (migrations, jobs, validators, serializer/strategy registries) is a usable
  constant-level dead-code tool *now* — a weekend, no engine required. Method-level dead
  code remains out of reach until something attributes method calls to declarations.
- **For the engine**: dead-code becomes one cheap query over the same facts — and with
  receiver-attributed method refs it extends to methods, which is where legacy-repo debt
  actually lives. Ship it as `<tool> dead [--scope DIR]` with rwr-style full-disclosure
  buckets (convention-invoked / string-referenced / spec-only / truly-unreferenced).
  Worth noting: `rdx query` (Cypher), `rdx lint`, and the skill registry show Shopify is
  also aiming Rubydex at agent tooling — another reason the differentiated layers
  (persistence, worktrees, method attribution, ranking) are the right ground to hold.
