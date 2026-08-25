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
files. Files the working tree has changed (`git diff-files`) and files git has
never seen (`git ls-files -o`) are hashed the way git does —
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

A gem the lockfile names and disk does not have is **reported**, in the text
output and as `gems.missing` in `--json`. It is a hole in every answer that
would have come from it, and a silent hole is indistinguishable from a method
that does not exist.

The layering is core → gems → checkout, so a gem may reopen core and the
checkout may reopen a gem, which is what Rails actually does.

**A resident front would hold the tree, and nothing here prevents that.**
`Tree::build(store, root)` is already the whole seam: it takes a store and a
checkout root and returns a value with no borrowed state and no background
work. A resident process (PLAN Phase 4) holds one, answers from it, and rebuilds
when the checkout's blob set moves — which the store can already tell it,
because that is what `--index` computes. Nothing is built for this yet, and
deliberately: staleness detection written before there is a process to need it
would be a guess at its shape.

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

checkout(id, root UNIQUE, indexed_at)
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
`~/.local/share/trekr/trekr.db`); the e2e tests use it for isolation.

## Measurements

2026-08-24, Apple M2 (8 cores), release build, warm page cache. Reproduce with
`make bench`. Cold time is a single run — a second one is by definition not
cold; everything else is a median of five. Run-to-run variance is about 20 %, so
these are two significant figures at best and are quoted that way.

| corpus | files | cold | no-op reindex | defs | const refs | call sites | DB |
|---|---:|---:|---:|---:|---:|---:|---:|
| rails | 3,307 | 3.1 s | **77 ms** | 50,353 | 91,178 | 308,453 | 65 MB |
| discourse | 11,287 | 8.6 s | **129 ms** | 59,194 | 206,215 | 1,227,403 | 154 MB |
| CRuby | 7,931 | 2.3 s | **107 ms** | 56,552 | 171,994 | 666,534 | 72 MB |

Cold time and DB now include the checkout's **gems**, indexed once per machine
and shared: rails is 86 gems / 1 897 files on top of its own 3 307. CRuby has
no `Gemfile.lock`, which is why it did not move. A re-index still parses
nothing; the extra 13 ms of no-op is the gem scan (5 ms) and one
`has_checkout` per gem.

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
- **One edited file costs ~75 ms**, of which ~61 ms is the scan floor above.
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

**What the resident front is worth.** `trekr --serve` against rails, driving
the built binary over stdio:

| operation | first call | warm median |
|---|---:|---:|
| `textDocument/definition` | 463 ms | **0–1 ms** |
| `textDocument/documentSymbol` | 1 ms | **0 ms** |
| `textDocument/hover` | 1 ms | **0 ms** |
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
| rails | 202 ms | 212 ms |
| discourse | 309 ms | 318 ms |
| CRuby | 116 ms | 126 ms |

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

**What receiver narrowing is worth.** Twelve method names on rails chosen for
heavy collision — each defined 5+ times and among the most-called in the repo —
querying the owner that the most call sites actually resolve to:

| | sites | share |
|---|---:|---:|
| **confirmed** | 8 168 | 32 % |
| **possible** | 10 933 | 43 % |
| **excluded** | 6 196 | **24 %** |
| total same-name call sites | 25 297 | |

Excluded by strength: 795 resolve to a **different owner** (positive evidence),
5 072 define no such name, 329 wrong arity. `rg -w` would return all 25 297
undifferentiated, and Ruby LSP would return them by bare name.

The spread across names is the interesting part. `ActiveSupport::Testing::Declarative#test`
is **6 658 of 6 820 confirmed (98 %)** — the `test "…" do` DSL, an implicit
receiver in classes that include the module.
`ActiveRecord::ConnectionHandling#lease_connection` is 1 024 of 1 168 (88 %).
`Arel::SelectManager#where` is 26 confirmed against 1 368 excluded, because
almost every `where` in rails is on an untyped or dynamically-extended receiver.

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

**What the resident front is worth.** `trekr --serve` against rails, driving
the built binary over stdio:

| operation | first call | warm median |
|---|---:|---:|
| `textDocument/definition` | 463 ms | **0–1 ms** |
| `textDocument/documentSymbol` | 1 ms | **0 ms** |
| `textDocument/hover` | 1 ms | **0 ms** |
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
| rails | 202 ms | 212 ms |
| discourse | 309 ms | 318 ms |
| CRuby | 116 ms | 126 ms |

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

| corpus | checkout only | + core | + gems |
|---|---:|---:|---:|
| rails | 27 % | 38 % | **38 %** |
| discourse | 16 % | 21 % | 22 % |
| CRuby | 27 % | 38 % | 38 % |

Rung by rung on rails: `self` 31 %, `const` 3 %, `local:new` 3 %, `includer`
1 %; residue 62 %. And by what encloses an implicit receiver — the split that
diagnosed session 3's gap:

| enclosing scope | before | after core + includer rung |
|---|---:|---:|
| a class | 67 % | **89 %** |
| a module | 30 % | **78 %** |
| nothing (top level) | 0 % | 0 % |

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

**Core delivered; gems did not, and that is the finding.** The prediction was
that the gem-truncated and not-in-index method buckets would mostly reclassify.
Core did exactly that — `puts`, `raise`, `Foo.new`, `prepend` all resolve now,
and `self`-inside-a-class went from two-thirds to nine-tenths. Gems then added
nothing measurable, for two separate reasons:

1. **The binding constraint moved.** Rails' method residue is now dominated by
   receiver *shapes the ladder cannot type*: `local` 29 % and `other` 16 % of
   all sampled call sites, against `implicit` at 10 %. Those are locals holding
   the result of a method call whose return type is unknown — rwr measured that
   only 2.3–4.5 % of definitions have a syntactically resolvable return type.
   No amount of indexing fixes that; it wants the next rungs on the ladder.
2. **Discourse could not test the hypothesis** (see above): its gems are not on
   this machine, so `self`-inside-a-class stayed at 52 % against rails' 89 %,
   which is the gap gem indexing was supposed to close. Testing it properly
   needs a corpus whose bundle is actually installed.

The honest read: **for method resolution the index is no longer the limit —
receiver typing is.** Top-level calls (`self` is `main`) stay at 0 % and always
will, since `main` is an Object instance with no useful identity.

**What receiver narrowing is worth.** Twelve method names on rails chosen for
heavy collision — each defined 5+ times and among the most-called in the repo —
querying the owner that the most call sites actually resolve to:

| | sites | share |
|---|---:|---:|
| **confirmed** | 8 168 | 32 % |
| **possible** | 10 933 | 43 % |
| **excluded** | 6 196 | **24 %** |
| total same-name call sites | 25 297 | |

Excluded by strength: 795 resolve to a **different owner** (positive evidence),
5 072 define no such name, 329 wrong arity. `rg -w` would return all 25 297
undifferentiated, and Ruby LSP would return them by bare name.

The spread across names is the interesting part. `ActiveSupport::Testing::Declarative#test`
is **6 658 of 6 820 confirmed (98 %)** — the `test "…" do` DSL, an implicit
receiver in classes that include the module.
`ActiveRecord::ConnectionHandling#lease_connection` is 1 024 of 1 168 (88 %).
`Arel::SelectManager#where` is 26 confirmed against 1 368 excluded, because
almost every `where` in rails is on an untyped or dynamically-extended receiver.

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

**What the resident front is worth.** `trekr --serve` against rails, driving
the built binary over stdio:

| operation | first call | warm median |
|---|---:|---:|
| `textDocument/definition` | 463 ms | **0–1 ms** |
| `textDocument/documentSymbol` | 1 ms | **0 ms** |
| `textDocument/hover` | 1 ms | **0 ms** |
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
| rails | 202 ms | 212 ms |
| discourse | 309 ms | 318 ms |
| CRuby | 116 ms | 126 ms |

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

| corpus | checkout only | + core | + gems |
|---|---:|---:|---:|
| rails | 82 % | 92 % | **98 %** |
| discourse | 78 % | 87 % | 91 % |
| CRuby | 73 % | 84 % | 84 % |

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

| corpus | resolved | `self` | `const` | `local:new` | residue |
|---|---:|---:|---:|---:|---:|
| rails | **27 %** | 24 % | 0 % | 3 % | 73 % |
| discourse | **16 %** | 10 % | 4 % | 2 % | 84 % |
| CRuby | **27 %** | 19 % | 4 % | 3 % | 73 % |

