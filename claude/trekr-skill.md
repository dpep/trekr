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

## Index first, once per machine

```sh
trekr --index          # the checkout you are in, plus its gems
```

A reindex with nothing changed parses nothing (~60 ms on a 3k-file repo), and a
second worktree of the same repo costs nothing — facts are keyed by git blob.

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

Every answer carries `status` (`resolved` | `residue`), `confidence`, and
`resolved_via` — the rung that resolved the receiver (`self`, `const`,
`local:new`, `literal`, `sig`, `sig:param`, `sig:step`, `includer`, `rbi_dsl`).
A residue answer carries ranked `candidates` with a named reason each. **Trust
the disclosure**: `residue` means the receiver is genuinely undetermined, not
that the tool failed.

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
