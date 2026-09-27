# trekr architecture

The design contract. [PLAN.md](PLAN.md) says *why*; this says *what is built*.
Change them in the same commit as the code, per [CLAUDE.md](../CLAUDE.md).

Status: **All three layers built.** Ruby core and the checkout's gems are
indexed. Not started: Rails DSL modeling, Tapioca `sorbet/rbi/` ingestion, the
LSP front, and `--refs` narrowed by receiver.

## The one idea

> **A blob's facts are a pure function of its bytes.**

Everything else is a consequence. Facts are keyed by git blob OID, so N
worktrees of a repo cost one index, a branch switch reparses only genuinely new
content, and a reindex with no edits parses nothing at all. The moment a fact
knows what path it came from, that property is gone — which is why the layer
boundary below is stated as a prohibition rather than a preference.

## Layers

```text
┌─ 3. resolve + rank ──────────────────────── BUILT ─┐
│    resolve/  receiver ladder, ranked residue       │
├─ 2. tree layer ──────────────────────────── BUILT ─┤
│    tree/     per checkout: constant namespace,     │
│              ancestor linearization. Method tables │
│              and singleton chains not yet.         │
├─ 1. blob layer ───────────────────────── BUILT ────┤
│    scan/     checkout → path→OID map               │
│    extract/  bytes → facts       (pure)            │
│    store/    OID → facts, SQLite WAL               │
└────────────────────────────────────────────────────┘
```

### `scan/` — the only module that knows a path exists

`git ls-files -s` yields path→OID for every tracked file in ~100 ms at 100k
files. Files the working tree has changed and files git has never seen — both
from one `git status`, which uses git's untracked cache where it is enabled
(DEC-043) — are hashed the way git does —
`sha1("blob <len>\0" + bytes)` — so an uncommitted edit keys identically to the
commit that will later contain it. A file that has vanished from the worktree is
simply absent from the map; there is no deletion case to handle downstream.

Submodule (`160000`) and symlink (`120000`) entries are dropped: their OIDs name
a commit and a path string, not Ruby.

**A git repository is required.** Content addressing is the product, and git is
what makes it nearly free (DEC-001).

### `extract/` — bytes in, facts out

Prism (`ruby-prism`, vendored C, no Ruby toolchain) via the `Visit` trait, with a
lexical scope stack on the visitor: push a frame, call the free `visit_*`
function to descend, pop. Semantics are lifted from Shopify's Rubydex (MIT) —
`docs/ruby-behaviors.md` is the conformance spec — but the crate is not a
dependency (PLAN §8).

The fact set:

| fact | carries |
|---|---|
| **definition** | name, kind (class/module/method/constant), lexical nesting, singleton, visibility, parameters, `via`, `target`, Sorbet `sig` return |
| **ancestry edge** | nesting, relation (superclass/include/prepend/extend), target as written — **only when written in a class or module body**, never inside a `def`, where the mixin runs against whatever `self` is at call time |
| **constant reference** | name as written, the nesting that will resolve it |
| **call site** | name, **receiver shape**, receiver text, arity, block |

Receiver shape — `implicit | self | const | local | ivar | other` — is the fact
Rubydex does not carry and the reason this engine is not a wrapper around it.
53–66% of Ruby call sites are implicit self and need no inference at all.

Macros are expanded at extraction, so no later layer needs to know they exist:
`attr_accessor :x` becomes `x` and `x=`; `module_function` turns one `def` into a
public singleton method and a private instance one; `alias` and `alias_method`
become methods with a `target`.

### `tree/` — a checkout's namespace, rebuilt not patched

Blob facts are deliberately ignorant of each other: `class Widget < Base`
records the string `Base` and stops. This layer turns a checkout's facts into a
constant namespace and an ancestor order.

**Constant lookup** is Ruby's own ladder, in order:

1. every enclosing lexical scope's **own** constants (never their ancestors);
2. the **ancestors of the innermost scope** only;
3. the top level.

A path `A::B::C` uses that ladder for `A` alone. Every later segment descends
through the previous one's ancestors — lexical nesting never applies past the
head.

**Linearization** is `[prepends, self, includes, superclass's whole chain]`,
and prepend/include are **not** symmetrical:

- an *include* dedups first-wins against prepends, earlier includes, and the
  parent chain — anything already reachable keeps its deeper position;
- a *prepend* re-orders last-wins, pulling an existing entry to the front, and
  does **not** dedup against includes.

So `prepend A; include A` gives `[A, Foo]` while `include A; prepend A` gives
`[A, Foo, A]`. A single "seen" set gets that wrong and looks right on every
simple case; the ported Rubydex torture tests in `src/tree/mod.rs` pin it.

Two things the blob layer cannot know, resolved here:

- **`Module.nesting`.** The blob layer records nesting *as written* (`["B", "A"]`
  inside `module A; module B`) because that is all the bytes determine. Only a
  namespace can qualify it to `["A::B", "A"]`, since a compact `module A::B`
  inside `module X` may land under `X` or at the top level.
- **Constant aliases.** `Bar = Foo` keeps its own declaration site — that is
  where go-to-definition on `Bar` belongs — but anywhere a *namespace* is
  wanted (`Bar::Baz`, `class Foo < Bar`) the alias is followed through.

**Flat once assembled** (DEC-060). Assembly writes a map, because placing a
declaration reads the namespace still being written. The finished namespace is
laid out as one flat byte layout ([`tree/snapshot.rs`](../src/tree/snapshot.rs))
and every query reads it in place: strings interned once, names sorted by FQN,
sites, mixins and extends as `u32` ranges into flat arrays, a target as an
interned name plus an interned nesting list, and an open-addressed FQN index.
Nothing points at anything, so the same bytes can be a file. Names are written
sorted and strings interned in first-met order, so one namespace always encodes
to the same bytes. Whatever assembly memoized against the half-built namespace
is dropped with it.

**Persisted per checkout, and mapped** (DEC-065). `Tree::build` first looks
for the checkout's snapshot beside the store — `trekr.trees/<checkout>-<key>.tree`
next to `trekr.db` — and maps it read-only when its header checks out, so a
query's namespace costs a validation instead of an assembly, and its pages
are the page cache's, shared by every process on the store. Methods are not
in it; they stay demand-loaded from SQL.

- **The key is everything the namespace is a function of**: each root's
  surface key and path in tree order (checkout plus every gem), the schema
  version, the format number, and the source text of the code that assembles
  and encodes it — so a rebuilt binary never reads a namespace an older
  assembly produced.
- **Never written in place.** A snapshot is written to a temporary name,
  synced, and renamed over its final one; racing builders write identical
  bytes. A process that mapped a file keeps reading it after it is replaced or
  unlinked, and switches when its key moves, as the LSP always did.
