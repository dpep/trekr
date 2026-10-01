---
name: trekr
description: Ruby code intelligence — answer "what does this call actually run" and "who really calls this method" with the `trekr` CLI. Use when a question is about a *position* in Ruby code ("what is this", "where does this call go", `trekr --def FILE:LINE:COL`), about references to a specific method rather than a name (`trekr --refs 'Owner#method'` — it rules out call sites whose receiver goes elsewhere, which grep cannot), about a class's ancestor chain (`--ancestors`), or to outline a file before reading it (`--symbols`). Prefer over rg for these: rg returns every textual match undifferentiated. Not for free-text search, and not for "where is this name defined" across languages — that is rq.
---

# trekr — Ruby code intelligence

`trekr` answers two questions grep cannot: **which method does this call site
actually run**, and **which call sites can actually reach this method**. It
resolves receivers — the enclosing class, constants, locals typed from `X.new`
or a Sorbet `sig`, Rails associations and schema columns — and it discloses how
sure it is rather than guessing.

Ruby only. For "where is `Foo` defined" across languages, use `rq`.

## One argument, dispatched on shape

```sh
trekr 'Widget#save'              # method: where it is, and who can reach it
trekr Widget                     # constant: where it is, and what it inherits
trekr app/models/user.rb:42:11   # position: what is at it
trekr app/models/user.rb:42      # same, column optional
```

Every shape takes `--json`, and `--context DIR` asks it of the checkout at
`DIR` instead of the current directory's (as do `--refs` and `--ancestors`).
The flags below are the explicit forms of the same
things and are not going away — prefer them in scripts, where relying on shape
inference is a way to get surprised.

**The boundary with `rq`, because it is easy to get wrong.** `rq Widget` answers
"where is this name defined", across Ruby, Rust, Go, Python, TypeScript — that
is the right tool for finding a definition by name. `trekr Widget` answers the
*Ruby* question about it: which file declares it, what it inherits, and for a
method how many call sites can actually reach it, tiered. Reach for rq to
**find** a name; reach for trekr to understand what a Ruby name **is** and who
uses it. They are not substitutes, and neither is broken when it declines to do
the other's job.

## Before the first question on a new machine

**Is `trekr` on PATH?** If not, nothing here works and the failure is quiet —
the plugin's LSP server is `trekr --lsp`, so goToDefinition goes silent too.
Install it, then retry:

```sh
brew install dpep/tools/trekr      # macOS/Homebrew
```

No Homebrew? From crates.io (needs the Rust toolchain):

```sh
cargo install trekr
```

Update with `brew upgrade dpep/tools/trekr`, or re-run `cargo install trekr`.
Source + issues: <https://github.com/dpep/trekr>. The LSP server comes up at the
**next session start** after the binary exists — installing fixes both surfaces,
one of them on a delay.

**Then index the repo you are asking about.** This is per-repo, not per-machine,
and there is no automatic first run:

```sh
trekr --index          # the checkout you are in, plus its gems and its Ruby's stdlib
```

A reindex with nothing changed parses nothing (~60 ms on a 3k-file repo), and a
second worktree of the same repo costs nothing — facts are keyed by git blob.

**No results is not the same as broken**, and trekr distinguishes them for you:

| you get | it means |
| --- | --- |
| `status: not_indexed`, **exit 2** | nobody has indexed this repo. The answer names the root and the command. Run it; do not go looking for the definition. |
| `status: residue`, exit 1 | trekr looked. The receiver is genuinely undetermined — ranked `candidates` say what it might be. |
| `status: residue`, `reason: "no name at this position"` | `--def` on a line with no name on it (blank, a comment, only punctuation). Expected; pick another line. |
| `status: no_such_method`, exit 1 | `'Owner#name'`: the owner resolved and nothing in its ancestors defines the name — `reason` says so, and nothing is listed. `--refs` adds a `hint` (`trekr --refs name`) for every call site of the name, unnarrowed. A chain with an unindexed ancestor is `residue` instead, naming it; an owner trekr cannot find at all is `residue` with the same `hint`. |
| exit 1, "no mention of …" | indexed, and the name really is not there. |
| exit 64–74, `{"error", "kind", "code"}` | the call failed: `usage` (fix the command), `not_found`/`not_a_repo` (fix the path), `git`, `database`/`io`, `internal`. Not an answer about the code. |

