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
| `site` | `path:line`, matched on the path's tail |
| `exit` | the process exit code, for cases about not dying |

`refs QUERY` asserts the `counts` object. `symbols FILE` asserts the outline,
in source order, comma-separated.

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
