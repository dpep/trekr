# Wiring trekr into Claude Code

Deliberately outside the skill, so the skill stays a copy of one file.

## 1. Build and put it on `PATH`

```sh
cargo build --release
ln -sf "$PWD/target/release/trekr" /usr/local/bin/trekr
```

## 2. Index once per machine

```sh
cd ~/code/your-app && trekr --index
```

Facts are keyed by git blob OID, so every worktree of a repo shares this and a
second checkout costs a scan. Gems are indexed once per `(name, version)` and
shared by every project that resolves the same one.

## 3. Register the server

Merge `claude/lsp-config.json` into your Claude Code settings. One server, one
language, the six Ruby extensions trekr indexes.

**`startupTimeout` is safe at the 5 s default.** `--serve` answers `initialize`
before touching the store: the tree is assembled lazily on the first query that
needs it, not during the handshake. A repo that has never been indexed still
completes the handshake — it simply answers nothing until you run `trekr --index`.
The first *query* on a large repo pays the tree build (~210 ms on rails, ~310 ms
on discourse); every one after it is warm.

## What it answers

goToDefinition, findReferences, documentSymbol, workspaceSymbol, hover,
goToImplementation, call hierarchy, and Prism syntax diagnostics.

Not completion, rename, formatting, or semantic tokens — an agent does not use
them, and announcing them would invite the editor to route work here that this
engine has no business doing.

## Reading the answers

`hover` is where the disclosure lives. LSP has no confidence field, so the
hover text names the rung that resolved the receiver, the type it found, and
how confident that makes it. `findReferences` returns confirmed sites before
possible ones — the order of the list is the tier.

The CLI answers all of the same questions with the tiers explicit; see
`claude/trekr-skill.md`.