- **Checked on open**: magic, format, key, length and a checksum over every
  byte. Any mismatch — truncated, another format, another key — is rebuilt and
  rewritten, never read.
- **One per checkout.** Writing a snapshot retires the checkout's previous
  ones, since refresh-on-save moves the key with every save. What that leaves
  — a key that moved with no query since, a checkout that is gone, a
  temporary file a dead writer left — `--gc` removes (`snapshots` in its
  `--json`).

A snapshot is built by whichever query first finds none for the current key.

### `resolve/` — which method does this call site run?

The ladder, tried in order, stopping at the first rung that names a type:

| rung | how the type is established | confidence |
|---|---|---|
| `self` | the enclosing scope **is** the receiver — a language rule, no inference | 1.0 |
| `includer` | a call inside a module, resolved through the classes that mix it in | agreeing / includers |
| `const` | `Foo.bar` — resolve `Foo`, look up a *class* method | 1.0 |
| `local:new` | `x = Foo.new` | agreeing / total |
| `local:const` | `x = Foo` — holds the class, so `x.bar` is a class method | agreeing / total |
| `literal` | `out = []` — core knows what an Array is | agreeing / total |
| `sig` | an inline Sorbet `sig` on the method the value came from | agreeing / total |
| `sig:param` | the parameter's declared class, from `params(...)` | 1.0 |
| `sig:step` | one call on an already-typed local, via that method's `sig` | agreeing / total |
| `rbi_dsl` | resolved, then redirected from a Tapioca `.rbi` to the model | |

`sig:param` exists because half of graph_weaver's untyped local receivers turned
out to be method *parameters* — they have no assignment to chase, so every rung
that looks for one is structurally blind to them, and a signature had already
said what they are. `sig:step` is deliberately **one** step: rwr's D61 measured
70 % of returns ending in another call, so the recursive version drowns while
the single sig-backed hop pays. A test asserts the second hop is refused.

Once a type is settled the method is found by Ruby's own lookup, so a hit is
exact rather than ranked (DEC-011). Below the ladder is **residue**: the
receiver shape as the reason, plus candidates ordered by named tiers a reader
can check — owner in the enclosing class's ancestors, then shares a namespace,
then same file, then arity fits, then arity does not. No invented weights.

Two things a naive implementation gets wrong here:

- **A bare call in a class body dispatches on the class.** `validates :name`,
  `prepend Foo`, and `class_attribute :x` are class-method calls even though a
  `def` written in the same place is not. "Is a `def` here a singleton method"
  and "what is `self` for a call here" are different questions; the extractor
  records the second separately.
- **`Foo.bar` walks the superclass chain, not the MRO.** Included modules
  contribute no class methods; `extend`ed ones do, along with *their* includes.

### `gems/` and core — making the index contain the answers

Two of the three reasons a lookup failed were "the thing is not in the index".
Both are now addressable without a Ruby toolchain.

**Core** is [`src/tree/core.rb`](../src/tree/core.rb): ~1000 lines of ordinary
Ruby with empty bodies, read at tree-build time by the same `extract()` a
checkout goes through (DEC-015). The ancestry is what earns it — every class
gets its implicit `< Object`, and a singleton chain continues into
`Class → Module → Object`, which is what makes `puts`, `raise`, `Foo.new`, and
a class body's `prepend` resolve at all.

**Gems** come from reading, never from running (DEC-016): `Gemfile.lock`
parsed directly, sources found by convention across `vendor/bundle`,
`$GEM_HOME`, `$GEM_PATH`, rbenv, rvm, asdf, Homebrew and system paths. Each gem
is its own checkout rooted at its unpacked directory — which already encodes
`name-version` — so two projects resolving the same version share one index and
the second pays nothing (DEC-017). Only `lib/` is walked.

**A gem version outlives the projects that used it** unless collected, because
it still maps its own blobs and so is never an orphan. `trekr --gc` removes the
checkouts no future index could reach — a gem on disk that no surviving repo's
bundle names, a repo or gem whose root is gone — spares anything an index saw
within `--older-than` (default 7 days), and then deletes only the blobs no
remaining checkout maps. `checkout.kind` says which rule applies, and a gem's
`indexed_at` moves every time a bundle names it, inside the index's own
transaction. Collecting is safe because it is undone by the next index: a
lockfile naming a gem the store lacks indexes it, collected or never seen
(DEC-049).

A gem the lockfile names and disk does not have is **reported**, in the text
output and as `gems.missing` in `--json`. It is a hole in every answer that
would have come from it, and a silent hole is indistinguishable from a method
that does not exist.

The layering is core → gems → checkout, so a gem may reopen core and the
checkout may reopen a gem, which is what Rails actually does.

**The resident front holds the tree.** `Tree::build(store, root)` is the whole
seam: it takes a store and a checkout root and returns a value with no borrowed
state and no background work. `--lsp` holds one per checkout, answers from it,
and rebuilds when the checkout's surface key moves — see [LSP front](#lsp-front).
Both it and every CLI query map the same snapshot file, so the namespace is in
memory once per machine rather than once per process.

### `resolve/refs.rs` — references narrowed by receiver

`rg -w save` finds every `save`. Ruby LSP matches method references by bare
name; Rubydex does not attribute method calls at all. What makes an answer
useful is knowing which of those sites can reach *this* method, and the ladder
already knows.

| tier | meaning | listed? |
|---|---|---|
| **confirmed** | the receiver's type resolves and Ruby's lookup from it lands on the queried method | yes |
| **possible** | the receiver is untyped and nothing rules the site out — ranked by proximity | yes |
| **excluded** | the receiver resolves elsewhere, or the arity does not fit | **counted**, and listable with `--include-excluded` |

`Widget#save` and `Widget.save` are different questions. A bare name narrows
nothing, so it keeps the whole-mention view with each call site naming the owner
it reaches.

**The exclusion count is broken down, because its three reasons are not equally
strong** (DEC-021). Only `different_owner` is positive evidence. `no_such_method`
is the largest and the weakest: Rails writes `delegate :where, to: :all`, so a
DSL-defined method is absent from the index without being absent from the
program. Behaviour is unchanged — those sites are not listed — but the claim is
split so a caller can see how much of it is inference.

Files are reparsed rather than read from the stored call rows. The ladder needs
the file's assignments, which are deliberately not stored (DEC-012), and
reparsing means an edit since the last index is still tiered correctly. The
index's only job here is to say which files are worth opening.

### `store/` — SQLite, WAL, no cleverness

Schema in [`src/store/schema.rs`](../src/store/schema.rs); it is the authority
and this table is its summary.