`trekr --status` shows the checkout you are in, its gems counted (`gems:
{count, indexed, files}`), its Ruby's stdlib (`stdlib: {root, files,
hidden}`), and how many other checkouts are indexed (`others`); `--status
--all` lists every checkout, each with `kind` (`repo`, `gem` or `stdlib`). A checkout nobody indexed is `status: not_indexed`, exit 2, as
a query from it is — `checkouts` is empty and `others` counts the rest.
`--context DIR` asks about another checkout; outside any checkout the repos
are listed.

## References to a *method*, not a name

This is the reason to reach for trekr:

```sh
trekr --refs 'ActiveRecord::ConnectionHandling#lease_connection' --json
```

```json
{ "status": "resolved", "owner": "ActiveRecord::ConnectionHandling", "method": "lease_connection",
  "definition": [{"path": "activerecord/lib/active_record/connection_handling.rb", "line": 269,
                  "root": "/…/rails"}],
  "resolves_to": "ActiveRecord::ConnectionHandling#lease_connection", "inherited": false,
  "counts": {"confirmed": 1024, "possible": 84, "excluded": 87,
             "excluded_different_owner": 56, "excluded_no_such_method": 31,
             "excluded_arity": 0},
  "references": [{"path": "actioncable/test/subscription_adapter/postgresql_test.rb",
                  "root": "/…/rails", "line": 26, "col": 26, "tier": "confirmed",
                  "receiver": "const", "receiver_type": "ActiveRecord::Base",
                  "owner": "ActiveRecord::ConnectionHandling",
                  "why": "the receiver's type resolves here"}] }