Session 1 measured receiver *shapes* and predicted 56 % would need no
inference at all (implicit 44.6 % + self 0.4 % + const 11.4 %). The delivered
figure is 24 %. **That prediction was wrong, and the way it was wrong is the
useful part**: a receiver shape needing no inference is not the same as the
method being *findable*. Diagnosed by bucketing 150 implicit-receiver residues
per corpus:

| why the implicit call did not resolve | rails | discourse |
|---|---:|---:|
| resolved | 47 % | 42 % |
| enclosing scope not in the index (top level — `self` is `main`) | 9 % | 9 % |
| ancestor chain truncated by an unindexed gem | 5 % | 25 % |
| the name is defined nowhere in the index | 9 % | 10 % |
| the name is defined elsewhere, chain complete | 29 % | 13 % |

Three separate things, none of them a wrong turn on the ladder:

1. **`self` is not always a class.** ~9 % of implicit calls sit at the top
   level of a Rakefile, Gemfile, or migration, where `self` is `main` — an
   `Object` instance, and `Object` is not indexed.
2. **The method may not be in the index at all.** `puts`, `raise`,
   `block_given?` (Kernel); `prepend`, `new`, `class_attribute` (Module/Class
   and ActiveSupport's extensions to them); anything below a gem ancestor. In
   discourse a quarter of implicit calls sit under an unresolved
   `ActiveRecord::Base`.
3. **The enclosing scope is often a module, and a module is not the
   receiver.** This is the Rails concern pattern:
   `ActiveRecord::Transactions#destroyed?` is defined in
   `ActiveRecord::Persistence`; neither includes the other, and both are mixed
   into `ActiveRecord::Base`. Lexical resolution cannot see that. Measured over
   200 implicit call sites in rails: **67 % resolve inside a class (77/115) and
   30 % inside a module (21/69)**.

So the honest denominator is not "call sites whose receiver needs no
inference" but **"call sites whose receiver type is determinable *and* whose
method is inside the indexed set."** Two of the three causes above are
addressed directly by gem and core indexing (PLAN Phase 3) — the same change
that moves constants from 82 % toward the high 90s. The third wants a rung that
does not exist yet: when a module is included by exactly one class, that class
*is* the receiver, and the index already knows it.

**Index time is dominated by the store write, not by parsing.** `--profile` on
discourse: scan 80 ms, parse 270 ms across 8 workers, **store-write 2 660 ms**.
Roughly 1.5 M row inserts through one SQLite connection at ~575k/s. Parse
speeds up 5× from one worker to four and then plateaus, because it was never
the majority. Anything that wants to make indexing faster should start here —
batched or multi-row inserts, or relaxing durability for the bulk load — and
not with the worker count (DEC-014). The shape differs from rq's, which is why
the same "more workers" advice reproduces there and flattens here: rq overlaps
its single writer with the parse so workers keep feeding it, where trekr
collects every fact and writes at the end.

**The query planner needs statistics, and this is not optional.** Without them
SQLite plans `--refs` as a nested scan of the checkout's files: `--refs new` on
rails took **90 seconds** for 13,684 rows. With `ANALYZE` run, the planner
reverses the join — files drive, a bloom filter rejects — and the same query
takes **66 ms**. `PRAGMA optimize` on close is what keeps it that way; it
re-analyzes only tables that have grown enough to matter, so a no-op reindex is
still 61 ms. Anyone adding a query over these tables should check
`EXPLAIN QUERY PLAN` on a *populated* database — a fixture-sized one hides this
entirely.

| `--refs NAME` on rails | rows | time |
|---|---:|---:|
| `find_each` | 26 | 9 ms |
| `save` | 625 | 13 ms |
| `each` | 1,994 | 34 ms |
| `new` | 13,684 | 89 ms |

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
- Instance, class, and global variables are not recorded (not in PLAN §4's
  Phase 1 fact set).
- Multi-write constant targets (`A, B = 1, 2`) define nothing.
- `refine` is not modeled.
- Orphaned blobs are never collected (DEC-003).
