---
name: trekr
description: Ruby code intelligence with the `trekr` CLI — "what does this call actually run" and "who really calls this method". Use for a *position* in Ruby code ("what is this", "where does this call go": `trekr --def FILE:LINE:COL`), for references to a specific method rather than a name (`trekr --refs 'Owner#method'` rules out call sites whose receiver goes elsewhere, which grep cannot), for "is this Ruby unused / safe to delete" (`--dead`), for a class's ancestor chain (`--ancestors`), or to outline a file (`--symbols`). Not for free-text search (rg), nor "where is this name defined" across languages (rq).
---

# trekr — Ruby code intelligence

`trekr` answers two questions grep cannot: **which method does this call site
actually run**, and **which call sites can actually reach this method**. It
types receivers and says how sure it is instead of guessing.

Pick the tool by the question:

- **rq**: "where is `Foo` defined?" Finding a name, in any language.
- **trekr**: what a *Ruby* name is, what a call runs, who reaches a method,
  what looks unused. Ruby only.
- **rg**: free text such as messages, comments and config.

## Before the first question

**Is `trekr` on PATH?** If not, it fails quietly, and so does the plugin's
LSP (`trekr --lsp`). Install, then retry:

```sh
brew install dpep/tools/trekr      # or: cargo install trekr
```

The LSP comes up at the **next session start**.

**There is no setup step.** The first query in a checkout indexes it, with its
gems and Ruby's stdlib: seconds, ~20 s at 100k files. A position answers early,
marked `warming`. Progress goes to stderr, never stdout. `--no-index` (or
`TREKR_NO_INDEX=1`) never indexes: an unindexed checkout is `not_indexed`,
exit 2.

## Commands

```sh
trekr --def app/models/post.rb:42:11     # what is at this position, and where it's defined
trekr --refs 'Widget#save'               # call sites that can reach this method, tiered
trekr --refs 'Widget.build'              # a class method
trekr --refs spec/widget_spec.rb:12      # what is at a position: a method, a let, a variable
trekr --dead app/models spec/models      # deletion candidates, graded
trekr --ancestors Post                   # linearized ancestor chain
trekr --symbols app/models/post.rb       # outline a file before reading it
trekr --index                            # index up front, or after edits
trekr --status                           # what is indexed here
```

