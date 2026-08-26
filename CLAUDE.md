# trekr development conventions

`trekr` is a **Ruby code-intelligence engine** — position→meaning and
definition→references for massive legacy Rails monorepos, agent-first. Read
[docs/PLAN.md](docs/PLAN.md) before anything else: it carries the research, the
read on the other engines (Ruby LSP / Rubydex / Sorbet), the architecture, the measurement
gate, and the phased roadmap. Keep it — and the docs that grow out of it — in sync
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
- **Full disclosure, ranked.** Every answer carries `status: resolved | ambiguous |
  residue`, `confidence`, `resolved_via`. Residue still returns ranked candidates
  with the reason. Nothing silently dropped, nothing silently promoted.
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

## Toolchain

Rust, single crate until there's a concrete reason to split. `cargo` is keg-only:
`/opt/homebrew/opt/rustup/bin/cargo` (or add to PATH). Gate before commit:
`cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`.

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
- Fixture repos under `tests/fixtures/`, generic names (`Widget`, `HandlerA`) —
  public repo, nothing employer-identifying.
- Verify through `cargo test`, not hand-run binaries; e2e drives the built binary
  with an isolated DB env var and a temp repo.
- Bench corpora (large, real): `~/code/lib/ruby/rails`, `~/code/lib/ruby/discourse`,
  `~/code/lib/ruby/mastodon`. Ranking/scale checks belong there, not in unit tests.
- Accuracy is `script/gold.py` against the TracePoint gold set
  ([BASELINE.md](docs/BASELINE.md)); the other engines are scored by `script/compare.py`
  against the same sites over LSP ([COMPARISON.md](docs/COMPARISON.md)), which
  is an append-only series — add a dated row, never edit one.

## Landing changes

Solo repo: no PRs, commit directly to `main`, small logically-connected commits,
behavior or structure but not both. `CHANGELOG.md` gets its entry in the commit
that earns it, under `## Unreleased`.

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
