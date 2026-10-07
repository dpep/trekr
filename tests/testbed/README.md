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

A case that holds a `ruby/` directory runs on a Ruby whose standard library
is that directory: it is installed as rvm installs Ruby 9.8.7, under a home
of the case's own, and the checkout names it in `.ruby-version` (DEC-180).
The stdlib is not a second checkout of the case's making — every app runs on
one — so it belongs here, where a gem does not.

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
| `variable` | `local`, `parameter`, `ivar` or `cvar`, for a variable |
| `defined_via` | the macro that declared it, for a declaration |
| `site` | `path:line`, matched on the path's tail |
| `signature` | the first of `signatures` — a Sorbet stub of the method — as `site` |
| `receiver_type` | the type the receiver was given, resolved or not |
| `reason` | a word the residue's reason contains |
| `exit` | the process exit code, for cases about not dying |

`refs QUERY` (a name, or a `FILE:LINE[:COL]` position) asserts the `counts` object, `status=` the answer's status, and
`resolves_to=` the method the query lands on (`Base#save`), `exit=` the exit
code, and `rows=` how many mentions a name's answer lists; `card` takes
`resolves_to=` too.
`card Owner#name` asserts the card's `status`, `reason`, `owner` and `exit`, as
`def` does. `dead FILE Owner#name=tier …`
asserts each method's `--dead` tier — or a class's, module's or constant's, by
its whole name (`Admin::Widget=unreferenced`), or an example group's `let`, `subject` or
`def` by the line it is written on (`@3=unreferenced`) — `none` for one not reported; `tier~word`
also asserts its caveat contains `word`, `tier!~word` that it does not, and `tier~` that it has none. `ancestors NAME A,B,C`
asserts the chain starts with those names, and `unresolved=X,Y` (empty for
none) asserts the ancestors it could not resolve. A case with a `dead` line also has its whole `--dead . --json` pinned,
byte for byte, in `tests/dead.golden`: an output change names the cases it
moved, and `UPDATE_GOLDEN=1` accepts it. `symbols FILE` asserts the outline,
in source order, comma-separated, and `private=a,b` which of its rows are
private.

`hover FILE:LINE:COL <text>` drives a real `--lsp` session and asserts the
markdown an editor would show contains `<text>`. It exists because some of what
an answer carries reaches an editor **only** through hover: `textDocument/
definition` is a bare list of locations and cannot say what kind of location it
handed back. Where a case stages a server-visible shape, pin the wire too.

`definition FILE:LINE:COL a.rb:2,b.rb:5` and `declaration …` drive Go to
Definition and Go to Declaration, and assert the `path:line`s listed, in
order (empty after the position for none).

Some checks need no line of their own: the editor is held to the CLI. At every
`refs FILE:LINE:COL` the case's answer is not residue, Find References must
list the CLI's rows and incoming calls its `confirmed` ones; at every `def` on a
call it places in the checkout, outgoing calls from the method around the call
must reach the same definition (not for a symbol, which no method calls); and
at every `def` the CLI places in the checkout, Go to Definition must list its
`definition` and Go to Declaration its `signatures` — an `.rbi` site being a
definition only when it is all there is, and a declaration falling back to
the definition (DEC-646).

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
