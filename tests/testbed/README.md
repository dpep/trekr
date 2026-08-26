# The testbed

Corner cases we have already paid for, in a form where recording the next one
costs nothing.

**Adding a case is dropping in files. No Rust.** `tests/testbed.rs` iterates
every directory here; a new one is picked up automatically.

```text
tests/testbed/011-your-case/
  app.rb        one or more Ruby files — a tiny source tree
  expected      one assertion per line
```

Each case is staged as a real git checkout with its own database and indexed,
so it exercises the whole path: extract → store → tree → resolve → CLI.

## The `expected` format

```text
# Why this case exists. Say what broke, not what the code does.
def app.rb:8:7   status=resolved owner=Widget via=local:new
def app.rb:12:11 status=residue candidates=2 candidate1=Alpha
refs Widget#save confirmed=1 possible=0 excluded=1
symbols app.rb   Widget,save,Job,run
```

`def FILE:LINE:COL` asserts fields of `--def --json`:

| key | is |
| --- | -- |
| `status` | `resolved`, `ambiguous`, or `residue` |
| `owner` | the class or module the method was found in |
| `via` | `resolved_via` — the rung that typed the receiver |
| `name` | the name at that position |
| `confidence` | exact, as printed |
| `candidates` | how many were offered |
| `candidate1` | the top candidate's owner — the ranking assertion |
| `kind` | `definition` or `declaration` — is the body at that location |
| `defined_via` | the macro that declared it, for a declaration |
| `site` | `path:line`, matched on the path's tail |
| `exit` | the process exit code, for cases about not dying |

`refs QUERY` asserts the `counts` object. `symbols FILE` asserts the outline,
in source order, comma-separated.

`hover FILE:LINE:COL <text>` drives a real `--lsp` session and asserts the
markdown an editor would show contains `<text>`. It exists because some of what
an answer carries reaches an editor **only** through hover: `textDocument/
definition` is a bare list of locations and cannot say what kind of location it
handed back. Where a case stages a server-visible shape, pin the wire too.

An unknown key fails loudly: a typo in an expectation is a test that proves
nothing.

## Writing a good case

- **Pin behaviour, not wishes.** If the current answer is imperfect but
  deliberate, record it and say so in the comment — then a change to it is a
  decision rather than a surprise. Case 010 does this.
- **Make it fail first.** Every case here was checked against a build with the
  fix removed. A case that passes both ways is worse than no case; that has
  bitten this project twice.
- **Keep the source tiny and generic** — `Widget`, `Job`, `Alpha`. Public repo.

## What does not belong here

A case stages exactly **one** checkout, so behaviour that needs two — an app and
a gem it resolves (DEC-029) — cannot be pinned honestly. Writing a
single-checkout approximation would pin the shape rather than the thing that
broke, which is the failure mode the rule above exists to prevent. Such
behaviour is covered where it can be honest: an assertion in `tests/cli_e2e.rs`
or `tests/lsp_e2e.rs`, which can build as many repos as they need.