```text
blob(id, oid UNIQUE, lines, parse_errors)
  def(blob_id, name, kind, nesting, singleton, visibility, params,
      via, target, sig_returns, line, col, end_line)
  ancestry(blob_id, nesting, relation, target, line, col)
  const_ref(blob_id, name, nesting, line, col)
  call_site(blob_id, name, recv, recv_text, nesting, argc, block, line, col)

checkout(id, root UNIQUE, indexed_at, kind, surface_key, map_key, git_state)
  gem_use(checkout_id, gem_root)            ← which bundles name which gem
  file(checkout_id, path, blob_id)          ← the only table naming a path
```

**No table under `blob` may mention a path, a checkout, or a repository.**

Two encodings share a column apiece rather than earning a table:

- `nesting` — lexical scopes innermost first, joined by `;`. Ruby constant paths
  are `[A-Za-z0-9_:]`, so the separator cannot occur inside one. The stack is
  stored rather than derived because `module A::B` opens **one** scope, not two,
  and only the stack shows that.
- `params` — `kind:name` pairs joined by `;`, using Ruby's own
  `Method#parameters` vocabulary (`req` `opt` `rest` `post` `keyreq` `key`
  `keyrest` `block` `nokey`). Arity is derivable; a glossary of ours is not
  needed.

`argc` is NULL when a splat makes the count unknowable — an honest absence
rather than a sentinel.

## CLI

Operations are flags, not subcommands (rq's convention), so no word is reserved
and the default action stays free for the query verbs layer 3 will add. Every
command that prints honors `--json` / `--ndjson`.

| command | answers |
|---|---|
| `--index [PATH]` | scan a checkout and store what is new |
| `--status` | what is indexed, per checkout, plus the shared totals |
| `--symbols FILE` | one file's definitions, in source order |
| `--refs NAME` | every mention of a name in this checkout |
| `--def FILE:LINE:COL` | what is the name here, and where is it defined |
| `--ancestors NAME` | the linearized ancestor chain |
| `--drop [PATH]` | forget a checkout's file map |
| `--gc [--dry-run] [--older-than AGE] [--vacuum]` | remove checkouts nothing can reach again, the blobs only they mapped, and tree snapshots no checkout's index names |
| `--usage [--days N]` | which commands and editor features get used, by whom, how often empty, how slow (see below) |

`--refs` is **name-level, not resolved**: two unrelated `Config` classes both
answer, and so does every `#save` on every receiver. Each row says what sort of
mention it is and, for a call, what shape the receiver had — disclosure instead
of a guess. Narrowing it is layer 3's job.

| exit | meaning |
|---|---|
| 0 | something was indexed, or a query matched |
| 1 | nothing matched, nothing to do — a definitive answer |
| 2 | the request could not be served (not a repo, unreadable file) |

`--def` reparses the one file with Prism rather than reading stored spans, so
it answers correctly on a file edited since the last index.

Every answer carries `status` (`resolved` | `residue`) and `confidence`. For
constants that confidence is 1 or 0, and **that is not a hedge**: the ladder
above is Ruby's own algorithm, so within the indexed set a hit is exact rather
than ranked. The uncertainty that does exist is reported as evidence —
`scopes_tried`, `unresolved_ancestors` — rather than smeared into a number that
would look like a measurement (DEC-008). A method call is `residue` carrying its
receiver shape, which is where layer 3 will start.

`$TREKR_DB` overrides the database path (default
`~/.local/share/trekr/trekr.db`); the e2e tests use it for isolation. Beside
the database, `trekr.trees/` holds one tree snapshot per checkout (DEC-065) — a
cache: deleting it costs each checkout one rebuild.

### Usage counts

`src/usage/` counts every use of the engine so features can be kept, cut or
improved on evidence (DEC-063). One table, in its own file
(`trekr.usage.db` beside the store, `$TREKR_USAGE` to move it or `off`):

```text
usage_daily(day, surface, feature, flags, origin, outcome, latency, cold, count)
  PRIMARY KEY (every column but count), WITHOUT ROWID
```

- `surface` `cli` | `lsp`. `feature` is the command (`def`, `refs`, `card`,
  `dead`, `index`, …, `invalid` for a call that did not parse) or the LSP
  operation without its `textDocument/` prefix, plus the lifecycle events
  `session`, `resume`, `reload`, `reload-failed`, `retire`, `index`.
- `flags` names the knobs and variants reached for (`json`, `explain`, `bare`,
  `by-name`, `snapped`, `stale`, `require`, `cut`), never their values.
- `origin` is rq's caller taxonomy (`claude-code`, `cursor`, `ci`, `human`,
  `piped`); an LSP session with no agent in its environment is labelled by the
  client's `clientInfo.name`.
- `outcome` `hit` | `uncertain` (ambiguous, confidence below 0.5, or residue
  with ranked guesses) | `empty` | `not-indexed` | `cancelled` |
  `error:<kind>`.
- `latency` is a decade bucket (`<1ms` … `10s+`); `cold` marks an LSP
  session's first request, including the first after a hot-reload resume.

No query text, path, or repository name is stored. The CLI counts in `run()`
after the command's output is written; the LSP after the response is sent.
Rows older than 90 days are pruned on write. `--usage` folds the rows into one
line per feature; `--json`/`--ndjson` emit the rows themselves.

## LSP front

`trekr --lsp` (`src/serve/`) is a resident front over the same store: the CLI's
answers in LSP's clothing, plus what only a resident process can do cheaply.
The editor owns its lifetime. When its binary is replaced, the server becomes
the new binary in place (DEC-050).

| module | owns |
|---|---|
| `mod.rs` | the loop: initialize, dispatch, shutdown/exit, error codes, idle warm-up, answers rewritten into the client's path spelling |
| `wire.rs` | stdio framing: stdin read on the loop's own thread from the raw descriptor (no hidden read-ahead), stdout written by a thread |
| `inbox.rs` | reading the wire ahead, so `$/cancelRequest` is seen before the request it withdraws is reached, and mid-scan |
| `state.rs` | per-checkout trees (rebuilt when the surface key moves) and completion listings; documents — the editor's copy, or a disk read revalidated by mtime+length |
| `handlers.rs` | the nine agent operations, syntax diagnostics, `require` strings as links |
| `gather.rs` | how much of a references answer is kept, in what order, and what is said about the rest (DEC-056) |
| `require.rs` | which file a `require` string names: finding them in a file, the static load path, Ruby's search rules (DEC-053) |
| `vars.rs` | a file's variables, pure: which writes each local read can see, each ivar with its written class and `self` (DEC-064) |
| `variables.rs` | a variable under the cursor: definition, references, highlight, hover; an ivar's class files, read when asked (DEC-064) |
| `doc.rs` | a definition's doc comment and its signature as written, read from its file when asked (DEC-052) |
| `complete.rs` | completion (DEC-040), and the chosen item's doc on resolve (DEC-052) |
| `fresh.rs` | refresh-on-save and the background `--index` child (DEC-039) |
| `convert.rs` | UTF-16 ↔ byte columns, spans, a per-file line index |
| `reload.rs` | hot reload: the launch-path stamp, probing the new build, the handoff file, the exec |
| `log.rs` | the ndjson debugging log, and the usage counts for LSP operations and lifecycle events |

