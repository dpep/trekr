# trekr

Ruby code intelligence for agents: **position → meaning**, **definition →
references**. Built for legacy Rails monorepos with many worktrees, where the
incumbents cost gigabytes per workspace and answer "the first ten methods with
that name."

> **Early.** The blob layer is built, tested, and measured, and **constants
> resolve**. Method resolution and ranking are not started. See
> [docs/PLAN.md](docs/PLAN.md) for where it goes and
> [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for what exists.

## The idea

**A blob's facts are a pure function of its bytes.**

Facts are keyed by git blob OID, so every worktree of a repo shares one index, a
branch switch reparses only what is genuinely new, and a reindex with no edits
parses nothing at all. Measured on rails: 1.5 s cold, **61 ms** to reindex with
nothing changed, **~0.2 s and zero parses** for a second worktree. Rubydex —
Shopify's Rust indexer, and the closest thing to a competitor — pays 177 ms for
that same no-op, and pays it again on every process boot because it never writes
anything down.

No Ruby toolchain, no `bundle install`, no bootable app. Prism parses; SQLite
remembers.

## Try it

```sh
cargo build --release

trekr --index                    # index the checkout you are standing in
trekr --status                   # what is indexed, and what the checkouts share
trekr --symbols lib/thing.rb     # outline a file before reading it
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

`--refs` is **name-level**: two unrelated `Config` classes both answer, and so
does every `#save` on every receiver. Each row says what sort of mention it is
and what shape the receiver had, rather than guessing and being quietly wrong.
Narrowing that is the next layer's job — and the receiver shape is the fact it
will narrow on. Across 2.2 M call sites in rails, discourse, and CRuby, 56 % need
no inference at all.

## Development

```sh
make check     # the commit gate: fmt, clippy, tests
make bench     # reproduce every number in docs/ARCHITECTURE.md
make dogfood REPO=~/code/lib/ruby/rails Q=find_each
```

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
against. trekr does not depend on the crate; the reasons are in PLAN §8.
Parsing is [Prism](https://github.com/ruby/prism). Store and CLI conventions
come from [rq](https://github.com/dpepper/rq), Prism patterns from `rwr`.
