# Changelog

## Unreleased

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
