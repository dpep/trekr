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

Every shape takes `--json`. The flags below are the explicit forms of the same
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
trekr --index          # the checkout you are in, plus its gems
```

A reindex with nothing changed parses nothing (~60 ms on a 3k-file repo), and a
second worktree of the same repo costs nothing — facts are keyed by git blob.

**No results is not the same as broken**, and trekr distinguishes them for you:

| you get | it means |
| --- | --- |
| `status: not_indexed`, **exit 2** | nobody has indexed this repo. The answer names the root and the command. Run it; do not go looking for the definition. |
| `status: residue`, exit 1 | trekr looked. The receiver is genuinely undetermined — ranked `candidates` say what it might be. |
| exit 1, "no mention of …" | indexed, and the name really is not there. |

`trekr --status` lists what is indexed.

## References to a *method*, not a name

This is the reason to reach for trekr:

```sh
trekr --refs 'ActiveRecord::Querying#where' --json
```

```json
{ "owner": "ActiveRecord::Querying", "method": "where",
  "definition": [{"path": "activerecord/lib/active_record/querying.rb", "line": 24}],
  "counts": {"confirmed": 1197, "possible": 69, "excluded": 541,
             "excluded_different_owner": 47, "excluded_no_such_method": 63,
             "excluded_arity": 431},
  "references": [{"path": "...", "line": 20, "tier": "confirmed",
                  "receiver": "const", "receiver_type": "Topic",
                  "why": "the receiver's type resolves here"}] }
```

* **confirmed** — the receiver's type resolves and Ruby's lookup lands here.
* **possible** — untyped receiver, nothing rules it out. Ranked, never dropped.
* **excluded** — counted, not listed. `--include-excluded` shows them with the
  reason, so the count is auditable.

`Owner.method` asks about a class method instead. A bare `--refs name` keeps the
whole-mention view.

## What is at this position

```sh
trekr --def app/models/post.rb:42:11 --json
trekr --def app/models/post.rb:42          # column optional when typing by hand
```

The column is forgiving: if it holds no name, trekr answers for the nearest one
**on that line** and says so in `snapped_to` (with the other names and their
columns, so a follow-up can be exact). An exact hit never snaps.

**`--def` keeps itself fresh.** It checks git in O(1) and re-reads the file you
asked about if the checkout moved, so a definition that shifted lines is found
at its new line without reindexing. When the answer carries `index`, read it:

```json
"index": { "stale": true, "refreshed": "app/models/user.rb", "hint": "trekr --index ~/code/app" }
```

The file you asked about is current; **other files may lag**, and `hint` is the
cure. No `index` field means the checkout has not moved since it was indexed.
One limit worth knowing: an edit git has not noticed — no `add`, `status` or
`diff` since — is invisible to the check, so run `--index` after bulk edits.

Every answer carries `status` (`resolved` | `ambiguous` | `residue`),
`confidence`, and `resolved_via` — the rung that resolved the receiver (`self`,
`const`, `local:new`, `literal`, `sig`, `sig:param`, `sig:step`, `includer`,
`rbi_dsl`). A residue answer carries ranked `candidates` with a named reason
each. **Trust the disclosure**: `residue` means the receiver is genuinely
undetermined, not that the tool failed.

### `kind` — is that location the code, or the line that declared it

```json
{ "status": "resolved", "owner": "Widget", "kind": "declaration",
  "defined_via": "belongs_to",
  "sites": [{"path": "app/models/widget.rb", "line": 7}] }
```

* **`definition`** — the body is there. A `def`, or a `define_method` block.
* **`declaration`** — the name was made or described there and runs elsewhere:
  a macro (`belongs_to`, `has_many`, `enum`, `scope`, `delegate`, `schema` for a
  column, `define_model_callbacks`), an alias, a bare `private :foo`, or a
  Sorbet stub (`defined_via: rbi`). `defined_via` names which.

  `rbi` is worth its own reaction: real source always wins over a stub, so a
  stub answer means **the implementation is not indexed** — usually a gem that
  has not been indexed yet.

Read it before deciding what to open. A declaration is usually the line a person
wants — `belongs_to :supplier` explains `widget.supplier` better than the
`define_method` inside Rails does — but it is **not** the code that runs, so do
not go looking for a body there. Residue candidates carry their own `kind` too.

(`kind` on the answer is about the *location*. `kind` inside `sites[]` and
`--symbols` is about the *symbol* — class, module, method, constant. Different
questions, different nesting levels.)

## Deletion candidates

```sh
trekr --dead app/models app/services --json
```

Every method defined in scope, checked against references from the **whole**
index. Tiers: `unreferenced` (nothing found), `convention-only` (reached only by
a symbol handed to a macro — usually a sign it *is* used), `single-caller` (one
reference: the inlining candidate).

**It never says "dead", and you should not either.** Measured against a year of
discourse's history, `unreferenced` candidates were deleted 19.8 % of the time
against a 19.0 % base rate — no lift. Treat a candidate as *"nothing was found,
here is what was checked"*, weigh the `confidence` field (a file using `send`
lowers it), and remember trekr does not read ERB templates, so a method called
only from a view looks unreferenced.

## Two more

```sh
trekr --symbols app/models/post.rb --json   # outline before reading
trekr --ancestors Post --json               # linearized chain, unresolved named
```

## Reading the output

* `--json` everywhere; `--ndjson` for streaming.
* Exit `0` matched, `1` a definitive nothing, `2` could not serve.
* `gems.missing` in `--index` output names gems the lockfile wants and disk
  lacks — a hole in every answer that would have come from them.