```

Every `path` in any answer is relative to the `root` beside it: the checkout
holding the file, which for a gem's method is the gem. Join the two for a file
to open. `root` is `null` only for Ruby core. Text output writes a path in the
checkout you asked about relative to it, and anything else absolute.

* **confirmed** — the receiver's type resolves and Ruby's lookup lands here.
  For a method the owner inherits (`resolves_to` names where, and `inherited`
  is true), that is a receiver of the owner or a subclass landing on it;
  another class inheriting the same method is excluded.
* **possible** — untyped receiver, nothing rules it out; or one typed as an
  ancestor (`self`, a `sig`'s type) that may be the subclass defining this.
  Ranked, never dropped.
* **excluded** — counted, not listed. `--include-excluded` shows them with the
  reason, so the count is auditable.

`Owner.method` asks about a class method instead. A bare `--refs name` keeps the
whole-mention view.

**Through the LSP tool, findReferences is capped** (1,000 by default, confirmed
callers first) and the cut is only announced to editors, not to you. If you get
exactly that many locations, the list was cut — run `trekr --refs` for all of
them and their counts.

## What is at this position

```sh
trekr --def app/models/post.rb:42:11 --json
trekr --def app/models/post.rb:42          # column optional when typing by hand
```

The column is forgiving: if it holds no name, trekr answers for the nearest one
**on that line** and adds `snapped_to` — the name it picked, its column, and the
line's other names as `alternatives`, so a follow-up can be exact (text says
it on the line under the answer). No `snapped_to` means the column hit the
name. The string in `it_behaves_like "x"`, `include_examples` or
`include_context` is not snapped: it answers the shared group of that name.
Nor is a symbol no rule reads as a method's name (`on: :create`, a hash key):
`residue`, `under: symbol`. A line with no name at all is
`residue` with `reason: "no name at this position"`.

**On a variable, `--def` answers the variable**, not the nearest call:
`under: variable`, `resolved_via: flow`, and `definition` is the writes its value
can come from (`kind: assigned`).

| `variable` | where the writes come from |
| --- | --- |
| `local`, `parameter` | flow through the method: both branches of an `if`, a loop's later write, the parameter itself |
| `ivar`, `cvar` | the writes to that `@x` or `@@x` in this file only, and `reason` says so. The LSP also searches the class's other files and its ancestors. |

```sh
trekr --def activerecord/lib/active_record/relation/batches.rb:94:34
# activerecord/lib/active_record/relation/batches.rb:93:11  local `cursor`
```

**`--def` keeps itself fresh.** It checks git in O(1) and re-reads the file you
asked about if the checkout moved, so a definition that shifted lines is found
at its new line without reindexing. When the answer carries `index`, read it:

```json
"index": { "stale": true, "refreshed": "app/models/user.rb", "hint": "trekr --index ~/code/app" }
```

* **`index.stale`** — the checkout moved since it was indexed. The file you
  asked about was re-read (`refreshed` names it when it had changed); **other
  files may lag**, and `hint` is the cure.
* **`index.busy`** — another trekr was writing the index, so the file you
  asked about was answered from its indexed version rather than wait. Ask
  again once that index finishes.

No `index` field means the checkout has not moved since it was indexed. One
limit: an edit git has not noticed — no `add`, `status` or `diff` since — is
invisible to the check, so run `--index` after bulk edits.

A `--def` answer carries `status`, `confidence`, and (when something typed
the receiver) `resolved_via`. They answer different questions, so read them
separately:

* **`status`** — is a competitor *known*? `resolved`: no. `ambiguous`: yes —
  the pick is first, the others are `candidates`; exit 0 either way.
  `residue`: the receiver is undetermined, and ranked `candidates` each carry a
  reason.
* **`confidence`** — the share of the evidence that agrees. A local whose
  read three writes can reach — two `Foo.new`, one from an untyped call — is
  `resolved` at 0.67: nothing contradicts `Foo`, but not everything says it.
  No confidence is low enough to turn `resolved` into `ambiguous`.
* **`resolved_via`** — the rung that typed the receiver: `self`, `const`,
  `local:new`, `literal`, `sig`, `sig:param`, `sig:step`, `includer`,
  `rbi_dsl`, `super`, and `flow` for a variable. `chain` means the receiver
  is a call whose method's return type is declared (`x.strip.downcase` with
  `x` typed); `chain:name` that its receiver was untyped, so every definition
  of that name was asked — `ambiguous` when some declare no return type.
  Ruby core's return types come from the RBS signatures of the checkout's own
  Ruby, so `x.gsub(a, b).downcase` is `String#downcase`.

Core is the checkout's Ruby's: the one `.ruby-version` or the Gemfile names,
else `$GEM_HOME`'s, the `ruby` on `PATH`, or the only one installed, read from
the `rbs` gem bundled with it. **No Ruby found, or one without rbs, means no
core**: `puts` and `"x".upcase` are `residue` whose reason says Ruby core is
not indexed for this checkout. `trekr --index` and `--status` name the Ruby
and rbs used, and a named Ruby that is not installed (`ruby_not_found`).

A core site is the owner's stub, written beside the database: `path:
"String.rb"` with `root` a directory per Ruby's signatures,
`trekr.core/rbs-<version>-<key>/` next to `trekr.db`, so it opens like any
other site.

**`super` is followed.** `--def` on a `super` answers the method it runs: the
next definition after the method's owner in the ancestors (prepends, the
class, includes, the superclass chain), per includer when the method is in a
module. A `super` whose owner the source does not name — in a block, or
`def obj.x` — is `residue`, never a guess.

```sh
trekr --def activerecord/lib/active_record/associations/has_many_through_association.rb:10:9
# activerecord/lib/active_record/associations/association.rb:41:11  initialize
```

**Trust the disclosure**: `residue` means the receiver is genuinely
undetermined, not that the tool failed.

### `kind` — is that location the code, or the line that declared it

```json
{ "status": "resolved", "owner": "Widget", "kind": "declaration",
  "defined_via": "belongs_to",
  "definition": [{"path": "app/models/widget.rb", "root": "/…/app", "line": 7}] }