**Mapping ranked answers onto LSP**, which has no confidence field:

- `definition` returns the resolved site(s). Residue returns up to five ranked
  candidates, so the editor shows a peek list rather than jumping confidently
  to a guess; `hover` at the same position says, in words, that it is one.
- `references` orders confirmed before possible and drops excluded — the order
  is the disclosure. On a class or constant it resolves every written constant
  and keeps those that land on the same FQN. It is bounded; see below.
- `incomingCalls` reports only the confirmed tier; its items are the calling
  methods, so the hierarchy can be walked.

**References are bounded, and say so** (DEC-056). A common name in a
monorepo has hundreds of thousands of call sites; nobody reads them in an
editor, and an agent's context cannot hold them. An answer keeps at most
`initializationOptions.referenceLimit` references (default 1000), and when
that leaves anything out the server sends one `window/showMessage` —
"showing 1,000 of 5,450 references to reload, confirmed callers first. For
all of them: `trekr --refs 'Topic#reload'`" — and logs a `references` event
with what was shown, found and read. What is kept depends on how it is asked:

| request | files read | kept |
|---|---|---|
| a method whose owner is known | all that call the name, unless `limit` confirmed callers are in hand first | the best `limit` by evidence: confirmed, then possible by proximity |
| a name whose receiver never resolved | page by page from the index, until `limit` are found | those, ordered by evidence |
| a method whose owner is known, with a `partialResultToken` | nearest the definition first, until `limit` are found | streamed as `$/progress` batches, one per chunk of files read — the definition first, each batch ordered by evidence; the response is `[]` |

The bare name stops early because its "confirmed" means a typed receiver that
finds *some* method of that name, so a full scan to promote those would buy
nothing about the method asked after. Its files come from
`Store::files_calling_page` rather than `files_calling`: listing every file
that calls `to` means reading every call of it (2.5 million rows at thirty
times discourse) before the first file is opened, and the answer needs a few
thousand. The page query's plan is pinned, because with `sqlite_stat4` saying
the name is everywhere the bundled SQLite otherwise walks every file of the
checkout and sorts their calls — 0.7 s a page at ten times discourse.

A stream stops early because it cannot take back what it sent. Its files are
read nearest the definition first (its own file, its directory, outward), so
the prefix comes from the code most likely to be about the method. Neither
early stop is ranked across the whole checkout, and the message says so:
"nearest the definition first", never "confirmed callers first".

A streamed scan checks for `$/cancelRequest` before each batch as well as
before each chunk, so a withdrawn request sends nothing further.

Neither VS Code's language client (`vscode-languageclient` 9) nor Claude
Code's LSP tool sends a `partialResultToken` for references, so the unstreamed
path is the one editors take today. Claude Code shows no `window/showMessage`
either: an agent sees a list of exactly `referenceLimit` locations and no
note that it was cut.

**Hover is for a person reading code** (DEC-052), so it shows what the
definition is and leaves out how it was found:

````text
```ruby
def ActiveRecord::Associations::ClassMethods#has_many(name, scope = nil, **options, &extension)
```
_caveat, only when the answer is a guess_
first paragraph of the doc comment · **Deprecated** / **Returns** from YARD
Defined in [`lib/active_record/associations.rb:1302`](file://…#L1302) · gem `activerecord-8.0.5.1`
````

The signature is the definition's own text after its name — parameters with
their defaults, a class's `< Parent`, a constant's value — under the FQN the
tree settled on. `status`, `confidence` and `resolved_via` stay in `--json`,
where a caller branches on them; in a hover they were noise. What replaces
them is a sentence, and only when it matters: `Best guess — … 3 other
definitions of save exist` for an ambiguous pick, `receiver type unknown — 7
possible definitions: …` for residue. Never a number.

The doc is **read when asked, not indexed**. The site the tree returns names a
file and a line; the session reads that file (the editor's buffer if open, else
disk, cached while its mtime and length hold) and walks up from the definition
over the contiguous comment block. The reader is `doc::doc_above`, a pure
function of text and line. A blank line ends the block; a Sorbet `sig` between
comment and `def` is stepped over; a bare `private` is not, since a comment
above it heads a section. Magic comments, tool directives (`rubocop:`,
`:call-seq:`, rbs-inline's `#:`) and RDoc's `#--`…`#++` are dropped, and
`:nodoc:` means no doc. The summary is the first paragraph, capped at six lines
and 400 bytes; of YARD's tags only `@return` and `@deprecated` are kept, because
the signature already shows the parameters.

A file edited since it was indexed moves its definitions. The definition is
found at the indexed line, or else as the one definition of that name, kind and
scope in the file as it is now. If neither holds the hover shows no doc at all:
a comment attached to the wrong definition is worse than none.

`completionItem/resolve` shows the same doc and signature for the one item
selected. The list itself carries none: it can be hundreds of items, and reading
a file per item would stall every keystroke. `workspace/symbol` shows none for
the same reason.

**A `require` string is a path** (DEC-053). `definition`, `hover` and
`documentLink` on the string literal of a `require`, `require_relative`,
`load` or `autoload` answer with the file it loads, opened at its top. The
whole literal is the origin, wherever the cursor is in it — a
`LocationLink`'s `originSelectionRange` when the client takes links — because
Ruby resolves the whole string and a directory is not a location.

`require.rs` is two pure functions and one that reads the disk.
`requires_in` parses a file with Prism and returns each call whose path is a
literal, or a literal in disguise (`File.expand_path("x", __dir__)`,
`File.join(__dir__, "x")`, `File.dirname(__FILE__) + "/x"`, `"#{__dir__}/x"`,
`Rails.root.join("x")`), cached on the document per edit. `resolve` follows
one the way Ruby would, given the directories and a file-exists test.
`LoadPath::for_checkout` builds the load path, in order:

1. the checkout's `lib/`, `spec/`, `test/` — rspec-core and `rails test` add
   the last two, which is where `rails_helper` lives;
2. its path gems' `lib/`, from `Gemfile.lock`'s `PATH` sections;
3. each gem the bundle resolves (`Store::gems_used`), its `lib/`;
4. the standard library of the Ruby those gems were installed into, and its
   arch directory — beside the gem for rbenv, asdf, Homebrew and system Rubies,
   under `.rvm/rubies` for rvm. A `vendor/bundle` names no Ruby and gets none.

Per directory, `x.rb` then the compiled `x`, as `rb_find_file_ext` does. A
compiled extension first on the path is named in the hover and is no
definition. Several matches are all returned in path order, since the order
among gems here is not bundler's. `documentLink` links only a string with
exactly one file behind it; the others are left to `definition`'s peek list.

The session keeps each checkout's load path until its `gems_used` changes.
Gem and stdlib directories are listed once, so a lookup stats only the few
that hold the path's first component; the checkout's own directories are stat'ed each
time, because they change. On discourse (308 directories) the build is
8.6 ms, a warm `documentLink` over 70 requires 0.4–0.6 ms, and `definition` on
one 0.1–0.26 ms.

**A variable is answered from the file, not the index** (DEC-064).
`definition`, `references`, `documentHighlight` and `hover` on a local,
parameter, `@ivar` or `@@cvar` are answered before anything else is tried.
Nothing is stored; `vars::analyze` walks a file's Prism tree once per edit,
cached on the document like its facts.

- **A local** goes to the writes its value can come from. Prism already says
  which names are locals and how many block scopes up each lives (`depth`);
  the walk adds flow. A write replaces what reaches; each branch of an
  `if`/`unless`/`case`/`&&`/`||` starts from the same state and the branches'
  results are merged; `||=` keeps the old value as a possibility and `+=`
  does not; a `rescue` sees every write its body made; `def`, `class` and
  `module` start empty. A loop body (`while`, `for`, any block) may run again,
  so it is walked twice: the first pass records which writes each body holds,
  and the second lets a read at its top see a write at its bottom. Every
  binding form is a write — parameters of every shape, `|a, (b, c); d|`,
  `in {x:}`, `=> x`, named captures, `rescue => e`, `for x in`, `a, b = …`.
  A write under the cursor is its own definition. References and highlight
  are every mention in the same scope.
- **An ivar** goes to every write in its class and the class's ancestors:
  `@x =`, op-assigns, multi-assign targets, `attr_writer`/`attr_accessor`
  (symbol or string), and `instance_variable_set(:@x, …)` on `self`.
  `initialize` first, then by file and line. Which object it lives on is
  decided where it is written: in an instance method (or a `define_method`
  block) it is the instance's, found through `Tree::ancestors`; in a class
  body, a `def self.x` or `class << self` it is the class object's, found in
  that class's own files only. `Tree::sites` names the files — reopened
  classes included, gems and core left out, at most 64 — and each is parsed
  when asked; a mention counts when `Tree::scope_fqn` places its written
  nesting on the chain. With no checkout, no nesting, or a class the tree
  does not know, only the file at hand is searched. Nothing is guessed from
  outside the chain: a module's ivar set only by the classes that include it
  has no definition. `@@x` follows the same rules, shared by the class and
  its instances.
- **Highlight** is the file's own mentions, writes marked as writes; for an
  ivar the written nesting and `self` must match, which needs no index.
- **Hover** is one line: ``local `total` · assigned at line 12 (and 1
  more)``, ``ivar `@name` · set in `initialize` (and 1 more)``.

Globals are not answered. On discourse (release build, other work on the
machine) definition, highlight, hover and references on a variable take
0.1–0.8 ms median. The first ivar question about a class reads its files,
up to 11 ms (`TopicQuery`), on top of the tree the session builds once for
every operation. Analysing the largest file, an 8,706-line spec, takes 6 ms,
half of it Prism's parse.

**What reflects unsaved edits:** the open file's own facts (outline, position
lookup, diagnostics, completion) and every file scan (`references`,
`incomingCalls`) — open buffers overlay disk. **What does not:** the tree,
which is assembled from the index as of the last save. A method added but not
saved is not yet visible from *other* files; saving moves the index
(`Store::refresh_file`) and the tree follows.

**Threads.** Requests are answered on one thread; the tree is not shared. Two
things leave it: a file scan's reading and parsing fans out on rayon and
returns facts, with tiering kept on the main thread; and completion's member
listing is built by a worker on its own connection and tree, which hands back
only the listing (DEC-044). The listing streams every method row past the
tree rather than loading them into it (DEC-045), so the session's tree stays
demand-loaded. Background indexing is a child process, not a thread (DEC-039).
The child is spawned with `TREKR_BACKGROUND=1` and lowers itself before it
starts a thread — nice +10, disk I/O at macOS `IOPOL_UTILITY` or Linux
best-effort level 7 — and logs what the kernel then holds as `index_priority`.
The child does it rather than the spawn, so a hand-run `trekr --index` stays
at full speed. Not the lowest I/O tier: the index writes under SQLite's write
lock, and a starvable tier stretches the lock a save waits on (DEC-062).

**Hot reload** (DEC-050). The loop stats the file it was launched as (argv[0],
through symlinks) at every quiet moment, and every 2 s when idle. When that
file changes, the loop waits for a moment with nothing unanswered and no index
child running. It then probes the new build (`TREKR_LSP_PROBE`) and writes a
`0600` handoff. The handoff holds the `initialize` params, the client's
registrations, the editor's buffers, and any partly read message. The loop
flushes its output and `exec`s the new build with `TREKR_LSP_RESUME` pointing
at the handoff. The pid and pipes survive, so the new build resumes without a
handshake. If the new build does not run, the old one keeps serving. If it
runs but cannot resume, the server retires (exits) so the client restarts it.
The log records `reload`, `resume`, `reload_failed` and `retire`.

## Measurements

2026-09-27, Apple M2 (8 cores), release build at 4b035da, warm page cache, on
a machine shared with other work (load 3–4). Reproduce with `make bench`. Cold
time is a single run — a second one is by definition not cold; everything else
is a median of five. Run-to-run variance is about 20 %, so these are two
significant figures at best and are quoted that way.

| corpus | files | cold | no-op reindex | defs | const refs | call sites | DB |
|---|---:|---:|---:|---:|---:|---:|---:|
| rails | 3,307 | 1.9 s | **36 ms** | 65,279 | 91,170 | 352,656 | 65 MB |
| discourse | 11,301 | 8.0 s | **80 ms** | 76,869 | 206,575 | 1,377,499 | 234 MB |
| CRuby | 7,931 | 2.9 s | **67 ms** | 56,843 | 172,117 | 711,531 | 78 MB |
| mastodon | 3,270 | 5.3 s | **33 ms** | 21,615 | 27,157 | 213,576 | 80 MB |
| graph_weaver | 250 | 1.4 s | **24 ms** | 31,508 | 59,288 | 75,333 | 29 MB |

Cold time and DB include the checkout's **gems**, indexed once per machine and
shared: discourse brings 297 of them. DB is what each corpus added to one
shared store, in this order, so a gem an earlier corpus brought is not counted
again. A re-index still parses nothing.

**At monorepo scale.** discourse's Ruby replicated N times, every file wrapped
in its own module so that no two blobs and no two constants are alike —
content addressing would otherwise flatter every number. 30× is 336k files,
the size of the monorepo DEC-035 measured. Single runs, the machine shared.

| | 1× | 10× | 30× |
|---|---:|---:|---:|
| cold `--index` (DEC-057) | 6.3 s | 40 s | 240 s |
| cold `--index`, private memory peak | 100 MB | 180 MB | 390 MB |
| DB | 0.26 GB | 1.5 GB | 4.4 GB |
| no-op `--index` | 0.10 s | 0.78 s | 4.8 s |
| — with git's untracked cache primed and fsmonitor on | | | 0.42 s |
| one-file `--index` | 0.15 s | 0.92 s | 9.4 s |
| `--def` | 0.33 s | 1.1 s | 2.9 s |
| `--def`, private memory peak | 0.10 GB | 0.45 GB | 1.06 GB |
| LSP first answer (tree build) | 0.23 s | 4.0 s | 2.7 s |
| LSP live heap, tree + completion listing | 95 MB | 485 MB | 1.23 GB |

What grows with the repo and is paid per question is the tree: every CLI query
builds all of it (3.5 s at 30×, 2.1 s of that decoding declaration rows), and
an LSP session holds it and the completion listing in private memory. The
30× one-file reindex was mostly git walking a worktree whose caches had not
been primed; with them, a no-op is dominated by trekr reading `git ls-files`
into the file map.

Cold time is the noisiest figure here — one run, and CRuby has swung between
2.3 s and 3.9 s across runs on page-cache state alone. Treat it as one
significant figure.

- **A no-op reindex parses nothing** — the property the whole design exists for.
  About 40 ms of rails' 61 ms is the three `git` calls (`ls-files -s` 7 ms,
  `diff-files` 9 ms, `ls-files -o` 24 ms); the rest is rewriting the file map.
  Rubydex pays 177 ms on rails and 845 ms on GitLab for the same no-op (PLAN
  §8), *and* pays it again on every process boot.
