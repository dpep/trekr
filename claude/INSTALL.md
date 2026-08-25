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

Claude Code takes LSP servers from *plugins*, so trekr ships `claude/.lsp.json`
already in the shape a plugin root expects — copy it, never reshape it. One
server, one language, the six Ruby extensions trekr indexes.

Point a local marketplace at a plugin directory holding that file plus a
manifest:

```sh
P=~/.claude/marketplaces/local
mkdir -p $P/.claude-plugin $P/plugins/trekr/.claude-plugin
cp claude/.lsp.json $P/plugins/trekr/
```

`$P/.claude-plugin/marketplace.json`:

```json
{ "name": "dpep-local",
  "owner": { "name": "you" },
  "plugins": [ { "name": "trekr", "source": "./plugins/trekr" } ] }
```

`$P/plugins/trekr/.claude-plugin/plugin.json`:

```json
{ "name": "trekr", "version": "0.0.1", "description": "Ruby code intelligence" }
```

Then install it — and note that *installing* is the step, not enabling:

```sh
claude plugin marketplace add ~/.claude/marketplaces/local
claude plugin install trekr@dpep-local
```

Both halves of that are load-bearing, and both fail *silently*. `settings.json`
has no `lspServers` key at all — its schema passes unknown keys through, so a
server declared there is accepted and never read. And `enabledPlugins` only
toggles a plugin that is already installed; setting it by hand registers
nothing. Either way the marketplace resolves, `claude plugin list` stays empty
of trekr, and `.rb` files keep answering "No LSP server available for file
type: .rb".

**`.lsp.json` keys servers at the top level, with no `lspServers` wrapper** —
unlike `.mcp.json`, which accepts either. A wrapped file parses as one server
named `lspServers` that has no `command`, and the whole file is dropped with
"LSP config validation failed for .lsp.json in plugin trekr".

`claude plugin details trekr@dpep-local` confirms it: **LSP servers (1) trekr**.
Servers are read once at session start; a new session picks up the install.

**`startupTimeout` is safe at the 5 s default.** `--serve` answers `initialize`
before touching the store: the tree is assembled lazily on the first query that
needs it, not during the handshake. A repo that has never been indexed still
completes the handshake — it simply answers nothing until you run `trekr --index`.
The first *query* on a large repo pays the tree build (~210 ms on rails, ~310 ms
on discourse); every one after it is warm.

**The workspace root does not limit what it answers.** Claude Code roots the
server at the session's directory, which is routinely a different repo — or one
in a different language. trekr answers for any `.rb` path you name, against the
checkout that file lives in (DEC-024). Only `workspaceSymbol` uses the root, and
it widens to every indexed checkout when the root is not one.

## 4. Teach Claude to reach for it

The LSP server answers when Claude asks; these two steps make Claude ask.

```sh
mkdir -p ~/.claude/skills/trekr
cp claude/trekr-skill.md ~/.claude/skills/trekr/SKILL.md
```

Then add a line to the search-tools section of your global `~/.claude/CLAUDE.md`
so trekr wins the reach-for-grep reflex — alongside whatever `rq`/`rg` guidance
lives there:

```md
- **`trekr` — Ruby: what does this position mean, and who really calls this
  method.** `--def FILE:LINE:COL` says what a call actually runs; `--refs
  'Owner#method'` tiers callers confirmed/possible/excluded by receiver, which
  grep cannot. Ruby only; cross-language "where is this name defined" stays rq/rg.
```

The skill teaches the CLI's flags; the CLAUDE.md line changes which tool gets
picked. Both matter — a skill that's installed but never chosen answers nothing.

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

## When it answers nothing

`~/.local/share/trekr/serve.log` — one ndjson line per request, with the file,
the line, the duration and how much came back. An `"answered": 0` in well under
a millisecond means the request was refused before any work, not that the
engine looked and found nothing.

`TREKR_LOG` takes a path, `-` for stderr, or `off`. `TREKR_LOG_LEVEL=debug` (or
running `trekr --serve --profile` by hand) adds the wire-level params.