```

* **`definition`** — the body is there. A `def`, or a `define_method` block
  (`define_method(:x, some_method)` is a declaration: the body is elsewhere).
* **`declaration`** — the name was made or described there and runs elsewhere:
  a macro (`belongs_to`, `has_many`, `enum`, `scope`, `delegate`,
  `def_delegator`, `schema` for a column, `define_model_callbacks`), an alias, a bare `private :foo`, or a
  Sorbet stub (`defined_via: rbi`). `defined_via` names which.

  `rbi` is worth its own reaction: real source always wins over a stub, so a
  stub answer means **the implementation is not indexed** — usually a gem that
  has not been indexed yet.

Read it before deciding what to open. A declaration is usually the line a person
wants — `belongs_to :supplier` explains `widget.supplier` better than the
`define_method` inside Rails does — but it is **not** the code that runs, so do
not go looking for a body there. Residue candidates carry their own `kind` too.

(`kind` on the answer is about the *location*. `kind` inside `definition[]` and
`--symbols` is about the *symbol* — class, module, method, constant. Different
questions, different nesting levels.)

## Deletion candidates

```sh
trekr --dead app/models app/services --json
```

Every method defined in scope, checked against references from the **whole
checkout** (not other indexed repos, so the answer does not depend on them). Tiers: `unreferenced` (nothing found), `convention-only` (reached only by
a symbol handed to a macro — usually a sign it *is* used), `super-only` (reached
only by `super` from the overrides in `super_from`: live exactly when they are),
`override` (no reference, but it overrides the ancestor methods in `overrides`,
so a framework calling those runs it — `readonly?` on a model; any other tier
that overrides one is graded `lower`),
`single-caller` (one reference: the inlining candidate; `caller` names it, and
its `tier` says whether it certainly reaches the method — `possible` is an
untyped receiver, and grades the row `lower`). Every row has a `reason` in
words, and `summary` counts the rows per tier and per confidence. **One pass, no cascade:** a method whose only caller is itself a
candidate is `single-caller`, and its `reason` says the caller is a candidate —
delete the caller and it becomes unreferenced. `visibility` (`public`,
`protected`, `private`) says whether a caller outside the checkout could
break: a private candidate's evidence is complete, a public one's is not.

**It never says "dead", and you should not either.** Measured against a year of
discourse's history, `unreferenced` candidates were deleted 19.8 % of the time
against a 19.0 % base rate — no lift. Treat a candidate as *"nothing was found,
here is what was checked"*, weigh `confidence` (`clear`, or `lower` when the
file uses `send`, `method_missing` and the like — `caveat` names them). trekr
does not read view templates: a row whose name a template's Ruby writes says
"named in a view (…), which is not read", and a protocol hook Ruby or Rails
calls by name (`marshal_load`, `to_partial_path`, `each`, a job's `perform`)
says "a hook … calls by name". A method called only from a view whose
template does not spell it still looks unreferenced.

## Two more

```sh
trekr --symbols app/models/post.rb --json   # outline before reading
trekr --ancestors Post --json               # linearized chain, unresolved named
```

**A name declared with two different superclasses is two classes** (DEC-072)
— common in a monorepo, where a test fake `Post = Struct.new(…)` sits beside
`class Post < ActiveRecord::Base`. Ruby would refuse to load both, so trekr
keeps them apart: `--ancestors Post` answers `status: ambiguous` with one entry
per variant under `variants` (`ancestors`, `definition`, `unresolved_ancestors`), and
the top-level chain is just `[Post]`; the bare card `trekr Post` answers the
same way. Other queries pick the variant nearest
the file asking; when none is nearest, `--def` is `ambiguous` and `--refs`
tiers the site `possible`.

## Reading the output

* `--json` everywhere; `--ndjson` for streaming. Under `--ndjson` a row set
  (`--refs`, `--dead`, `--symbols`) is one row per line, as the `--json` array
  holds it, then a last `{"answer": {…}}` line: the rest of the `--json`
  answer (`counts`, `summary`, `status`…) and `rows`, the count. Filter rows
  with `select(.answer | not)`.
* One name per fact across commands: `query` is what you typed, `fqn` what
  it resolved to, `definition` where it is defined (always present, `[]` when
  unknown), `receiver`/`receiver_text`/`receiver_type` for a call's receiver,
  `unresolved_ancestors` for what could not be seen, and `path` + `root` +
  `line` + `col` on everything located.
* An answer with `warming` came from a first index still running (`read` of
  `of` files in): it may change, and claims nothing certain. Ask again when
  the index ends.
* Branch on the exit code; never read an error as "nothing found":

  | exit | means | do |
  | --- | --- | --- |
  | `0` | an answer (`resolved` or `ambiguous`, something listed) | read it |
  | `1` | nothing found: `no_such_method` (certain), `residue` (it names what it could not see), no mention | read `reason`/`candidates` |
  | `2` | `status: not_indexed`, or a miss while `warming` (an index still running) | run the `hint`, ask again |
  | `64` | `usage`: the command line is wrong | fix the command; a retry won't help |
  | `66` | `not_found`, `not_a_repo`: a path is missing or in no checkout | fix the path |
  | `69` | `git`: git could not be run | |
  | `70` | `internal`: a trekr bug | |
  | `74` | `database`, `io`: the index or a file could not be read or written | check the disk or `$TREKR_DB` |

  Under `--json`/`--ndjson` an error is one object on stdout, and the message
  is on stderr either way:

  ```json
  { "error": "cannot read app/gone.rb: No such file or directory (os error 2)", "kind": "not_found", "code": 66 }
  ```
* `gems.missing` in `--index` output names gems the lockfile wants and disk
  lacks — a hole in every answer that would have come from them.
  `gems.unlocated` lists git and path gems that were not indexed, each with
  its `why` (a git checkout not where bundler puts it, a path outside the
  checkout). `gems.resolved_from` is `lockfile`, or `declared` when there was no
  `Gemfile.lock` and the gemspecs' and Gemfile's dependencies were resolved to
  the highest installed versions instead; absent, no gem was indexed at all.
  `gems.picked` lists each gem found as `name version`; without a lockfile,
  `gems.ruby` says whose Ruby the picks came from and `gems.unread` the
  requirements trekr could not read (the highest installed was taken).
  Gems are looked for in the checkout's own Ruby's directories first;
  `gems.other_ruby` lists the ones found only for another Ruby.
* `ruby` (top level of `--index`, per checkout in `--status`) is the Ruby the
  checkout runs on: `version`, `root` (its stdlib's) and `how` it was chosen —
  `named` by `.ruby-version`/Gemfile, `gem_home`, `path`, `only`, or `kept`
  from the last index; `null` when none was found.
* `gems.stdlib` is the Ruby standard library indexed with the checkout —
  `root`, the `ruby` it belongs to and how it was chosen, and `hidden`: the
  default gems (json, logger, uri…) the bundle has its own copy of, whose
  stdlib files this app does not see. Absent when the checkout names no gem
  and no Ruby. Dev tooling (irb, rdoc, bundler's internals) is not indexed,
  so a question about it is residue. A stdlib class that is partly C
  (`Pathname`, `Monitor`, `OpenSSL::*`) answers residue naming its compiled
  extension for a method its Ruby lacks, rather than "no such method".
