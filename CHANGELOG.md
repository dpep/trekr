# Changelog

## Unreleased

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