- **A second worktree costs ~0.2 s and zero parses.** A `--shared` clone of
  rails indexes with `parsed: 0` — the facts were already on disk.
- **One edited file costs ~0.15 s on discourse** (2026-09-26), about 60 ms
  of it the scan and 60 ms the write — the edited blob's facts and the commit;
  the 11k-file map is diffed and only its moved rows written (DEC-048). It had crept to
  0.64 s once every index that parsed anything ran a full `ANALYZE` (DEC-042),
  and to 0.27 s before the scan used git's untracked cache (DEC-043).
- **The no-op scan is git's untracked-file walk**, and `git status` skips most
  of it when `core.untrackedCache` is on: discourse no-op 165 → 86 ms, rails
  64 → 41 ms. With the cache off it is the same walk as before (DEC-043).
- Cold time is not the headline and is not uniformly better than Rubydex's
  (rails 1.5 s vs their 1.35 s index+resolve; discourse 3.2 s vs their 2.4 s).
  About 0.3 s of ours is the `ANALYZE` that keeps queries fast — a cost paid at
  write time so it is not paid at read time.
  The difference is that ours happens once per machine and theirs happens once
  per process, and ours ends with the facts on disk rather than in RAM. It is
  also not yet a like-for-like comparison: they resolve, and this layer does not.

Fact shape across all three corpora (2.2 M call sites):

| receiver shape | share | what it costs to resolve |
|---|---:|---|
| implicit | 44.6 % | nothing — the enclosing class is the receiver |
| other | 26.1 % | chains, literals, operators — the residue |
| local | 14.3 % | a constructor / identity walk |
| const | 11.4 % | constant resolution |
| ivar | 3.1 % | an assignment walk |
| self | 0.4 % | nothing |

So **56 % of call sites need no inference at all**, and 71 % are reachable by
the first three rungs of the ladder. (rwr measured implicit self at 53–66 %;
the gap is a counting difference — this figure includes operator calls, which
inflate `other`.)

Sorbet `sig` extraction is exercised at scale on `graph_weaver`: 3,757 of its
methods get a concrete return class. None of the three corpora above use
Sorbet, so the sig path contributes nothing to their numbers.

**What receiver narrowing is worth.** Twelve method names on rails chosen for
heavy collision — each defined 5+ times and among the most-called in the repo —
querying the owner that the most call sites actually resolve to, before and
after Rails DSL modelling:

| | before | after |
|---|---:|---:|
| **confirmed** | 8 168 (32 %) | **11 919 (47 %)** |
| **possible** | 10 933 (43 %) | 10 740 (42 %) |
| **excluded** | 6 196 (24 %) | 2 634 (10 %) |
| — of which positive evidence | 795 | 1 012 |
| — of which "no such name" | 5 072 (82 %) | **1 104 (42 %)** |
| — of which arity | 329 | 518 |

`rg -w` returns all 25 293 undifferentiated, and Ruby LSP returns them by bare
name.

The DSL work moved two things at once. Confirmed rose 15 points because a
`delegate`d method on a *constant* receiver — `Topic.where` — goes straight from
"nothing defines this" to "confirmed here". And the weak exclusion reason fell
by 78 %, which is DEC-021's demoted claim becoming sound: `ActiveRecord::Querying#where`
alone went from 26 confirmed to **1 197**.

Note for comparability: the harness picks the owner that the most call sites
resolve to, so the owner it asks about *changed* as resolution improved —
`Arel::SelectManager#where` became `ActiveRecord::Querying#where`. The twelve
names are the same; the twelve queries are not, and that is the harness working
rather than drifting.

**Hand-checked precision**, 22 samples read against their source: 12 of 12
`confirmed` were genuinely calls to the queried method, and 10 of 10
positive-evidence exclusions genuinely went elsewhere (`String#size` ruled out
of `Array#size`, `Integer#to_s` out of `Kernel#to_s`). A sample that small
bounds nothing tightly; it is a check that the mechanism is not systematically
wrong, not a precision figure.

**Cost.** A refs query pays the tree build plus a reparse of every file that
mentions the name: 360–400 ms on rails against a 210 ms tree build, so the scan
itself is 150–190 ms even for 6 820 sites. Whole-index queries are therefore
squarely in the territory where a resident front would pay for itself twice —
once for the tree, once for the parse cache.

**What the resident front is worth.** `trekr --lsp` against rails, driving
the built binary over stdio:

| operation | first call | warm median |
|---|---:|---:|
| `textDocument/definition` | 463 ms | **0–1 ms** |
| `textDocument/documentSymbol` | 1 ms | **0 ms** |
| `textDocument/hover` | 1 ms | **0 ms** |

