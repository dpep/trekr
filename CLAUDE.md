# trekr development conventions

`trekr` is a **Ruby code-intelligence engine** — position→meaning and
definition→references for massive legacy Rails monorepos, agent-first. Read
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) before anything else: the layers,
what is built, and how it was measured. [docs/DECISIONS.md](docs/DECISIONS.md)
carries what was considered and why. [docs/PLAN.md](docs/PLAN.md) is the
original research and phase plan — historical now, but still where the read on
the other engines (Ruby LSP / Rubydex / Sorbet) lives. Keep the docs in sync
with the code in the same commit, rq/rwr style.

## First principles

- **Measured, or it didn't happen.** Every precision or performance claim traces to
  a run (the TracePoint gold set, the bench corpora). Negative results get written
  down so they aren't re-proposed. Numbers keep the sig figs they earned.
- **Three layers, strictly separated** (PLAN §4): blob layer (facts as a pure
  function of content, keyed by git blob OID, SQLite WAL), tree layer (per-checkout
  assembly: constants, MRO, method tables — cheap to rebuild, memoized), resolve+rank
  (receiver ladder, confidence, explain). Cross-layer leaks are the failure mode
  Glean/Kythe warn about.
- **Full disclosure, ranked.** A definition answer carries a `status`
  (`resolved`, `ambiguous`, `residue`, or why there is no answer yet), and one
  that names a definition says how it got there (`resolved_via`) and how sure
  it is (`confidence`). Residue still returns ranked candidates with the
  reason. Nothing silently dropped, nothing silently promoted.
- **Ruby-free, bundle-free, daemon-free engine.** Prism (`ruby-prism` crate) parses;
  no project Ruby, no `bundle install`, no bootable app. Resident processes (a
  future LSP front) are thin fronts over the on-disk state, never the owner of it.
- **Agent/script-friendly CLI** (rq house rules): `--json`/`--ndjson` everywhere,
  stable field names, meaningful exit codes, `--explain`.

## Lifting from neighbors

- **Rubydex (MIT, with attribution)**: `docs/ruby-behaviors.md` is the conformance
  spec; `ruby_indexer_tests.rs`/`resolution_tests.rs` are the corpus to port; take
  the Name model (str + parent_scope + nesting), worklist constant resolution,
  ancestor order `[prepends, self, includes, superclass]`. Do NOT depend on its
  `Graph` (in-memory, unpersisted — PLAN §8).
- **rwr** (`~/code/lib/rust/rwr`): Prism node machinery (`src/pattern/generated.rs`),
  `hierarchy/`, `sigs.rs`, `resolve_type`, mmap+rayon walker. Copy with a pointer to
  the source, extract a shared crate only at a second consumer.
- **rq** (`~/code/lib/rust/rq`): store/identity conventions, CLI affordances,
  DECISIONS.md discipline. No schema compatibility required.

## Navigate with rq

Find definitions in this repo with `rq`, not `rg`: `rq resolve_at`,
`rq 'Store::init'`, `rq --symbols src/tree/mod.rs`. `rg` is for free text.
When rq misses or ranks the definition you meant below #1, note the query and
what you expected in your report, so it reaches rq's miss log.

## Toolchain

Rust, single crate until there's a concrete reason to split. `cargo` is keg-only:
`/opt/homebrew/opt/rustup/bin/cargo` (or add to PATH). Gate before commit:
`script/check.sh` — `cargo fmt --check`, `cargo clippy --all-targets -- -D
warnings`, `cargo test`, then the VS Code extension's unit tests (`npm test` in
`editors/vscode`, installing its deps on first run).

**Every file is read through `scan::read_source` or `scan::read_text`**:
bounded, regular files only, because a checkout's file may be a pipe or a
link to `/dev/zero`. `clippy.toml` refuses `std::fs::read` and
`read_to_string`; a test file allows them at its top.

**Before a release, and after touching `src/serve/` or `editors/vscode/`:**
`script/check.sh --e2e` (or `TREKR_CHECK_E2E=1`, or `make vscode-test`), which
adds the extension's e2e suite in a real VS Code against the debug build. It
downloads VS Code once, so it is not in the commit gate — and that is how its
hover assertion sat red on `main` through a release. `release` runs
`script/check.sh` as its gate, so `export TREKR_CHECK_E2E=1` in the local
`.release.conf` makes every release run it.

