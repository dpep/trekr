# Changelog

## Unreleased

- `--refs NAME`: every mention of a name in a checkout — definitions, constant
  references, and call sites — each disclosing what sort of mention it is and,
  for a call, the receiver's shape. Name-level; narrowing is the resolve
  layer's job.
- Fixed: `trekr … | head` panicked instead of exiting, because Rust ignores
  SIGPIPE.
- Fixed: `--refs` on a common name took 90 s. The store now runs
  `PRAGMA optimize` on close, without which SQLite plans the join as a nested
  scan.
- Measured: rails (3.3k files) indexes cold in 1.1 s and reindexes in 63 ms
  with nothing parsed; discourse (11.3k) in 3.2 s / 124 ms; CRuby (7.9k) in
  4.9 s / 104 ms. A second worktree costs 152 ms and zero parses. Numbers and
  their caveats in `docs/ARCHITECTURE.md`.
- Scaffolded the crate: single binary `trekr`, modules mirroring PLAN §4's
  layers, `script/check.sh` as the commit gate.
- Blob-layer extraction (`extract/`): Prism reads one blob's bytes into
  definitions, ancestry edges, constant references, and call sites carrying
  receiver shape. Semantics lifted from Shopify's Rubydex (MIT); the crate is
  not a dependency.
- Checkout scan (`scan/`): `git ls-files -s` for tracked blob OIDs, git's own
  `sha1("blob <len>\0" + bytes)` for anything the working tree has changed, so
  an uncommitted edit keys the same as it will once committed.