Bare forms are sugar: `trekr FILE:LINE[:COL]` is `--def`; `trekr 'Widget#save'`
or `trekr Widget` is a **card** (definition plus reference counts, or a
model's table). Prefer explicit flags in scripts. `--context DIR` asks about
another checkout.

- `--json` everywhere. Under `--ndjson` (`-J`), a row set (`--refs`, `--dead`,
  `--symbols`) streams one row per line and ends with one `{"answer": {…}}`
  line holding the counts and summary. Filter the rows with
  `select(.answer | not)`.
- `--explain` shows in text why an answer came out as it did.

## Reading an answer

Branch on `status` and the exit code. **No results is not the same as broken.**

| status / exit | means | do |
| --- | --- | --- |
| `resolved`, 0 | no competitor known | read it |
| `ambiguous`, 0 | competitors known: the pick is first, the rest are `candidates` | weigh them |
| `residue`, 1 | receiver genuinely undetermined; ranked `candidates` each carry a `why` | an honest "can't tell", not a failure |
| `residue`, `reason: "no name at this position"`, 1 | `--def` on a blank line, comment, or punctuation | pick another position |
| `no_such_method`, 1 | the owner resolved; nothing in its ancestors defines the name | certain. `--refs` adds a `hint` for the unnarrowed view |
| exit 1, "no mention of …" | indexed, and the name isn't there | believe it |
| `incomplete` / `not_indexed`, 2 | no answer yet | run the `hint`, ask again |
| 64 `usage` | bad command line | fix the command; a retry won't help |
| 66 `not_found` / `not_a_repo` | a path is missing, or no checkout contains it | fix the path |
| 69 / 70 / 74 | git failed / trekr bug / index or disk I/O | not an answer about the code |

Under `--json`, an error is one `{"error", "kind", "code"}` object on stdout.
Never read it as "nothing found".

Other fields that change what you do next:

- **`confidence`** is separate from `status`: the share of evidence that
  agrees. A `resolved` 0.67 means nothing contradicts it, but not everything
  confirms it. On a residue, it's how often the first candidate was right in
  measurement (0.7 when few classes define the name, 0.2 when many do).
- **`path` is relative to the `root` beside it.** Join them before opening a
  file. For a gem's method, `root` is the gem.
- **`kind: declaration`** means the name was made there, but its body runs
  elsewhere: a macro (`belongs_to`, `scope`, `delegate`, `enum`), an alias, a
  schema column, or a Sorbet stub. `defined_via` says which. Don't go looking
  for a body on that line. `defined_via: rbi` means **the real implementation
  isn't indexed**, usually because of a gem.
- **`warming`**: a first index was still running. The answer may change.
- **`snapped_to`**: the column held no name, so trekr answered for the nearest
  one on the line. `alternatives` lists the others, for an exact follow-up.

### Freshness: edits are read; `--index` after many

`--def`, `--refs` and `--dead` read every file edited, added (untracked too)
or deleted since the index, for that answer only — no `git add` needed, and
an edit you undo stops counting. The `index` field names what was read
(`refreshed_files`). `stale: true` means other files may lag — over 32
changed, git could not say, or another trekr was writing (`busy_files`) — and
`cause` says which; run the `hint`.

**`--ancestors` and cards read only the index.** After a branch switch or a
large edit, run `trekr --index` before trusting them.

## References to a method

This is the reason to reach for trekr.

```sh
trekr --refs 'ActiveRecord::ConnectionHandling#lease_connection' --json
```

The answer carries `counts` per tier, `resolves_to` (where the method
actually lives), and `references`, each with a `tier` and a `why`.

- **confirmed**: the receiver's type resolves, and Ruby's lookup lands here.
- **possible**: the receiver is untyped, or it's typed as an ancestor that may
  be the subclass defining this. These are ranked and never dropped, so weigh
  them before calling something unused.
- **excluded**: counted but not listed. `--include-excluded` lists them with
  the reason.

A position form (`--refs FILE:LINE[:COL]`) answers for whatever is there. A
method's definition or call gets `Owner#method`. A constant gets its mentions.
A variable gets its reads and writes. A spec's `let`, `subject` or group `def`
gets every read as RSpec runs it, each with `from`. **Use it before deleting a
`let`.** A column on none of those exits 64.

`X.new` counts as a call of `initialize` (`"called_as": "new"`).

**The LSP's findReferences is capped** (1,000 by default), silently. Exactly
that many locations means the list was cut: run `trekr --refs`.

## Deletion candidates: `--dead`

```sh
trekr --dead app/models app/services --json
```

It lists methods, then spec `let`s/`subject`s/group `def`s and shared groups,
then classes, modules and constants, checked against the **whole checkout**.
Each row has a `tier`, a `reason`, a `confidence` (`clear` or `lower`), and
often a `caveat`.

| tier | means |
| --- | --- |
| `unreferenced` | nothing names it |
| `shadowed` | every call lands on an override (`overridden_by`), so this one never runs |
| `test-only` | a class or constant only specs name |
| `override` | nothing names it, but it overrides an ancestor method (`overrides`) a framework may call |
| `convention-only` | reached only by a macro symbol, a route, or a by-name lookup (`convention.by`). Usually means it *is* used |
| `super-only` | reached only by `super` from `super_from`. It's live exactly when they are |
| `single-caller` | one reference (`caller`): an inlining candidate. A `possible` caller grades the row `lower` |

**It never says "dead", and you shouldn't either.** A row means "nothing was
found; here is what was checked". Before deleting:

- Read `confidence` and `caveat`. A `caveat` names a way in trekr can't see:
  `send`/`method_missing`, a computed name, a Haml or Slim view (ERB and RABL
  are read), a hook Ruby calls by name (`marshal_load`, `perform`), an
  unindexed ancestor, or a gem.
- **Every class, module and constant row is `lower`** unless a convention
  names it. Treat it as a lead, not a verdict.
- **Every spec-member row is `lower`.** Run the spec file after deleting a
  `let`. (`let!` is never listed.)
- `visibility: private` means the evidence is complete. A `public` method may
  have callers outside the checkout.
- **One pass, no cascade.** A method whose only caller is itself a candidate
  is `single-caller`, and its reason says so. Delete the caller, re-index, and
  ask again.
- `--dead` lists nothing while `warming`.

## Gotchas

- **`--def` on a variable answers the variable** (`under: variable`). Its
  `definition` lists the writes its value can come from. For `@ivar`, that's
  this file's writes only.
- **`super` is followed** to the method it runs. A `super` whose owner the
  source doesn't name (in a block, or `def obj.x`) is `residue`.
- **One name with two superclasses is two classes** (often a test fake beside
  the real model). `--ancestors` and the card answer `ambiguous`, with
  `variants`.
- **No Ruby (or no `rbs`) for the checkout means no core**: `"x".upcase` is
  `residue`, and the reason says so. `trekr --index` names the Ruby picked.
- **Views**: ERB and RABL are read. Route helpers (`posts_path`) are
  `residue`. Haml and Slim aren't read.

Every field, explained: <https://github.com/dpep/trekr/blob/main/docs/OUTPUT.md>.
