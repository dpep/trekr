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
```

Every command honors `--json` and `--ndjson`, because the intended caller is an
agent. Exit codes mean something: `0` matched, `1` a definitive nothing, `2` the
request could not be served.

```console
$ trekr --refs find_each   # in rails — 4 of 26 mentions, one per receiver shape
activerecord/lib/active_record/relation/batches.rb:85:9  definition  method
activerecord/test/cases/batches_test.rb:20:12  call        const Post
activerecord/test/cases/batches_test.rb:562:33  call        local incorrectly_sorted_orders
activerecord/lib/active_record/destroy_association_async_job.rb:28:82  call        other
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

**82 % of rails constant references resolve** (78 % discourse), and every one
that does not names a gem or a core class that is not indexed yet — none is a
wrong turn on the ladder. Every answer carries `status`, `confidence`, and
`resolved_via`; a method call comes back as honest residue with its receiver
shape, because narrowing that needs a ladder that does not exist yet.

## References to a *method*, not a name

This is the one no other Ruby tool has. Ask about a *method*, and every call
site is sorted by whether its receiver can actually reach it:

```console
$ trekr --refs 'ActiveRecord::ConnectionHandling#lease_connection'
activerecord/lib/active_record/connection_handling.rb:269:9  definition
actioncable/test/subscription_adapter/postgresql_test.rb:26:26  confirmed  the receiver's type resolves here
actioncable/test/subscription_adapter/postgresql_test.rb:71:38  confirmed  the receiver's type resolves here
...
1024 confirmed, 55 possible, 89 excluded of 1168 same-name call sites
  excluded: 58 resolve to a different owner, 31 define no such name, 0 wrong arity
```

**Confirmed** means the receiver's type resolves and Ruby's own lookup from it
lands here. **Possible** means the receiver is untyped and nothing rules the
site out — ranked by proximity, never dropped. **Excluded** sites are not
listed but are counted, because that count is the difference between this and a
grep; `--include-excluded` lists them with their reason so the claim is
auditable rather than asserted.

Across twelve heavy-collision method names on rails — 25,297 same-name call
sites — that comes to **32 % confirmed, 43 % possible, 24 % excluded**. `rg -w`
returns all 25,297 undifferentiated. `Widget.save` and `Widget#save` are
different questions and answer differently.

## In Claude Code

`trekr --lsp` speaks LSP: goToDefinition, findReferences, documentSymbol,
workspaceSymbol, hover, goToImplementation, call hierarchy, and Prism syntax
diagnostics. Deliberately not completion, rename, or formatting — an agent does
not use them, and announcing them would invite an editor to route work here that
this engine has no business doing.

[claude/INSTALL.md](claude/INSTALL.md) wires up the skill and the server.

## Development

```sh
make check     # the commit gate: fmt, clippy, tests
make bench     # reproduce every number in docs/ARCHITECTURE.md
make dogfood REPO=/path/to/rails Q=find_each
```

`make bench` and `make dogfood` read corpora from `CORPORA`/`REPO`, which
default to this author's checkout layout — point them at your own clones of
rails, discourse, mastodon, and CRuby.

`make dogfood` is not optional ceremony: running `--refs` on real Rails is what
found both defects in the last commit, and neither was reachable from a
fixture-sized test.

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
