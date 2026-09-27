# trekr

Ruby code intelligence for agents: **position → meaning**, **definition →
references**. Built for legacy Rails monorepos with many worktrees, where the
incumbents cost gigabytes per workspace and answer "the first ten methods with
that name."

> **Early, and working.** The three engine layers are built — blob facts, a
> per-checkout namespace, receiver resolution — plus an LSP front and enough
> Rails DSL modelling to follow `belongs_to`, `enum`, `delegate`, and Tapioca's
> generated RBIs. Ruby core and the checkout's gems are indexed. See
> [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for what exists and what it
> measures, and [docs/PLAN.md](docs/PLAN.md) for where it goes.

## Install

```sh
brew install dpep/tools/trekr
```

No Homebrew:

```sh
cargo install trekr
```

No Ruby toolchain, no `bundle install`, no bootable app — not to install it, and
not to run it. Prism parses; SQLite remembers.

Homebrew wires up tab completion on install; `trekr --completions bash` (or
`zsh`, `fish`, …) prints the script for anyone who needs it elsewhere.

## The idea

**A blob's facts are a pure function of its bytes.**

Facts are keyed by git blob OID, so every worktree of a repo shares one index, a
branch switch reparses only what is genuinely new, and a reindex with no edits
parses nothing at all. Measured on rails: 1.5 s cold, **61 ms** to reindex with
nothing changed, **~0.2 s and zero parses** for a second worktree. Rubydex —
Shopify's Rust indexer, and the closest peer — pays 177 ms for that same no-op,
and pays it again on every process boot because it never writes anything down.

## Try it

```sh
trekr --index                    # index the checkout you are standing in
trekr --status                   # what is indexed, and what the checkouts share
trekr --symbols lib/thing.rb     # outline a file before reading it
trekr --refs 'Widget#save'       # references narrowed by receiver
trekr --refs Widget              # every mention of a name in this checkout
trekr --def lib/thing.rb:12:5    # what is this name, and where is it defined
trekr --ancestors Widget         # the linearized ancestor chain
trekr --dead app/models          # methods nothing appears to call, graded
trekr --gc --dry-run             # what old gem versions and deleted worktrees would free
```

Every command honors `--json` and `--ndjson`, because the intended caller is an
agent. Exit codes mean something, and each means one thing:

| Exit | Meaning |
| --- | --- |
| `0` | An answer: something matched, was indexed, or was collected. |
| `1` | A definitive nothing: trekr looked, and it is not there. |
| `2` | No answer yet: this checkout is not indexed. Run the `hint` (`trekr --index …`), then ask again. |
| `64`–`74` | An error, below. |

### Errors

When a run fails under `--json`/`--ndjson` — a bad flag, a file that does not
exist, a directory outside any checkout, a store that cannot be opened — stdout
carries one object instead of an answer:

```json
{ "error": "trekr: cannot read app/gone.rb: No such file or directory (os error 2)", "kind": "not_found", "code": 66 }
```

`kind` is stable, and `code` is the exit code. The message also goes to stderr;
in text mode stdout stays empty. The codes come from `sysexits(3)`, one per
remedy, the same as [rq](https://github.com/dpep/rq)'s:

| `kind` | Exit | Meaning |
| --- | --- | --- |
| `usage` | 64 | The command line is wrong: an unknown flag, a bad value, an input whose shape trekr cannot tell, nothing asked — including flags before `--json`. It will not succeed on retry. |
| `not_found`, `not_a_repo` | 66 | A path the command names does not exist, or no git checkout contains it. |
| `git` | 69 | git could not be run. |
| `internal` | 70 | trekr failed at something that should always work — a bug. |
| `database`, `io` | 74 | The index, or a file it reads, could not be opened, read or written. |

`trekr --help` lists the same table.

### On rails

A bare name lists every mention — definitions, and calls with the owner they
resolve to, or the receiver's shape when they don't. `find_each` has 28 in
rails; five of them:

```console
$ trekr --refs find_each
activerecord/lib/active_record/destroy_association_async_job.rb:28:82  call        other
activerecord/lib/active_record/querying.rb:24:14  definition  method
activerecord/lib/active_record/relation/batches.rb:85:9  definition  method
activerecord/test/cases/batches_test.rb:20:12  call        ActiveRecord::Querying
activerecord/test/cases/batches_test.rb:562:33  call        local incorrectly_sorted_orders
```

Name the owner and the same sites come back tiered by whether they can reach
*that* method ([below](#references-to-a-method-not-a-name)):

```console
$ trekr --refs 'ActiveRecord::Batches#find_each'
activerecord/lib/active_record/relation/batches.rb:85:9  definition
activerecord/test/cases/batches_test.rb:948:13  confirmed  the receiver's type resolves here
activerecord/lib/active_record/destroy_association_async_job.rb:28:82  possible   untyped receiver, enclosing class shares a namespace with the owner
activerecord/test/cases/batches_test.rb:562:33  possible   untyped receiver, nothing rules it out
...
1 confirmed, 13 possible, 12 excluded of 26 same-name call sites
  excluded: 12 resolve to a different owner, 0 define no such name, 0 wrong arity
```

`--def` is where the tree layer shows: it reparses the one file with Prism, then
walks Ruby's own constant-lookup ladder — enclosing lexical scopes, then the
innermost scope's ancestors, then the top level.

```console
$ trekr --def activerecord/lib/active_record/relation.rb:68:70
activerecord/lib/active_record/relation/batches.rb:7:10  ActiveRecord::Batches

$ trekr --ancestors ActiveRecord::Relation | head -3
ActiveRecord::Relation
ActiveRecord::TokenFor::RelationMethods
ActiveRecord::SignedId::RelationMethods
```

**98 % of rails constant references resolve** (91 % discourse) with core and
the gems indexed; rails' remainder is one optional adapter that is not
installed. A method call goes up a receiver ladder — `self`, a constant, a
local typed from `X.new` or a Sorbet `sig`, a Rails association — and when the
receiver cannot be pinned down, the answer is `residue` with ranked candidates,
never a silent guess. Every answer carries `status`, `confidence`, and
`resolved_via`.

`--dead` grades candidates for deletion; it never calls anything dead:

```console
$ trekr --dead activerecord/lib/active_record/associations
unreferenced     activerecord/lib/active_record/associations/collection_proxy.rb:1123  pretty_print
single-caller    activerecord/lib/active_record/associations/preloader/association.rb:32  load_records_in_batch
convention-only  activerecord/lib/active_record/associations/association.rb:198  marshal_dump   (lower confidence: send)
super-only       activerecord/lib/active_record/associations/belongs_to_association.rb:76  target_changed?   (lower confidence: send, public_send)
```

`unreferenced` means nothing was found, `single-caller` is one reference (an
inlining candidate), `convention-only` is reached only by a symbol handed to a
macro, and `super-only` only by `super` from its overrides — live exactly when
they are. `pretty_print` above is a fair warning: `pp` calls it by protocol, and
trekr does not read ERB, so a method used only from a view looks unreferenced
too.

### Usage counts

`trekr --usage` shows which commands and editor features get used, by whom (an
agent, a person, an editor), how often they come back empty, and how slow. It
counts locally — no queries, paths or repository names — in `trekr.usage.db`
beside the index. `TREKR_USAGE=off` stops the counting: nothing is recorded,
the file is never opened, and `--usage` has nothing to report. A path in
`TREKR_USAGE` moves the file instead.

### In a very large repo

- Turn on git's own caches: `git config core.untrackedCache true` and
  `git config core.fsmonitor true` (the built-in fsmonitor runs on macOS and
  Windows). Every `--index` starts with a `git status`; on a synthetic
  336k-file monorepo the two take it from 2.7 s to 0.09 s.
- A first `--index` needs free disk of about **twice the store's final size**
  while it runs: it is one transaction, and the WAL holds all of it until the
  commit. At 336k files the store is 4.4 GB and the first index takes about
  four minutes.

## References to a *method*, not a name

This is the one no other Ruby tool has. Ask about a *method*, and every call
site is sorted by whether its receiver can actually reach it:

```console
$ trekr --refs 'ActiveRecord::ConnectionHandling#lease_connection'
activerecord/lib/active_record/connection_handling.rb:269:9  definition
actioncable/test/subscription_adapter/postgresql_test.rb:26:26  confirmed  the receiver's type resolves here
actioncable/test/subscription_adapter/postgresql_test.rb:71:38  confirmed  the receiver's type resolves here
...
activerecord/lib/active_record/connection_handling.rb:270:23  possible   untyped receiver, but the enclosing class inherits from the owner
...
1024 confirmed, 84 possible, 87 excluded of 1195 same-name call sites
  excluded: 56 resolve to a different owner, 31 define no such name, 0 wrong arity
```

**Confirmed** means the receiver's type resolves and Ruby's own lookup from it
lands here. **Possible** means the receiver is untyped and nothing rules the
site out — ranked by proximity, never dropped. **Excluded** sites are not
listed but are counted, because that count is the difference between this and a
grep; `--include-excluded` lists them with their reason so the claim is
auditable rather than asserted.

`rg -w lease_connection` returns 1,237 lines in rails — the 1,195 call sites
plus the comments — with no way to tell them apart. `Widget.save` and
`Widget#save` are different questions and answer differently.

## In Claude Code

`trekr --lsp` speaks LSP: definition, references, hover, document and
workspace symbols, implementation, call hierarchy, `require` strings as links,
and Prism syntax diagnostics. It answers on methods and constants, and on
locals, parameters and instance variables — whose other mentions it also
highlights. It keeps the index current as files are saved, and indexes an
unindexed checkout in the background.

[claude/INSTALL.md](claude/INSTALL.md) wires up the skill and the server.

## In VS Code

The same server, plus receiver-aware completion, through the extension in
[editors/vscode](editors/vscode/README.md). It is not published to the Marketplace;
build and install it from this repo:

```sh
npm --prefix editors/vscode ci
npm --prefix editors/vscode run package              # writes editors/vscode/trekr-<version>.vsix
code --install-extension editors/vscode/trekr-*.vsix  # or Extensions view → ⋯ → Install from VSIX
```

It is meant to *replace* Ruby LSP and Sorbet as the Ruby language server, not
run beside them — two servers answer every request twice. What you gain:
definitions and references that follow the receiver instead of the bare name,
completion from the receiver's real ancestors, one index shared by every
worktree, and nothing to boot — no project Ruby, no `bundle install`. What you
give up: rename, formatting, semantic highlighting, signature help, inlay
hints, test code lenses, Sorbet's type errors and ruby-lsp-rails' routes. Most
have a standalone extension; [the extension's
README](editors/vscode/README.md#replacing-ruby-lsp-and-sorbet) says which.

## Known limits

The full list, with the reasoning, is in
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md#known-gaps). The ones most likely
to surprise you:

- `X = Class.new(Base) do … end` (and `Struct.new`, `Data.define`,
  `Module.new`) is read as a class body. Right for its methods; a liberty for
  its constants, which Ruby scopes to the enclosing scope and trekr to `X`.
- `class Foo < Struct.new(:a)` gets no member readers; `Foo = Struct.new(:a)`
  does.
- An `@ivar` receiver is typed by a vote of every write to it in that class
  *in that file* — not only the writes that reach the read, and not the
  class's other files.
- A `super` that lands in Ruby core is only as right as trekr's core stubs
  ([src/tree/core.rb](src/tree/core.rb)) are complete.
- ERB templates are not read, and `refine` is not modeled.

## Development

```sh
make check     # the commit gate: fmt, clippy, tests
make bench     # reproduce every number in docs/ARCHITECTURE.md
make dogfood REPO=/path/to/rails Q=find_each
```

`make bench` and `make dogfood` read corpora from `CORPORA`/`REPO`, which
default to this author's checkout layout — point them at your own clones of
rails, discourse, mastodon, and CRuby.

`make dogfood` is not optional ceremony: running `--refs` on real Rails keeps
finding defects no fixture-sized test can reach.

Conventions are in [CLAUDE.md](CLAUDE.md); decisions already made and turned
down are in [docs/DECISIONS.md](docs/DECISIONS.md) — check it before proposing
an alternative.

## Credits

Ruby semantics are lifted, with attribution, from
[Shopify's Rubydex](https://github.com/Shopify/rubydex) (MIT) — its
`docs/ruby-behaviors.md` is the conformance spec this extractor is written
against, and a block of resolution cases in `src/tree/mod.rs` is ported from its
test suite. trekr does not depend on the crate; the reasons are in PLAN §8.
Parsing is [Prism](https://github.com/ruby/prism). Store and CLI conventions
come from [rq](https://github.com/dpep/rq), Prism patterns from
[rwr](https://github.com/dpep/rwr).

## License

MIT — see [LICENSE.txt](LICENSE.txt). Third-party notices, including Rubydex's,
are in [NOTICE.md](NOTICE.md).
