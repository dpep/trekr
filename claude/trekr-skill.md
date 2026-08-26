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

**No results is not the same as broken.** `trekr --status` lists what is
indexed; an unindexed repo answers nothing and says so with exit code 2.

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
```

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