Reading the definition's file for its doc (DEC-052) adds 0.3–1.3 ms to a hover
whose definition file the session has not read yet, and 0.2–0.45 ms once it
has. Measured on discourse and rails, median of five, OS page cache warm. The
largest was `has_many`, a 1,909-line file.
| `textDocument/references` (`each`) | 257 ms | **25 ms** |

The first call pays the 210 ms tree build; every one after it pays nothing. A
references query drops from ~245 ms on the CLI (34 ms of scan behind a 210 ms
rebuild) to 25 ms — the scan alone, which is exactly what the economics
predicted. Go-to-definition goes from a fifth of a second to under a
millisecond, because a resolved position needs only the cached tree and the
cached parse of the open buffer.

This is the whole argument for the front, and it is now measured rather than
projected. The engine stays daemon-free either way: everything here the CLI
also answers, from the same store, without a process running.

**The tree layer is rebuilt on every invocation, and that is the design.**
`--refs` needs no tree; `--ancestors` needs a whole one. The gap between them is
what a full rebuild from SQL costs:

| corpus | rebuild | total for `--ancestors` |
|---|---:|---:|
| rails | 45 ms | 54 ms |
| discourse | 166 ms | 174 ms |
| CRuby | 30 ms | 39 ms |
| mastodon | 146 ms | 154 ms |
| graph_weaver | 55 ms | 65 ms |

*(2026-09-27. The paragraphs below are history: the 202 / 309 / 116 ms they
discuss were this table's earlier values.)*

**This has now crossed DEC-007's threshold and the decision needs revisiting.**
The progression is instructive: 43 ms with constants alone, 120 ms once method
tables arrived, 202 ms once gems did. CRuby stayed at 116 ms because it has no
gems, which confirms where the cost is — assembling a bigger namespace, not
querying it. Batching the per-gem queries from 258 down to 3 moved it 233 → 221
ms, so the round trips were never the problem.

The indicated move is the one DEC-007 named: cache one built tree **per
process**. That pays for a resident LSP front (PLAN Phase 4) and does nothing
for a one-shot CLI invocation, which builds it once regardless. So the CLI's
honest cost for a resolved answer is now ~200–300 ms, and driving it lower is
Phase 4's problem, not a reason to make the tree incremental.

PLAN §4 said keep the tree cheap to rebuild rather than clever to patch, and
gated that on a measurement. At well under 100 ms for 11k files there is nothing to
invalidate incrementally — memoizing per namespace on contributing blob OIDs
would be paying interest on a debt we do not have (DEC-007). Linearization *is*
memoized within a single build, because a file's every constant reference asks
for the chain of the same enclosing class.

Sanity on real code: `--ancestors ActiveRecord::Base` in rails linearizes 40+
concerns with **nothing unresolved**; `--ancestors Topic` in discourse gets its
concerns in order and honestly reports `ActiveRecord::Base` unresolved, because
gems are not indexed yet.

**How much of real code resolves.** 120 constant references sampled per corpus
(excluding tests), each asked through `--def` exactly as a caller would, at
three stages: checkout only, then with core, then with gems.

| corpus | checkout only | + core | + gems | now |
|---|---:|---:|---:|---:|
| rails | 82 % | 92 % | 98 % | **98 %** |
| graph_weaver | — | — | — | **98 %** |
| discourse | 78 % | 87 % | 91 % | 91 % |
| CRuby | 73 % | 84 % | 84 % | 80 % |
| mastodon | — | — | — | 72 % |

Rails' remaining residue is a single name (`::Rack::Cache::MetaStore`, an
optional adapter not installed). CRuby did not move on the last step because it
has no `Gemfile.lock`, and its residue is CRuby-internal (`Primitive`,
`TOPLEVEL_BINDING`, `WIN32OLE::ARGV`) — things no gem index would supply.

`mastodon` and `CRuby` are the two low numbers and they are low for different
reasons. Mastodon has only 75 of its 344 locked gems on disk; its figure also
**predates** the implied-namespace fix below, which was prompted by exactly this
measurement and is not yet re-run. CRuby's residue is internal
(`Primitive`, `TOPLEVEL_BINDING`, `WIN32OLE::ARGV`) — nothing a gem index would
supply — and it moved 84 → 80 % only because excluding `sorbet/` changed which
rows the sample draws from.

**Discourse is a weak test of the gem step and should be read as one**: its
bundle was never installed on this machine, so 238 of its gems — the entire
Rails stack included — are named by the lockfile and absent from disk. The
index says so (`gems.missing`), and `ActiveRecord::Base` is still in its
residue.

The resolver did not change across those three columns. The index did — which
was the whole prediction, and it held.

**How much of real code's method dispatch resolves.** 120 call sites sampled
per corpus, each asked through `--def`:

| corpus | resolved | chain **complete** | chain **truncated** |
|---|---:|---:|---:|
| graph_weaver | 50 % | 50 % (60/120) | — (none) |
| rails | 39 % | 40 % (47/118) | 0 % (0/2) |
| CRuby | 33 % | 35 % (40/115) | 0 % (0/5) |
| discourse | 24 % | 30 % (29/96) | **0 % (0/24)** |
| mastodon | 20 % | 30 % (24/80) | **0 % (0/40)** |

**The gem hypothesis is confirmed, and no installed bundle was needed to do it.**
Split every sample by whether the ancestor chain the lookup walked was complete
— an unresolved ancestor means something the chain needed is not indexed — and
the result is unambiguous: **0 of 71 chain-truncated call sites resolved, across
every corpus.** Not one. Mastodon's blended 20 % is a third truncated samples
and discourse's 24 % a fifth; their chain-complete rates of 30 % are the honest
comparison, and the blend is exactly the flattering denominator this split
exists to avoid.

What that leaves is the real ceiling. On rails, where the index is essentially
complete (2 truncated samples out of 120), resolution is **40 %**. The residue
there is receiver shapes the ladder cannot type: `local` 28 % and `other` 16 %
of all samples. Rung contributions on rails: `self` 31 %, `const` 3 %,
`local:new` 3 %, `includer` 1 %, `literal` 1 %.

**The prediction on record was refuted.** graph_weaver — a Sorbet repo with
3 620 indexed sig returns — was predicted to move far more than rails on sig
strength. It is the best corpus at 50 %, but the sig rungs fired **once in 600
samples** (`sig:param` 1, `sig:step` 0). Its lead comes from `const` receivers
(18 % — it is a code generator, full of `Foo.bar`) and from having its gems
installed. DEC-018 records why: those sigs describe its *dependencies*, and its
own `lib/` has 570 defs with 36 sigs. rwr's 64 % is a property of signatures,
not coverage of call sites.