**Never hand-copy a dev build over `/opt/homebrew/bin/trekr`.** It is Homebrew's
symlink into the Cellar; replacing it with a real file breaks `brew link` at the
next release, which someone else then has to repair. Verify against
`target/release/trekr` directly — every measurement script here already takes a
`TREKR_BIN`. If a dev build genuinely has to be the one on `PATH`:

```sh
brew unlink trekr   # …verify…   then:   brew link trekr
```

## Testing

- **Corner cases go in `tests/testbed/`** — a directory of Ruby files plus an
  `expected` file, picked up automatically by one harness. Adding a case is
  dropping in files, no Rust. See `tests/testbed/README.md`; the rule that
  matters is that every case is checked against a build with the fix removed,
  because a case that passes both ways is worse than none.
- **An extraction change bumps the store version** (`schema::VERSION`).
  `tests/extraction.golden` holds each testbed input's stored facts, hashed;
  output that moves without a bump fails, and a bump says to regenerate with
  `UPDATE_GOLDEN=1 cargo test --lib extraction_matches_its_golden`. A new
  testbed input fails too, until that regeneration records it (no bump).
- **`tests/json-shapes.golden` pins the `--json` output's shape**: every
  field path the testbed's answers reach, per command, and the JSON types
  seen there. A field that changes type or vanishes fails the testbed; when
  the change is meant (or only adds fields), regenerate with
  `UPDATE_GOLDEN=1 cargo test --test testbed`.
- Fixture repos under `tests/fixtures/`, generic names (`Widget`, `HandlerA`) —
  public repo, nothing employer-identifying.
- Verify through `cargo test`, not hand-run binaries; e2e drives the built binary
  with an isolated DB env var and a temp repo.
- Bench corpora (large, real): `~/code/lib/ruby/rails`, `~/code/lib/ruby/discourse`,
  `~/code/lib/ruby/mastodon`. Ranking/scale checks belong there, not in unit tests.
- Accuracy is `script/gold.py` against the TracePoint gold set
  ([BASELINE.md](docs/BASELINE.md)) — widget_shop's, and a gem's own suite via
  `make gold-gem GEM=…`; the other engines are scored by `script/compare.py`
  against the same sites over LSP ([COMPARISON.md](docs/COMPARISON.md)), which
  shows the latest run only — a re-run replaces the tables; git keeps the old.

## Landing changes

Solo repo: no PRs, commit directly to `main`, small logically-connected commits,
behavior or structure but not both. `CHANGELOG.md` gets its entry in the commit
that earns it, under `## Unreleased`.

**Fix rounds take fixes; features take their own branch and hunt.** A fix
round accepts only a change that makes a hunt finding pass. A new code path —
a probe, a JSON field, a flag — goes on a branch with its own hunt. 0.8.7's
freshness work landed in a fix window, carried three silent wrong answers,
and came back out as four reverts.

**A CHANGELOG line claims only what a test pins.** A headline ahead of the
code is the overclaim the hunts keep finding (five in the 0.8.7 cycle): narrow
the line to what a test checks, or write the test.

**A scripted edit must fail loudly when its anchor is gone.** Session 32 shipped
five user-facing commits with no changelog, because three `str.replace` calls
targeted `## Unreleased` after a release had renamed that heading — and a
`replace` that matches nothing is a silent success. Same shape as a test that
asserts against a file the fixture does not create: it passes, and it proves
nothing. Assert the anchor before replacing, and when appending to a section
that may not exist, create it.

**Pre-1.0, prefer the clean break over the compatibility shim.** Rename, remove
and reshape toward the surface the tool should have; do not carry aliases or
deprecation paths. The changelog says what a user must *do*, and that is the
whole migration. `--serve` became `--lsp` this way. This stops at 1.0.

Released now — crates.io, `brew install dpep/tools/trekr`, and the `trekr`
plugin in the myclaude marketplace — so a change that alters the built binary
earns a version bump (patch or minor; see `/semver`). Releases go through the
`release` script and are supervisor-driven: keep `## Unreleased` accurate and
leave the cutting to them. CI runs on **ubuntu**, so macOS-only correctness is
not correctness.

**The VS Code extension ships separately**, as `dpep.trekr` on the Marketplace,
and `release` does not publish it. When `editors/vscode/` changes for users:
bump `editors/vscode/package.json`'s version, add an entry to
`editors/vscode/CHANGELOG.md`, `npm --prefix editors/vscode run package`, then
`npx @vscode/vsce publish --packagePath editors/vscode/trekr-<version>.vsix`
(after a one-time `vsce login dpep`; the Marketplace verifies each upload for a
few minutes). A server-only change needs no extension release — the extension
runs whatever `trekr` is installed.
