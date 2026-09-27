# Wiring trekr into Claude Code

Deliberately outside the skill, so the skill stays a copy of one file.

## 1. Install the binary

```sh
brew install dpep/tools/trekr
```

No Homebrew: `cargo install trekr`. Either way it lands on `PATH`; the brew
route also wires up shell completions.

## 2. Index each repo once

```sh
cd ~/code/your-app && trekr --index
```

Facts are keyed by git blob OID, so every worktree of a repo shares this and a
second checkout costs a scan. The LSP server indexes an unindexed checkout on
its own, in the background; the CLI never does, and says so (exit `2`, with
the command to run). Gems are indexed once per `(name, version)` and
shared by every project that resolves the same one.

## 3. Install the plugin

The skill and the LSP server ship together as one plugin:

```sh
claude plugin marketplace add dpep/myclaude
claude plugin install trekr@myclaude
```

That is both halves at once — the skill that teaches Claude the CLI, and the
`.lsp.json` that registers `trekr --lsp`. Restart Claude Code afterward;
servers are read once at session start.

### If you are assembling this by hand

Claude Code takes LSP servers from *plugins*, so trekr ships `claude/.lsp.json`
already in the shape a plugin root expects — copy it to the plugin root, never
reshape it. One server, one language, the six Ruby extensions trekr indexes.

Note that *installing* is the step, not enabling. Both halves of that are
load-bearing, and both fail *silently*. `settings.json`
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

`claude plugin details trekr@myclaude` confirms it: **LSP servers (1) trekr**.
Servers are read once at session start; a new session picks up the install.

**`startupTimeout` is safe at the 5 s default.** `--lsp` answers `initialize`
before touching the store, then builds the root's tree while it has nothing
else to do (~90–470 ms on rails and discourse, depending on the page cache), so
the first query usually finds it warm. A checkout that has never been indexed
still completes the handshake: the server starts `trekr --index` for it in the
background (DEC-039) and answers from core and gems until that finishes —
`hover` says so. Saved edits are refreshed in the index as they happen.

**The workspace root does not limit what it answers.** Claude Code roots the
server at the session's directory, which is routinely a different repo — or one
in a different language. trekr answers for any `.rb` path you name, against the
checkout that file lives in (DEC-024). Only `workspaceSymbol` uses the root, and
it widens to every indexed checkout when the root is not one.

## 4. Teach Claude to reach for it

Claude Code reads `~/.claude/CLAUDE.md` into every session: it is your
standing instructions, in plain Markdown. A *skill* is different — a document
Claude loads only when a task matches its description, and the plugin in step
3 already installed trekr's. Left alone, Claude often reaches for `grep` out
of habit before any skill comes to mind; one line in `CLAUDE.md` is what makes
it pick trekr for a Ruby question. Create the file if you don't have one, and add:

```md
- **`trekr` — Ruby: what does this position mean, and who really calls this
  method.** `--def FILE:LINE:COL` says what a call actually runs; `--refs
  'Owner#method'` tiers callers confirmed/possible/excluded by receiver, which
  grep cannot. Ruby only; cross-language "where is this name defined" stays rq/rg.
```

The skill teaches the flags; the line decides which tool gets picked. A skill
that is installed but never chosen answers nothing.

## What it answers

goToDefinition, findReferences, documentSymbol, workspaceSymbol, hover,
goToImplementation, call hierarchy, documentHighlight, `require` strings as
document links, and Prism syntax diagnostics — plus completion, for editors
(DEC-040). Definition, references, hover and highlight work on locals,
parameters, `@ivars` and `@@cvars` as well as methods and constants.

Not rename, formatting, or semantic tokens.

**In VS Code**, the client extension is in `editors/vscode/`; the
[README](../README.md#in-vs-code) says how to build and install the `.vsix`,
and [the extension's own](../editors/vscode/README.md) covers disabling Ruby
LSP and Sorbet, what that gives up, and running alongside the rq extension.

## Reading the answers

`hover` is where the disclosure lives, because LSP has no confidence field. A
resolved answer is the signature, its doc comment, and where it lives — no
caveat. A guess says so in words ("Best guess — the receiver's type is
inferred…"), never as a number, and residue says what was checked.

Hover also says **what kind of location a jump sends you to**: "Defined in"
when the body is there, "Declared by `belongs_to` in" when a macro, an alias
or a Sorbet stub made the name and the body runs elsewhere. That has nowhere
else to go: `textDocument/definition` is a bare list of locations, so an editor
that only follows the jump cannot tell a `belongs_to :supplier` line from the
method it generates. Hover, or the CLI's `kind` field, is the only place to
read it.

`findReferences` returns confirmed sites before possible ones — the order of the
list is the tier.

The CLI answers all of the same questions with the tiers explicit; see
`claude/trekr-skill.md`.

## When it answers nothing

`~/.local/share/trekr/lsp.log` — one ndjson line per request, with the file,
the line, the duration and how much came back. An `"answered": 0` in well under
a millisecond means the request was refused before any work, not that the
engine looked and found nothing.

`TREKR_LOG` takes a path, `-` for stderr, or `off`. `TREKR_LOG_LEVEL=debug` (or
running `trekr --lsp --profile` by hand) adds the wire-level params.