The three rungs added this session — `sig:param`, `literal`, `sig:step` —
contributed 1–4 points each, dominated by `literal`. Useful, and much smaller
than the diagnosis suggested; the diagnosis found that half of untyped local
receivers were method parameters, but most of those parameters have no `sig`
either.

**The gem hypothesis, confirmed positively (2026-08-25).** Session 5 could only
confirm it negatively: 0 of 71 chain-truncated call sites resolved, across every
corpus. discourse's bundle is now installed — 300 of its 349 locked gems are on
disk, and `--index` finds and indexes 281 of them (10,711 files); the 49 absent
are platform variants (`ffi` for linux, aarch64 builds) legitimately missing on
macOS.

Re-measuring discourse with those gems present, 60 stable-keyed positions:

| | bundle-less | bundled |
|---|---:|---:|
| ancestor chain **truncated** | 24 of 120 (0 % resolved) | **0 of 60** |
| `self` inside a class | 52 % | **83 %** |
| overall resolved | 31 % | **43 %** |

**The truncated bucket did not shrink — it disappeared.** Every sampled call
site's ancestor chain is now complete, which is what "0 of 71 resolved" was
evidence *for*: those sites were unresolvable because the index was missing an
ancestor, not because the ladder failed. `self`-inside-a-class closed most of
the distance to rails' 89 %.

Predictions held: 75–85 % was predicted for the class rate (83 %), "in the 40s"
for overall (43 %). At n=60 that is ±6 points, so read the last row as "rose by
about ten".

*Mastodon remains bundle-less and its columns above are unchanged.*

**Rails DSL modelling moved it again**, though a caveat first: the runs below
use a different sample seed from the table above, so at n=120 a swing under ~5
points is noise. rails went 39 % → 42 % (inside that band, so treat it as
unmoved); discourse went 24 % → **32 %**, which is outside it. The `includer`
rung tripled on rails (0.8 % → 2.5 %) as concerns' `ClassMethods` became
reachable. The prediction was "+2–5 points on AR-heavy corpora, not more";
discourse beat it and rails did not move, which is the right shape — an app
uses the DSLs, a framework defines them.

**Index time is dominated by the store write, not by parsing.** `--profile` on
discourse: scan 80 ms, parse 270 ms across 8 workers, **store-write 2 660 ms**.
Roughly 1.5 M row inserts through one SQLite connection at ~575k/s. Parse
speeds up 5× from one worker to four and then plateaus, because it was never
the majority. Anything that wants to make indexing faster should start here —
batched or multi-row inserts, or relaxing durability for the bulk load — and
not with the worker count (DEC-014). The shape differs from rq's, which is why
the same "more workers" advice reproduces there and flattens here: rq overlaps
its single writer with the parse so workers keep feeding it. trekr collected
every fact and wrote at the end until DEC-047; it now streams parsed files to
the writer too, so `--profile`'s `parse` is wall time that overlaps the write,
and its throughput line is per worker.

**With gems, the cost was the number of commits, not the rows** (DEC-041).
A cold discourse index is its own 11k files plus 297 gems, and each gem was its
own transaction. A commit writes every page the transaction dirtied, and the
name indexes are keyed randomly, so each small gem rewrote most of them: 2.8 M
rows took 10.1 s of store-write at 280k rows/s, against 750k rows/s for the
app's single write. Writing a bundle's gems as one transaction took the cold
index from **12.7 s to 7.6 s** (store-write 10.1 → 5.3 s, median of five,
interleaved) and rails' from 2.7 s to 1.6 s. Same rows, byte for byte.

**The query planner needs statistics, and this is not optional.** Without them
SQLite plans `--refs` as a nested scan of the checkout's files: `--refs new` on
rails took **90 seconds** for 13,684 rows. With `ANALYZE` run, the planner
reverses the join — files drive, a bloom filter rejects — and the same query
takes **66 ms**. `--index` regathers them once the store has grown a tenth
past the last analysis (DEC-042), and `PRAGMA optimize` on close covers the
rest, so neither a no-op nor a one-file reindex pays for a full `ANALYZE`. Anyone adding a query over these tables should check
`EXPLAIN QUERY PLAN` on a *populated* database — a fixture-sized one hides this
entirely.

| `--refs NAME` on rails | rows | time |
|---|---:|---:|
| `find_each` | 28 | 63 ms |
| `save` | 716 | 188 ms |
| `each` | 2,009 | 204 ms |
| `new` | 13,736 | 662 ms |

These were 9–89 ms when a bare name was a single query. It now names, for each
call site, the owner the call reaches, which builds the tree and walks each
site's receiver — the cost is that, not the query.

**Where the bytes go.** 291 MB for the three corpora *and their gems*, up from
236 MB without them. Gems cost about what the code they contain suggests —
rails alone went 47 → 65 MB for 86 gems / 1 916 blobs, the same ~9.4 KB per
blob as a checkout — so the watch-item resolves benignly, and the sharing means
a second project with the same lockfile adds nothing. The shape below is from
the pre-gem measurement and has not changed:

| | MB | share |
|---|---:|---:|
| `call_site` + its two indexes | 171 | **72.5 %** |
| `const_ref` + indexes | 37 | 15.8 % |
| `def` + indexes | 21 | 8.9 % |
| `file`, `blob`, everything else | 7 | 2.8 % |

Call sites are the index. Extrapolating linearly to a 100k-file repo gives
~1 GB. Two things that would move the number are recorded but **not** acted on
until there is a reason: the `other` receiver shape is 26 % of call-site rows
(so ~19 % of the whole database) and may not earn its bytes once the resolve
layer says what it can do with it; and `call_site_blob` / `const_ref_blob`
(34 MB, 14 %) exist for a foreign-key cascade that DEC-003 means never fires. Not
optimized, deliberately: the encoding is boring on purpose and there is no
measurement yet saying it needs to be otherwise.

*Provenance: the local `discourse` and `mastodon` checkouts carry no `.git`, so
discourse was staged into a scratch git repo to be measured. That is DEC-001
biting on a real corpus.*

## Known gaps

Deliberate, and cheap to close when they earn it:

- `Class.new` / `Module.new` bodies are owners but not lexical scopes; not
  modeled. Constants inside them will be attributed to the enclosing scope.
- `private_constant` / `private_class_method` are not read.
- Instance, class, and global variables are not in the index (not in PLAN
  §4's Phase 1 fact set). The LSP answers locals, ivars and cvars from the
  files themselves (DEC-064); globals are not answered anywhere.
- Multi-write constant targets (`A, B = 1, 2`) define nothing.
- `refine` is not modeled.
- Orphaned blobs are never collected (DEC-003).
