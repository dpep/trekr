# trekr for VS Code

A Ruby language server backed by [trekr](https://github.com/dpep/trekr): Go to
Definition that follows the receiver, Find References narrowed to the method you
asked about, completion from the receiver's actual ancestors, call hierarchy,
outline, and syntax errors as you type. No project Ruby, no `bundle install`,
no bootable app — trekr reads the source and its gems off disk.

## Requirements

A `trekr` newer than 0.1.5 (the first with completion) on your `PATH` (`brew install dpep/tools/trekr`), or set
`trekr.path`. Open a Ruby project and trekr indexes it in the background the
first time (progress shows in the status bar); after that the index is shared
by every worktree of the repo and kept current as you save.

## What it does

| Feature | What trekr answers |
| --- | --- |
| Go to Definition / Peek | Where the call actually goes: the receiver's type is resolved (`w = Widget.new; w.save` → `Widget#save`). An unresolvable receiver gives a short ranked peek list, never a single confident guess. |
| Find All References | Call sites that can reach *this* method — confirmed ones first, then possible ones; ones whose receiver resolves elsewhere are left out. On a class or constant: every reference Ruby's lookup resolves to it. |
| Hover | The definition's signature and doc comment, and where it lives. A guess says so in words. |
| `require` strings | Cmd-click or Go to Definition on a `require`, `require_relative`, `load` or `autoload` string opens the file it loads — the checkout's, a gem's, or the standard library's. Several matches give a peek list; hover names the file and gem; a string with one file behind it is underlined as a link. |
| Completion | After `recv.`: the receiver's methods, own first, then inherited. After `Scope::`: its constants. A bare word: locals, the class's methods, constants in scope. |
| Outline, breadcrumbs, Go to Symbol | From the file as you have it, unsaved edits included. |
| Go to Implementation | Classes that include a module; overrides of a method. |
| Call Hierarchy | Callers (confirmed only) and callees, walkable in both directions. |
| Problems | Ruby syntax errors, from the same parser. |

## Settings

| Setting | Default | Meaning |
| --- | --- | --- |
| `trekr.path` | `"trekr"` | The trekr binary. |
| `trekr.features` | all | Which features are active. Remove one to leave it to another extension. |
| `trekr.index` | `true` | Index an unindexed checkout in the background. |
| `trekr.referenceLimit` | `1000` | Most locations Find All References returns, confirmed callers first; trekr says when it cut the list. |

Upgrading trekr (`brew upgrade trekr`) needs no restart. The running server
switches to the new binary within a couple of seconds and keeps your open
files and unsaved edits.

Commands: **trekr: Restart Language Server**, **trekr: Show Output**. The
server's own log is `lsp.log` beside trekr's database; `trekr --usage`
counts which features the editor used.

## Replacing Ruby LSP and Sorbet

trekr is meant to be *the* Ruby language server. Two servers answering the
same request show every definition and reference twice, so disable the others
for Ruby: in the Extensions view, disable **Ruby LSP** (`shopify.ruby-lsp`) and
**Sorbet** (`sorbet.sorbet-vscode-extension`) — per workspace if you still want
them elsewhere.

What that gives up, and where to get it instead:

| Ruby LSP / Sorbet feature | After switching |
| --- | --- |
| RuboCop diagnostics, formatting, quick fixes | A RuboCop extension (e.g. `rubocop.vscode-rubocop`) — unaffected by this switch. |
| Syntax highlighting | VS Code's built-in Ruby grammar. Semantic highlighting (Ruby LSP's token colouring on top of it) is gone. |
| Auto-inserting `end` | An `endwise` extension. |
| Run/debug test code lenses, Test Explorer | A test-runner extension, or the terminal. Debugging (`rdbg`) is a separate extension and is unaffected. |
| Rename, signature help, inlay hints, folding/selection ranges | Not provided by trekr. VS Code still folds by indentation and expands selection by word and bracket. |
| Sorbet type errors and typed hover | Run `srb tc` in CI or the terminal. trekr *reads* `sig`s to resolve receivers but does not type-check. |
| ruby-lsp-rails (routes, schema hover) | Not provided. trekr does resolve `has_many`, `belongs_to`, schema columns and `delegate` for definitions and completion. |

## With the rq extension

The [rq](https://github.com/dpep/rq) extension's default `fallback` mode only
answers Go to Definition where no other provider does. With trekr installed,
trekr answers Ruby precisely and rq fills in where trekr has nothing — leave rq
in `fallback`.
