<p align="center"><img src="editors/vscode/images/icon.svg" alt="trekr logo" width="128"></p>

# trekr

Ruby code intelligence: turning **position → meaning**, **definition →
references**. Supports Rails, RSpec, RBIs, gems, worktrees, and scale.

## Install

```sh
brew install dpep/tools/trekr    # or: cargo install trekr
```

VS Code Extension: [trekr from the Marketplace](https://marketplace.visualstudio.com/items?itemName=dpep.trekr) ([more below](#in-vs-code)).

## Usage

```sh
trekr --index                    # index the checkout you are standing in
trekr --status                   # this checkout and its gems; --all lists every checkout
trekr --symbols lib/thing.rb     # outline a file before reading it
trekr --refs 'Widget#save'       # references narrowed by receiver
trekr --refs Widget              # every mention of a name in this checkout
trekr --def lib/thing.rb:12:5    # what is this name, and where is it defined
trekr lib/thing.rb:12:5 --explain  # the same, and why it came out that way
trekr Widget#save                # a card: where it is defined, how many sites reach it
trekr --ancestors Widget         # the linearized ancestor chain
trekr --dead app/models          # methods nothing appears to call, graded
trekr --gc --dry-run             # what old gem versions and deleted worktrees would free
```

The bare forms are sugar: a position is `--def`, and `Owner#method` or a
`Constant` is a **card** — the definition and, for a method, the reference
counts by tier (`--refs` lists the sites).

Every command honors `--json` and `--ndjson`, because the intended caller is an
agent. Under `--ndjson` a row set (`--refs`, `--dead`, `--symbols`, `--usage`)
streams one row per line, then ends with one `{"answer": {…}}` line: the rest
of the `--json` answer (`counts`, `summary`, `status`…) plus `rows`, how many
lines came before it. That line is always written, even for an empty set, so a
reader can tell a finished stream from a broken one:

```sh
trekr --refs 'Post#publish' -J | jq -c 'select(.answer | not)'   # the sites
trekr --refs 'Post#publish' -J | tail -1 | jq .answer.counts      # the tally
```

Exit codes mean one thing each:

| Exit | Meaning |
| --- | --- |
| `0` | An answer: something matched, was indexed, or was collected. |
| `1` | Nothing found. `status` says how sure: `no_such_method` is certain, `residue` names what it could not see (an unindexed ancestor, an untyped receiver). |
| `2` | No answer yet: the checkout is not indexed, or its first index is still running and the miss may not hold. Run the `hint` (`trekr --index …`), then ask again. |
| `64`–`74` | An error, below. |

An answer given while a first index is still running carries `warming` in
JSON, claims nothing certain, and `--dead` lists nothing until it ends.

### Errors

When a run fails under `--json`/`--ndjson`, stdout carries one object instead
of an answer:

```json
{ "error": "cannot read app/gone.rb: No such file or directory (os error 2)", "kind": "not_found", "code": 66 }
```

`kind` is stable and `code` is the exit code. The message also goes to stderr;
in text mode stdout stays empty. Codes come from `sysexits(3)`, one per remedy,
as in [rq](https://github.com/dpep/rq):

| `kind` | Exit | Meaning |
| --- | --- | --- |
| `usage` | 64 | The command line is wrong: an unknown flag, a bad value, an input whose shape trekr cannot tell, nothing asked. JSON under `--json` wherever the flag sits. Retrying won't help. |
| `not_found`, `not_a_repo` | 66 | A path the command names does not exist, or no git checkout contains it. |
| `git` | 69 | git could not be run. |
| `internal` | 70 | trekr failed at something that should always work — a bug. |
| `database`, `io` | 74 | The index, or a file it reads, could not be opened, read or written. |

A damaged index, or one whose upgrade fails, is not an error: trekr moves it
aside (`trekr.db.broken-<time>`), says where on stderr, and rebuilds it. A
trekr that finds a newer trekr's index leaves it alone and keeps its own
beside it (`trekr.v52.db`). `trekr --status` lists both, and `trekr --gc`
removes them: a set-aside copy at once, another trekr's index once it has been
idle for `--older-than`.

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
activerecord/test/cases/batches_test.rb:20:12  confirmed  the receiver's class delegates this to a value whose type runs it
...
activerecord/lib/active_record/destroy_association_async_job.rb:28:82  possible   untyped receiver, enclosing class shares a namespace with the owner
...
13 confirmed, 13 possible, 0 excluded of 26 same-name call sites
```

`--def` reparses the one file with Prism, then walks Ruby's own
constant-lookup ladder — enclosing lexical scopes, then the innermost scope's
ancestors, then the top level:

```console
$ trekr --def activerecord/lib/active_record/relation.rb:68:70
activerecord/lib/active_record/relation/batches.rb:7:10  ActiveRecord::Batches

$ trekr --ancestors ActiveRecord::Relation | head -3
ActiveRecord::Relation::RecordFetchWarning
ActiveRecord::Relation
ActiveRecord::TokenFor::RelationMethods
```

A column on no name — whitespace, punctuation, most strings — answers for the
nearest name on that line and says so (`snapped_to` in JSON); `FILE:LINE`
takes the line's first name. The exception: the string in
`it_behaves_like "a widget"` answers the `shared_examples "a widget"` it
includes.

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
override         activerecord/lib/active_record/associations/collection_proxy.rb:1123  ActiveRecord::Associations::CollectionProxy#pretty_print  — no call names it, but it overrides ActiveRecord::Relation#pretty_print, so a call of that may run it   (lower confidence: a hook `pp` calls by name)
single-caller    activerecord/lib/active_record/associations/preloader/association.rb:32  ActiveRecord::Associations::Preloader::Association::LoaderQuery#load_records_in_batch  — one possible call, at activerecord/lib/active_record/associations/preloader/batch.rb:42: its receiver is untyped; its caller, group_and_load_similar, is itself a candidate   (lower confidence: untyped caller)
convention-only  activerecord/lib/active_record/associations/association.rb:198  ActiveRecord::Associations::Association#marshal_dump  — named only by a symbol handed to a macro (3)   (lower confidence: send, a hook Marshal calls by name)
super-only       activerecord/lib/active_record/associations/belongs_to_association.rb:76  ActiveRecord::Associations::BelongsToAssociation#target_changed?  — reached only by `super` from ActiveRecord::Associations::BelongsToPolymorphicAssociation   (lower confidence: send, public_send)
…

136 candidates in 33 file(s): 6 unreferenced, 3 override, 7 convention-only, 2 super-only, 118 single-caller (53 clear, 83 lower)
```

The tiers, from least evidence of use to most:

- `unreferenced` — nothing names it.
- `override` — nothing names it, but it overrides an ancestor's method, so
  whatever calls that one (often the framework) may run it.
- `convention-only` — named only by a symbol handed to a macro.
- `super-only` — reached only by `super` from its overrides.
- `single-caller` — one reference: an inlining candidate. `caller` in JSON
  says where, and whether it certainly reaches the method.

Each row is `clear` or `lower` confidence, and says why in words. It is one
pass and does not cascade: a method whose only caller is itself a candidate
is `single-caller`, and its reason says so. Two callers trekr cannot see grade
a row `lower` and are named in `caveat`: a view template that writes the name
(trekr does not read views), and Ruby or Rails calling a protocol hook by name
— `marshal_load`, `to_partial_path`, `each`, `perform`, … (DEC-315).

### Where it keeps things

The index is `~/.local/share/trekr/trekr.db`. `TREKR_DB=/some/path.db` points
every command at another one — a throwaway for CI, or one per project. Beside
it: tree snapshots (`trekr.trees/`), Ruby core as readable stub files
(`trekr.core/rbs-<version>-<key>/String.rb`, where a core definition lands),
the language server's `lsp.log`, and the usage counts (`trekr.usage.db`).
`trekr --help` lists the variables.

### Usage counts

`trekr --usage` shows which commands and editor features get used, by whom (an
agent, a person, an editor), how often they come back empty, and how slow. It
counts locally — no queries, paths or repository names — in `trekr.usage.db`.
`TREKR_USAGE=off` stops the counting entirely (`--usage` then says so and
exits `1`); a path in `TREKR_USAGE` moves the file.

`trekr --usage --misses` lists *which* editor clicks came back empty or
unsure: file, line, column, the token under the cursor and trekr's one-line
reason. They are read from `lsp.log`, so `TREKR_LOG=off` turns them off too.
`--days N` narrows the window; `--json` gives one object per miss.

### In a very large repo

- Turn on git's own caches: `git config core.untrackedCache true` and
  `git config core.fsmonitor true`. Every `--index` starts with a `git status`; on a synthetic
  336k-file monorepo the two take it from 2.7 s to 0.09 s.
- A first `--index` needs free disk of about **twice the store's final size**
  while it runs: the checkout's files land in one transaction, and the WAL
  holds all of it until the commit. At 336k files the store is 4.4 GB and the
  first index takes about four minutes.

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
1027 confirmed, 81 possible, 87 excluded of 1195 same-name call sites
  excluded: 56 resolve to a different owner, 31 define no such name, 0 wrong arity
```

- **Confirmed**: the receiver's type resolves, and Ruby's own lookup from it
  lands here — for a method the owner inherits, from the owner or a subclass.
- **Possible**: the receiver is untyped and nothing rules the site out. Ranked
  by proximity, never dropped.
- **Excluded**: not listed, but counted, because that count is the difference
  between this and a grep. `--include-excluded` lists them with their reason,
  so the claim is auditable rather than asserted.

`rg -w lease_connection` returns 1,237 lines in rails — the 1,195 call sites
plus the comments — with no way to tell them apart. `Widget.save` and
`Widget#save` are different questions and answer differently.

## In Claude Code

`trekr --lsp` speaks LSP: definition, references, hover, document and
workspace symbols, implementation, call hierarchy, `require` strings as links,
and Prism syntax diagnostics. It answers on methods and constants, and on
locals, parameters and instance variables — whose other mentions it also
highlights. It keeps the index current as files are saved, and indexes an
unindexed checkout in the background, the files you have open first.

[claude/INSTALL.md](claude/INSTALL.md) wires up the skill and the server.

## In VS Code

The same server, plus receiver-aware completion. Install
[trekr from the Marketplace](https://marketplace.visualstudio.com/items?itemName=dpep.trekr)
— search "trekr" in the Extensions view, or:

```sh
code --install-extension dpep.trekr
```

It finds the `trekr` binary on your `PATH`. To build the extension from
source, see [its README](editors/vscode/README.md).

It is meant to *replace* Ruby LSP and Sorbet as the Ruby language server, not
run beside them — two servers answer every request twice. What you gain:
definitions and references that follow the receiver instead of the bare name,
completion from the receiver's real ancestors, one index shared by every
worktree, and nothing to boot. What you give up: rename, formatting, semantic
highlighting, signature help, inlay hints, test code lenses, Sorbet's type
errors and ruby-lsp-rails' routes. Most have a standalone extension;
[the extension's README](editors/vscode/README.md#replacing-ruby-lsp-and-sorbet)
says which.

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
- Core comes from the `rbs` gem the app's Ruby carries (bundled since Ruby
  3.0). A `super` that lands in core is only as right as those signatures are
  complete, and a chain through a core method (`x.gsub(a, b).downcase`) takes
  their documented return type, even where a subclass such as ActiveSupport's
  `SafeBuffer` returns its own. No Ruby found for a checkout, or no rbs, means
  nothing is known of core, and `--index` says so.
- A method defined in a loop over a list *another file* assigns
  (`METHODS_WITH_QUERY.each { class_eval "def #{m}…" }`) is not named. Its
  class answers `residue` for such a name, never "no such method", and the
  string's own calls are still read.
- ERB templates are not read, and `refine` is not modeled.

## Development

```sh
make check     # the commit gate: fmt, clippy, tests
make bench     # reproduce every number in docs/ARCHITECTURE.md
make dogfood REPO=/path/to/rails Q=find_each
```

`make bench` and `make dogfood` read corpora from `CORPORA`/`REPO`, which
default to this author's checkout layout — point them at your own clones of
rails, discourse, mastodon, and CRuby. `make dogfood` is not optional
ceremony: running `--refs` on real Rails keeps finding defects no
fixture-sized test can reach.

Conventions are in [CLAUDE.md](CLAUDE.md); decisions already made and turned
down are in [docs/DECISIONS.md](docs/DECISIONS.md) — check it before proposing
an alternative.

## Credits

Ruby semantics are lifted, with attribution, from
[Shopify's Rubydex](https://github.com/Shopify/rubydex) (MIT) — its
`docs/ruby-behaviors.md` is the conformance spec this extractor is written
against, and a block of resolution cases in `src/tree/mod.rs` is ported from its
test suite. trekr does not depend on the crate; the reasons are in
[PLAN §8](docs/PLAN.md#8-rubydex-spike-2026-08-23). Parsing is
[Prism](https://github.com/ruby/prism). Store and CLI conventions come from
[rq](https://github.com/dpep/rq), Prism patterns from
[rwr](https://github.com/dpep/rwr).

## License

MIT — see [LICENSE.txt](LICENSE.txt). Third-party notices, including Rubydex's,
are in [NOTICE.md](NOTICE.md).
