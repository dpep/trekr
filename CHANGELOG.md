# Changelog

## Unreleased

- **Upgrading drops and rebuilds the index** (store v31): run `trekr --index`
  once per checkout.

- **A delegated method takes any arguments.** ActiveSupport's `delegate`
  generates `def name(...)`, and trekr read it as taking none, so `--refs`
  excluded every untyped call with an argument on arity. On rails, 426 of
  `ActiveRecord::Querying#where`'s 523 exclusions were these, among them
  `Topic.where(…).where(…)`; they are `possible` now.

- **`--dead` gives the same answer whatever else is indexed.** Its pre-filter
  counted a name's calls in every indexed repository, so another checkout's
  calls could hide a candidate (rails' `super-only` went from 72 to 33 when
  discourse was also indexed). It counts only the checkout now, and
  `mentions_by_name` is that checkout's count.

- **A split name's top-level `unresolved` lists only what no declaration
  resolves.** `--ancestors Post --json` on rails listed `Struct` and
  `ActiveRecord::Base` as unresolved, though each resolves in its own
  variant's chain. The bare card (`trekr Post`) called the same name
  `resolved` with the one-entry chain `[Post]`; it now answers `ambiguous`
  with `variants`, as `--ancestors` does.

- **A plain `class User` in a gem of its own is its own class.** In a
  monorepo, a name declared with two superclasses is already split per
  declaration (0.2.1). A superclass-less declaration in a gem directory (one
  holding a `.gemspec`) where neither is declared used to join every variant.
  On rails, activemodel's test `User` had joined both activerecord's model and
  railties' template. It now stands alone, as do plain `Post`, `Person`,
  `Session` and `CallbacksTest` in other gems' tests.

- **A bare constant inside a split class's body is looked up through that
  class's ancestors.** Inside `class Post < ActiveRecord::Base` beside a test
  fake's `Post = Struct.new`, `--def` and the editor found nothing for an
  inherited constant, because the lookup searched the split name, which has no
  ancestry.

- **Paths in `--json`/`--ndjson` are relative to a `root` beside them.**
  Every object with a `path` now carries `root`: the absolute root of the
  checkout holding the file (a gem's root for a definition in a gem), or
  `null` for Ruby core. `path` is relative to it everywhere, where definitions
  and `--def` sites used to be absolute and references relative. `--dead`'s
  `file` is now `path`. Scripts that read an absolute path should join `root`
  and `path`. Text output writes paths in the asked-about checkout relative,
  definitions included (they were `~/…`).

- **`has_many` with `class_name:` no longer types its reader as that class.**
  A collection reader returns a relation, and `firm.clients_of_firm` was
  treated as one Client.

## 0.2.1 — 2026-09-27

- **Upgrading drops and rebuilds the index** (store v29): run `trekr --index`
  once per checkout. The extractor records `super`, classes built by calls,
  literal `define_method`, Forwardable delegators and alias bodies.

- **A name declared with two superclasses is two classes.** A test fake's
  `Post = Struct.new` in one directory and `class Post < ActiveRecord::Base` in
  another used to merge: the file that sorted first gave `Post` its superclass,
  and every declaration's mixins and methods went to that one class. On rails,
  `Post.find_each` beside the models found nothing. Now each superclass is its
  own class, and a call site gets the one declared nearest it. When two are
  equally near, the one in the file named for the class wins (`post.rb`).
  Still tied, `--def` answers `ambiguous` with each declaration's landing and
  `--refs` tiers the site `possible`. `--ancestors` on such a name answers
  `ambiguous` with one chain per declaration under `variants`. An `.rbi`'s
  superclass never splits a name.

- **A `super` is never a reference to its own method.** `--refs` counted a
  method's own `super` as a possible reference to it when the lookup could not
  settle, `--dead` then called the method `super-only` with an empty
  `super_from`, and `--def` offered it as the top candidate. Now every
  `super-only` candidate names who reaches it.

- **`--refs 'Owner#name'` lists nothing for a method that is not there.**
  When the owner does not resolve, or its whole chain has no such method
  (`no_such_method`), text and `--json` both list no sites and exit `1`. They
  used to disagree: the JSON listed every same-name call site and exited `0`.
  A `hint` names `trekr --refs name` for the unnarrowed sites. Scripts that
  read those sites from the `Owner#name` form should ask the bare name.

- **`--symbols` puts `#` only on instance methods** (`.` on class methods), not
  on classes, modules and constants. Text lines from `--refs NAME` no longer
  end in padding.

- **`--dead` asks about the owner a method really has.** A method in `module
  Alpha; module Helpers` was checked against `Helpers`, the name as written,
  so every call that resolved to `Alpha::Helpers` was ruled out and a used
  method could read `unreferenced`. `owner` in `--dead`'s output is now the
  qualified name.

- **Forwardable's `def_delegator` and `def_delegators` define methods**, as
  ActiveSupport's `delegate` does: `def_delegator :@engine, :stop, :halt`
  defines `halt`, a declaration with `defined_via: def_delegator`. The
  `instance_` and `single_` spellings are read too.

- **An alias lands on the body it copied.** `alias_method :old_greet, :greet`
  followed by a new `def greet` left `old_greet` pointing at the alias line,
  with nothing recording which `greet` it meant. When the aliased method is
  written above the alias in the same scope, the alias now records that body
  and its parameters, and `--def`/`--refs` on `old_greet` answer with it
  (`kind: definition`). An alias of an inherited method is still the alias
  line.

- **`--def` on a variable answers the variable.** `x = 5; puts x` asked at the
  second `x` answered `puts`, the nearest call on the line. A local or
  parameter now answers with the writes its value can come from, an `@ivar` or
  `@@cvar` with its writes in that file — `under: variable`, `variable: local
  | parameter | ivar | cvar`, `resolved_via: flow`. A local that shadows a
  method is the local; `name()` is still the method.

- **`t.references :author` in a schema declares only `author_id`.** It also
  declared an `author` reader, which Rails does not make there; when the
  schema was read after the model, `post.author` pointed at the schema line
  instead of `belongs_to :author`.

- **A local receiver is typed from the writes that reach it.** `post =
  Post.first; post.author` in a file where an earlier method wrote `post =
  Cpk::Post.create!` answered `Cpk::Post`, because the first assignment in the
  file won the vote. Now only the writes the read can see vote — not another
  method's, not one a later write replaced — and when they disagree the answer
  is `ambiguous`, listing where the other types land. `x ||= Foo.new` counts
  as a write. `confidence` is still the share of those writes that agree.

- **`define_method(:name) { … }` defines `name`.** Only the looped,
  interpolated form was read, so a plain literal name was missing, and `--def`
  on a call to it said the method came from "a gem, a DSL, or method_missing".
  The block is read as the method's body, so a bare call inside it dispatches
  on the instance. `define_method(:x, instance_method(:y))` is a declaration.
  That reason now says only what was checked: nothing indexed in the
  receiver's ancestors defines the name.

- **`X = Class.new(Base) do … end` is a class.** So are `Struct.new(…)`,
  `Data.define(…)` and `Module.new` assigned to a constant: `--ancestors X`
  names the parent (`Base`, `Struct`, `Data`), the methods in the block belong
  to `X`, and Struct's and Data's members are methods on it (`defined_via:
  Struct.new`). They were constants with no ancestors, and the block's methods
  had no owner. `--symbols` lists such a constant as a `class` or `module`.

- **`super` is followed.** `--def` on a `super` answers the method it runs —
  the next definition after the method's owner, prepends and includes in
  Ruby's order, per including class for a module's method — with
  `resolved_via: super`. It used to answer for some other name on the line,
  confidently. `--refs` counts `super` sites (`receiver: super`), so a method
  reached only from its overrides is no longer unreferenced, and `--dead` gives
  those the new tier `super-only`, with `super_refs` and `super_from` naming
  the overrides. A `super` whose owner the source does not name — in a
  `class_eval` block, `def obj.x` — is residue. A class whose parent is
  computed (`DelegateClass(Base)`) now names it as an unresolved ancestor
  instead of inheriting `Object`.

- **Breaking for scripts: errors exit with their own codes, not `2`.** A bad
  flag or value, or an input trekr cannot parse, exits `64`. A missing file or
  a directory outside any checkout exits `66`, git that cannot run exits `69`,
  a bug exits `70`, and a store or file I/O failure exits `74`. `2` now means
  only `not_indexed`: run the `hint`, then ask again. A script that branched
  on `2` for "failed" should test for `>= 64`. Plain `trekr` with nothing to do
  exits `64`, where it used to exit `1`. The README and `trekr --help` have
  the table.
- **Errors are JSON under `--json`/`--ndjson`.** A failure prints one
  `{"error", "kind", "code"}` object on stdout, where before it printed text
  on stderr and nothing on stdout. This covers clap's parse errors too, even
  when `-j` comes before the bad flag. The message is still on stderr, and in
  text mode stdout stays empty. `kind` is `usage`, `not_found`, `not_a_repo`,
  `git`, `internal`, `database` or `io`, the same names `--usage` now counts
  errors under. An error reading a file now names the file.
- **`--refs 'Owner#method'` and `trekr Owner#method` say when the method does
  not exist.** When the owner resolved but nothing in its ancestors defines
  the method, the bare form said `status: resolved` with an empty
  `definition`, and `--refs` printed nothing to say why. Both now say
  `status: no_such_method` with a `reason` ("Widget has no method nope in its
  ancestors"), and text mode prints the same line. If an ancestor is not
  indexed, it answers `residue` instead and names that ancestor.
  `--refs --json` gains `status` and, when there is one, `reason`.

## 0.2.0 — 2026-09-27

- **Commands no longer stall while an index runs.** `--refs`, `--ancestors`,
  `--status` and the rest printed their answer and then sat up to 5 s before
  exiting, and `--def` waited before answering, whenever another trekr was
  writing the index (an editor's background index, a `--index` in another
  terminal). They now exit as soon as they answer. A `--def` whose file changed
  but could not be refreshed says so: `index.busy` names the file, answered
  from its indexed version. In the editor, a file saved during a background
  index is now refreshed once the index finishes, where before the save was
  silently dropped until the next one.

- **Queries stop rebuilding the namespace, and LSP sessions share it.** The
  assembled tree is written once per checkout beside the database
  (`trekr.trees/`, next to `trekr.db`) and mapped read-only by every CLI query
  and LSP session after that. On a 336k-file repo `--def` goes from ~3 s and
  1.1 GB of memory to ~0.05 s and 34 MB, an LSP session's first answer from
  ~2.9 s to under a second, and three editor sessions hold 2.4 GB instead of
  5.4 GB; on discourse, `--def` 0.22 → 0.02 s. The first query after an
  `--index` you run pays the build once, plus a write the size of the tree
  (120 MB at that scale); the index the LSP starts in the background builds it
  itself. The directory is a cache: deleting it costs one rebuild per
  checkout.
- **An editor session notices a bundle moving to another gem version.** It
  kept answering from the old version's classes until a file in the checkout
  itself changed; now any reindexed gem it uses reloads the tree.
- **`--gc` also removes tree snapshots no checkout's index names any more** —
  one left by a checkout that was collected, or whose index moved with no
  query since. `--json` reports them as `snapshots: {files, bytes}`, and they
  count toward the exit code.
- **Go to Definition on a variable.** In `--lsp`, Cmd-click a local or
  parameter and land on the assignments its value can come from — both
  branches of an `if`, the write at the bottom of a loop, a block or method
  parameter, a pattern or `rescue => e` binding. Cmd-click an `@ivar` and land
  on where its class sets it: `@x =`, `||=`, `attr_writer`/`attr_accessor`, or
  `instance_variable_set(:@x, …)`, in the class's reopenings and ancestors,
  `initialize` first. An ivar whose object could be one of several classes (a
  module's, set by whatever includes it) answers nothing rather than a guess.
  `@@class` variables work the same way; `$globals` are not answered. Find All
  References lists every read and write, hover says where it was set
  (``local `total` · assigned at line 12``), and the editor highlights the
  other mentions in the file — a new `documentHighlight` capability, with a
  `documentHighlight` entry in the VS Code extension's `trekr.features`. Read
  from the open buffer, unsaved edits included; no re-index.
- **The index `--lsp` starts in the background runs at lower priority**:
  nice +10 and a low disk-I/O tier, so it yields to the editor and to
  anything else on the machine. A `trekr --index` you run yourself is
  unchanged. On a busy machine the background index can take longer — a
  cold discourse index ~7 s → ~8–11 s. The LSP log records the child's
  priority as an `index_priority` event.
- **`trekr --usage` now covers the command line as well as the editor.** Every
  command (`--def`, `--refs`, `--dead`, the bare `Widget#save` form, …) and
  every `--lsp` operation is counted by day, caller (`claude-code`, `human`,
  `ci`, an editor's name, …), outcome (hit, uncertain, empty, not indexed,
  error) and a coarse latency bucket, and `--usage` shows which get used, by
  whom, how often they come back empty, and how slow. `--days N` narrows it;
  `--json`/`--ndjson` emit the daily rows. Counts only: no queries, paths or
  repository names are kept. They live in `trekr.usage.db` beside the index,
  so a reindex or `--gc` never clears them, and rows older than 90 days are
  dropped. `TREKR_USAGE=off` turns counting off. **The old log-based report is
  gone**: `--usage` starts counting from this version, and `lsp.log` still
  holds what came before. A hot-reloaded session's first request is now
  reported as a session opener, as it always should have been.
- **`--refs NAME` on a very common name is up to 2× faster in a large repo.**
  Each call site's tiering was matched to its row by scanning all of them;
  `--refs each` on a 336k-file repo 17.9 → 9.9 s.
- **`--refs Owner#method` on a very common name no longer takes the slow plan.**
  The query that lists the files calling a name is pinned to the name's
  index, as the LSP's already is: `--refs Array#to` on a 336k-file repo
  ~40 s → ~23 s. The README now says what a very large repo needs: git's
  untracked cache and fsmonitor, and twice the store's size in free disk for
  the first index.
- **A first `--index` of a very large repo is up to 4× faster.** When an
  index will more than double the store, the fact tables' indexes are rebuilt
  by sorting after the rows are in, instead of updated row by row: a 336k-file
  monorepo 16 min → 4 min, discourse 7.5 → 6.3 s. Same index. A cold index
  still needs free disk of about twice the store's size while it runs.
- **A no-op `--index` in a very large repo is faster**: it no longer loads
  every known blob to find it has nothing to parse — 0.50 → 0.42 s on a
  336k-file monorepo. **In a repo that size, turn on git's fsmonitor**
  (`git config core.fsmonitor true`): the scan's `git status` goes from over
  a second to under 0.1 s.
- **Find References is bounded, and says when it cut.** In `--lsp`, an
  answer keeps at most 1000 references — set `referenceLimit` in
  `initializationOptions` to change it — with confirmed callers (receiver
  resolved) kept ahead of possible ones. When anything was left out the
  editor shows one message, such as "showing 1,000 of 5,450 references to
  reload, confirmed callers first. For all of them: `trekr --refs
  'Topic#reload'`". A name whose receiver never resolved (`to`, `call`) stops
  reading files once it has the limit, rather than scanning the whole
  checkout. A client that sends a `partialResultToken` gets the references
  streamed as `$/progress` batches, the definition first. Before, a common
  name returned every call site: 81,505 for `to` on discourse, and 2.4
  million after 65 s on a monorepo thirty times its size. That answer now
  takes 34 ms.
- **Go to Definition on a `require` string opens the file.** In `--lsp`,
  a `require`, `require_relative`, `load` or `autoload` string resolves to the
  file it loads: relative to the requiring file, or along the load path —
  the checkout's `lib/`, `spec/` and `test/`, its path gems, each bundled
  gem's `lib/`, then the Ruby standard library the bundle was installed
  beside. Where you click in the string does not matter. When several files
  match (`json` is both a gem and stdlib), all are offered, first on the path
  first. A native extension, a gem not on disk, or a path built at runtime
  answers nothing rather than a nearby file; `File.expand_path("x", __dir__)`
  and the other literal-in-disguise idioms are followed. Hover on the string
  names the file and its gem, and strings with exactly one file behind them
  are underlined as links (`textDocument/documentLink`). The VS Code
  extension's `trekr.features` gains `documentLink`; if you have set that
  list yourself, add it there to get the links.
- **CLI queries skip freeing the namespace on exit**: 13–30 ms off `--def`,
  `--refs` and `--ancestors` on discourse, and ~0.2 s at thirty times its
  size.
- **Hover shows what a definition is, not how trekr found it.** The hover
  now shows the signature as written (`def Widget#resize(width, height =
  nil)`, `class Foo < Bar`, `LIMIT = 10`). Below it comes the first paragraph
  of the doc comment above the definition, YARD's `@return` and `@deprecated`,
  and a linked `Defined in path:line`, naming the gem for gem code. The
  `status · confidence · via` line is gone. When the answer is a guess, the
  hover says so in words, like `receiver type unknown — 3 possible
  definitions`; a confident answer carries no caveat. Choosing a completion
  item shows the same doc and signature. Docs are read from the definition's
  file when you hover, so nothing is re-indexed and the store does not grow.
  `--json` output is unchanged.
- **`--lsp` switches to a new trekr in place.** After `brew upgrade`, a
  reinstall or a `cargo build`, a running server hands its session to the new
  binary within a couple of seconds, on the same connection. Open files and
  unsaved edits carry over, and the editor restarts nothing. Before, the server
  exited and relied on the editor to restart it. If the new binary cannot run,
  the old server keeps serving and logs `reload_failed`. A new binary too old to
  resume a session still exits, so the editor restarts it. This takes effect
  from the *next* upgrade: servers already running an earlier version exit as
  before. The LSP log records each `reload` and `resume`.
- **`trekr --gc` reclaims old gem versions and deleted worktrees.** A gem
  version no indexed project's bundle names any more, and a checkout whose
  directory is gone, used to be kept forever. `--gc` removes those an index has
  not seen for `--older-than` (default `7d`), plus the parsed facts only they
  held — never ones another checkout shares. `--dry-run` shows what it would
  remove and the space; `--vacuum` also shrinks the file. A collected gem is
  simply re-read by the next `--index` whose lockfile names it. On one
  month-old store: 69 of 684 checkouts, 6 % of facts, 446 → 399 MB with
  `--vacuum`. **The store is rebuilt once on upgrade** (schema change): the
  first `--index` of each project is a cold one.
- **`--lsp` holds about half the private memory per server, and queries are
  2–4 % faster.** The store is read through `mmap`, so pages come from the
  shared page cache rather than being copied into each connection: an LSP
  session on discourse keeps 90 MB live instead of 168 MB. RSS reads *higher*
  now, because it counts the shared mapped pages.
- **Re-indexing after an edit is ~20 % faster in a large checkout.** Only the
  file-map rows that changed are written, instead of the whole map: discourse
  after a one-file edit 188 → 154 ms.
- **`--index` uses a third of the memory on a first index.** Files are written
  as they are parsed instead of all parsed first: discourse with its gems
  peaks at 160 MB rather than 440 MB, and finishes ~5 % sooner. In
  `--profile`, `parse` now overlaps `store-write`, and the throughput line is
  per worker.
- **A cold `--index` with gems is about 0.4 s faster.** The set of blobs
  already on this machine is read once per index, not once per gem: discourse
  with its 297 gems 7.3 s → 6.8 s. Same index.
- **`--lsp` uses about 40 % less memory once completion is ready.** Listing
  a checkout's members for completion no longer loads every method into the
  session's tree: discourse and rails in one session went from 630 MB to
  360 MB, and a completion asked for before the listing was ready waits
  0.37 s instead of 0.59 s.
- **`--lsp` completion gives the same list for the same position.** A list
  cut at its 300-item cap kept whichever members a hash map yielded first,
  which changed from one server process to the next; it now keeps the names
  that sort first, which are the ones the editor shows first anyway.
- **A cold `--index` that brings gems is about 40 % faster.** A bundle's gems
  are written as one transaction instead of one each: discourse with its 297
  gems went from 12.7 s to 7.6 s, rails from 2.7 s to 1.6 s. The index is
  identical. An index interrupted while writing gems now keeps none of them,
  rather than the ones it had finished — the checkout's own files are still
  committed first.
- **Re-indexing after an edit no longer pays for a full `ANALYZE`.** Statistics
  are regathered once the store has grown a tenth past the ones it has, instead
  of after every index that parsed anything. Re-indexing discourse after
  editing one file went from 0.64 s to 0.26 s.
- **A no-op `--index` is up to twice as fast in a repo with git's untracked
  cache on.** The scan asks `git status` for changed and untracked files, which
  uses the cache, instead of `git ls-files -o`, which walks the whole worktree
  every time: discourse 165 → 86 ms, rails 64 → 41 ms. **In a large repo, turn
  the cache on** — `git config core.untrackedCache true` (or `feature.manyFiles
  true`) — to get this; without it the scan costs what it did.
- **`--dead` is 2–6× faster.** Each file is parsed once per run rather than once
  per candidate that it calls, and the "is this name plainly used?" pre-filter
  stops counting at the threshold instead of counting every call of `id`:
  rails' `activerecord/lib/active_record` 3.5 s → 0.56 s, discourse
  `app/models` 3.5 s → 1.3 s. Same candidates; peak memory rises by the parsed
  files held (+50–80 MB on those runs).
- **`--refs Owner#method` is 10–30 % faster on a large app**: the files calling
  the name are parsed in parallel. discourse `Topic#title` 0.56 s → 0.39 s.
- **Every resolved query builds its namespace about 20 % faster**, which is
  most of a CLI `--def`, `--ancestors` or `--refs` and the first answer of an
  `--lsp` session: discourse's tree 215 → 167 ms, rails' 57 → 46 ms.
- **`--lsp` no longer stalls a request for half a second while it prepares
  completion.** The member listing is built on a worker thread instead of the
  one answering requests, and only a completion waits for it. After the first
  answer, the next request on discourse took 0.5 s; it now takes 13–24 ms.
- **`--lsp` speaks the protocol's error vocabulary, and honours cancellation.**
  An unsupported method answers `MethodNotFound` (it answered `null`, which a
  client reads as "nothing here") and a malformed request `InvalidParams` (it
  answered `InternalError`). `$/cancelRequest` is now read ahead of the queue:
  a request withdrawn before its turn is answered `RequestCancelled` without
  being worked, and a long `references` / `incomingCalls` stops mid-scan.
  `exit` stops the server with or without a preceding `shutdown`.
- **`--lsp` no longer serves a file as it was the first time it was asked
  about.** A file the editor has not opened is re-read when it changes on disk
  — an agent that edited a file and then asked about it got the old answer.
  Closing a file clears its syntax diagnostics, published diagnostics carry the
  document version, and a ranged `didChange` is applied in place rather than
  taken for the whole document.
- **`references` in `--lsp` answers for classes, modules and constants.** It
  searched only method call sites, so asking on `Widget` returned nothing;
  it now returns every written constant that Ruby's lookup resolves to the
  same name (`Widget`, `::Widget`, `Shop::Widget` from outside `Shop`), and
  none that resolve elsewhere. `includeDeclaration` is honoured — the
  definition comes first — unsaved edits in open files are counted, and
  ranges span the name in correct UTF-16 columns. Large answers are about 2×
  faster: files are parsed in parallel, and positions are converted with a
  per-file line index instead of a scan from the top of the file per hit.
- **Call hierarchy walks.** `incomingCalls` returns one item per calling
  method with each call as a range, and that item can be expanded again (it
  was the call site, which answered nothing); `prepareCallHierarchy` on a call
  prepares the method it calls, and `outgoingCalls` points each callee at its
  definition. **`documentSymbol` is nested** — methods inside their class,
  each spanning its body — so VS Code's outline, breadcrumbs and sticky scroll
  work; singleton methods are shown as `self.name`.
- **`--lsp` keeps the index current while you edit** (DEC-039). A save
  refreshes that file in the index, so a method added in one file is found
  from another without running `trekr --index`. Changed files reported by the
  editor's watcher are refreshed the same way; a burst of them, or a deletion,
  triggers a background `trekr --index`. **An unindexed checkout is indexed in
  the background** — the workspace root when it has a `Gemfile`, or any
  checkout a question lands in — with `$/progress` shown by clients that
  support it; until it finishes, `hover` says answers are partial. Turn
  background indexing off with the initialization option `{"index": false}`.
  The root's tree is built while the server is idle, so the first question no
  longer pays for it.
- **`--lsp` answers `textDocument/completion`** (DEC-040), receiver-aware and
  ranked. After `recv.` it lists what the receiver ladder says `recv` is — its
  own methods, then each ancestor's in lookup order, private ones only on
  `self`; after `Scope::`, that namespace's constants; on a bare word, locals
  and parameters, then the enclosing class's methods, then constants in
  lexical scope. An untyped receiver gets at most 20 same-prefix names marked
  "receiver type unknown", never the whole index. Measured p90: 3.7 ms on
  rails and 15 ms on discourse.
- **`--lsp` answers in the client's spelling of its workspace path.** Opened
  through a symlink (or macOS's `/var`), locations came back under the
  canonical path, and the editor opened the same file a second time.
- **A VS Code extension**, in `editors/vscode/`: launches `trekr --lsp` for
  Ruby files, with `trekr.path`, `trekr.features` (switch individual features
  off) and `trekr.index` settings. Its README covers replacing Ruby LSP and
  Sorbet and what that gives up.

## 0.1.5 — 2026-08-26

- **`trekr --dead <path>…` finds candidates for deletion or inlining.** Scope is
  the argument — files or directories — and the evidence is the **whole index**,
  because a method used once from outside the scope is not a candidate.

  Three tiers, each carrying what was found: **`unreferenced`** (no reference of
  any kind, anywhere), **`convention-only`** (reached solely by a symbol handed
  to a macro), and **`single-caller`** (exactly one written reference — the
  inlining candidate). Nothing is ever reported as `dead`, and confidence is
  **graded per candidate**: a file using `send`, `public_send`,
  `method_missing`, `define_method` or `const_get` lowers it and names which one
  it saw. Macro- and schema-generated definitions are not reported at all — an
  unreferenced column is a fact about the database.

  `--json` is first-class, per-symbol with reasons attached, so a deletion
  workflow can consume it as an evidence layer rather than a verdict.

  **Validated against 12 months of discourse's git history, and it makes no
  precision claim**: `unreferenced` candidates were deleted by humans 19.8 % of
  the time against a **19.0 % base rate for any method in the same scope** — no
  measurable lift. Read the tiers as *what was looked for and not found*, which
  is what they say. `convention-only` is the strong one, inversely: those
  methods are deleted 3.3 % of the time, surviving six times more often than
  average, because they are genuinely used by a macro.


- **A symbol handed to a macro counts as a reference.** `after_create :ensure_thing`
  invokes `ensure_thing`, and nothing in the file writes it as a call — so every
  callback-, validation- and route-registered method looked unused. `--refs` now
  finds those sites, tiered **possible** with their own reason (`named by a
  symbol handed to a macro — invoked by name, receiver unknown`), never
  confirmed: a symbol names the method and says nothing about the receiver.

  Recorded for a symbol in argument position of **any** call rather than a
  curated macro list, because an app's own DSL is unknowable — discourse's
  `step`/`policy`/`model` alone is a thousand sites. Measured on discourse:
  method names with no reference of any kind fall **9,054 → 7,105 (−21.5 %)**.

- **Fixed: `--def` on an `enum` line answered the accessor, not `enum`.** The
  mapping accessor added in 0.1.4 sat at the macro call's own offset, and a
  position lookup prefers definitions — so asking what `enum` is answered
  `subjects`. It is positioned at its attribute now, like every other
  macro-generated definition.

  **Existing databases reindex once** — the extractor's output changed.


## 0.1.4 — 2026-08-26

- **`trekr <input>` takes one argument and dispatches on its shape.**

  ```sh
  trekr 'Widget#save'              # where it is, and who can reach it
  trekr Widget                     # where it is, and what it inherits
  trekr app/models/user.rb:42:11   # what is at that position
  trekr app/models/user.rb:42      # same, column optional
  ```

  A method or constant gets a **card**, which is why this is not an alias for a
  flag: two commands' worth of answer in one. A method card carries the
  definition, its `kind`, and the confirmed/possible/excluded tier counts; a
  constant card carries the declaration sites and what it inherits. `--json`
  on every shape.

  **The flags are unchanged and remain the explicit form** — scripts should use
  them rather than depend on shape inference. A shape trekr cannot name is
  refused with the shapes spelled out, never guessed at.


- **`enum :status` now defines `Model.statuses`.** The macro model generated the
  members' predicates and scopes but never the attribute's own class method
  holding the mapping — so `SidebarUrl.segments` resolved to nothing.

  It also survives `prefix:`/`suffix:`, which previously refused the whole
  `enum`. Those options rename the *member* methods, and spelling those wrongly
  is worse than not offering them (so they are still refused); the mapping
  accessor is not renamed by either, and refusing it too was over-broad.

  **Existing databases reindex once** — the extractor's output changed.


## 0.1.3 — 2026-08-25

- **A query keeps itself honest about freshness.** `--def` probes git in O(1) —
  one stat of `.git/index` — and when the checkout has moved it **re-reads the
  file you asked about** before answering, so a definition that shifted lines is
  found at its new line without any explicit reindex. The answer carries
  `index: {stale, refreshed, hint}`, and the text surface says it in a line.

  **Bounded on purpose**: one file, whatever the repo size. A full scan is
  145 ms on discourse and ~6 s on a 10M-line monorepo, and neither can sit on a
  query path — so the rest of the index is left alone and *disclosed* as
  possibly lagging rather than silently trusted. `trekr --index` remains the
  only thing that declares a whole checkout fresh.

  **What the probe cannot see**, stated rather than left to be discovered: an
  edit that nothing has told git about does not move `.git/index`. Both limits
  are pinned by tests.

  **Existing databases reindex once** — the schema gained a column.


- **A no-op `--index` no longer rewrites the file map.** Every index deleted and
  re-inserted one row per file whether or not anything had changed — O(files) of
  pure cost on a repeat run. The checkout now stores a `map_key` folded over
  (path, blob oid); an identical key means an identical map and the rewrite is
  skipped.

  Measured on discourse (11,301 files), steady state: the `store-write` phase
  falls from **42 ms to 1 ms**, and a no-op index from **185 ms to 145 ms**. On
  rails, store-write 78 ms → 0 ms. What is left of a no-op is the scan, ~94 % of
  which is git's untracked-file discovery.

  **Existing databases reindex once** — the schema gained a column.

## 0.1.2 — 2026-08-25

- **An unindexed repo now answers `not_indexed`, not residue.** A query into a
  checkout the store has never seen names the repo root, gives the `trekr
  --index` command, and exits 2 — instead of `status: residue` with "no indexed
  constant", which read as a finding about the code when it was a setup step.
  **Anything scripted against residue-as-missing-index must read the new
  status.**

- **`--def` snaps to the nearest name on the line, and says so.** An off-by-one
  column answers for the nearest identifier with `snapped_to: {name, col,
  alternatives}` in JSON and a stderr note in text; `--def FILE:LINE` (no
  column) now works. Exact positions answer exactly as before, and the LSP
  server keeps exact-position semantics — editors send real columns.

- **Human output shows `$HOME` as `~`.** Every text surface — results,
  `--explain`, `--status`, `--usage`, errors. `--json`/`--ndjson` keep absolute
  paths, and LSP URIs are untouched.

## 0.1.1 — 2026-08-25

- **`--serve` is now `--lsp`, with no alias.** The flag says what it does. **You
  must edit any hand-written editor or MCP config** that spawns `trekr --serve`;
  plugin users get it with the plugin update. Pre-1.0 this project takes the
  clean break over a compatibility shim, and the changelog line is the whole
  migration.

  The request log moves with it: `~/.local/share/trekr/serve.log` →
  `lsp.log`, which `--usage` reads. To keep your history, `mv` it — nothing else
  refers to the old name.

- **The skill checks for the binary before it tries to use it.** First report
  from a second machine: the plugin was installed, `/trekr` was run, and the
  skill neither noticed `trekr` was missing nor helped install it — while the
  plugin's LSP server, which is `trekr --serve`, failed silently for the same
  reason. The skill now opens with the install (`brew install
  dpep/tools/trekr`, or `cargo install trekr` without Homebrew — verified
  against crates.io), says the LSP comes up at the next session start, points at
  the per-repo `trekr --index`, and separates "no results" from "broken" with
  `trekr --status`.

  Skill only; the binary is unchanged, so there is no crate or formula release
  in this. It ships by bumping the **plugin** version in the myclaude
  marketplace, because `claude plugin update` compares versions and not content
  — which is exactly why the gap reached a second machine at all.

- **A Sorbet stub answers `kind: declaration`, with `defined_via: rbi`.** An
  `.rbi` `def` is bodiless by construction — an ordinary definition by every
  syntactic test, and a description of a method that runs somewhere else.
  `Site::is_rbi` has said exactly that since DEC-019 ("an `.rbi` is a
  declaration, never an implementation"); `kind` shipped without asking it.

  Rare by design rather than by luck: DEC-019 makes real source win the whole
  ancestor chain before a stub wins any of it, so a stub only answers when it
  is all there is. Measured on widget_shop, the one corpus that commits
  `sorbet/rbi/`: **0 of 63 app sites and 9 of 400 gem sites** — nine answers
  that used to claim the body was there. No canonical figure moves; discourse
  is byte-identical.

## 0.1.0 — 2026-08-25

First public release.

trekr answers two questions about Ruby that grep cannot: **which method does
this call site actually run**, and **which call sites can actually reach this
method**.

- **`--def FILE:LINE:COL`** resolves a position by walking Ruby's own
  constant-lookup ladder — enclosing lexical scopes, the innermost scope's
  ancestors, then the top level. 82 % of rails constant references resolve
  (78 % on discourse), and every answer carries `status`, `confidence`,
  `resolved_via`, and whether the location is the code or the macro that
  declared it.
- **`--refs 'Owner#method'`** tiers call sites by whether the receiver can
  actually reach the method: **confirmed** (the receiver's type resolves and
  lookup lands here), **possible** (untyped receiver, nothing rules it out,
  ranked and never dropped), **excluded** (counted, not listed, auditable with
  `--include-excluded`). Across twelve heavy-collision names on rails —
  25,297 same-name call sites — 32 % confirmed, 43 % possible, 24 % excluded.
- **`--ancestors`**, **`--symbols`**, **`--status`**, **`--index`**, `--drop`,
  `--usage`, and `--explain`, all with `--json`/`--ndjson` and meaningful exit
  codes, because the intended caller is an agent.
- **`--serve`** speaks LSP over stdio: goToDefinition, findReferences,
  documentSymbol, workspaceSymbol, hover, goToImplementation, call hierarchy,
  and Prism syntax diagnostics.
- **`--completions <shell>`** prints a shell completion script, generated from
  the parser so it cannot drift from the flags.

A server whose binary has been replaced retires by exiting rather than by
closing its own stdin. Closing the descriptor turns the reader's blocking read
into EOF on macOS but not on Linux, where the process would log its retirement
and then hang holding the stale build.

Facts are keyed by git blob OID, so every worktree of a repo shares one index
and a reindex with nothing changed parses nothing: 1.5 s cold on rails, 61 ms
for a no-op reindex, ~0.2 s and zero parses for a second worktree. Ruby core
and the checkout's gems are indexed; gems are shared across every project
resolving the same `(name, version)`. No Ruby toolchain, no `bundle install`,
no bootable app.

## Pre-release development

What follows is the development log from before the first release — written as
deltas against the working tree of the day, not against any published version.
Kept because the reasoning is worth having; skip it if you want the shipped
behavior, which is above.

- **`define_model_callbacks` is modelled**, so `before_save`, `after_destroy`
  and the rest of ActiveRecord's model callbacks resolve. The macro sits in an
  `included do` block, which is `class_eval`'d into the includer, so its
  class-level methods are routed to the concern's `ClassMethods` — where
  Concern already puts an includer's class methods — and the module is emitted
  when the concern does not declare one itself. `only:` is honoured; a computed
  `only:` generates nothing rather than inventing the other two.

  Built and turned down in session 29 because the answers had to be scored as
  errors; they are now disclosed as declarations and score as such. 114
  discourse app sites, **112 answered, 0 confidently wrong**.

- **An answer says which kind of location it handed back.** `--def --json` and
  `--explain` carry `kind: definition | declaration`, and `defined_via` names
  the macro when it is a declaration (`belongs_to`, `enum`, `schema`,
  `delegate`, an alias, a bare `private :foo`). The store has always known this
  — a `def` row's `via` records what made it — and the answer never said, so a
  caller could not tell `belongs_to :supplier`, the line a reader wants, from
  the line Ruby runs.

  The test is **"is the body at this location"**, not "was a macro involved": a
  literal `def` is a definition, and so are `define_method`'s block, which *is*
  the body, and `module_function`'s copy, which points at the `def` it copied.
  Residue candidates carry their own `kind` too.

  On the LSP side it lives in **hover**: `textDocument/definition` is a bare
  list of locations and has nowhere to put it.

- **A mixin written inside a `def` is no longer an ancestry edge of the scope
  that lexically contains it.** It runs when the method runs, against whatever
  `self` is then, so recording it lexically does not merely miss an edge — it
  invents one. Rails writes `include ActiveModel::Validations` inside
  `has_secure_password`, in a `ClassMethods` body, which put that module's
  `alias_method :validate, :valid?` into the class-level lookup chain of every
  ActiveRecord model: a class-body `validate :thing` resolved, confidently, to
  the instance alias instead of `ClassMethods#validate`. It stays an ordinary
  call site, because `include` really is `Module#include`.

- **`class_methods do … end` opens the concern's `ClassMethods` module.**
  `ActiveSupport::Concern` creates `M::ClassMethods` from the block form and the
  nested-module form alike, and extends it into every includer. Leaving the
  block unmodelled put its methods on the concern as *instance* methods, where a
  class-body call cannot reach them — and a mixin written inside it became an
  instance-side edge of the concern rather than a class-side one of every
  includer. On discourse that single shape is **28 % of all declined app sites**;
  `correct` on real app code goes **43.4 % → 59.2 %** and residue-with-the-truth
  -offered **41.6 % → 25.3 %**.

- **An `includer`-rung answer that picked among competitors is `ambiguous`.**
  A call written inside a module is answered by asking the classes that mix it
  in; when two of them define the name in different places, the rung still
  reported `resolved` — DEC-027's rule applied to the receiver-name rung and
  never to this one. It now says `ambiguous` and lists the definitions it beat.
  Measured: discourse app confidently wrong **0.6 % → 0.2 %**, gem floor
  **4.0 % → 3.3 %**, with `correct` and `found` byte-identical on both.

- **A method whose name is computed from a literal array is extracted.**
  `[:before, :after, :around].each { |c| define_method "#{c}_action" … }` is how
  actionpack writes `before_action`, and how ActiveRecord writes its model
  callbacks — nothing that reads only the `def` keyword can see them. Scoped
  tightly: a literal array, `each`, one block parameter, and a name whose only
  interpolation is a bare read of that parameter. A constant array, a second
  interpolation, or `#{n.to_s}` all generate nothing, because a name
  half-guessed is worse than a name not offered — the lookup would find it and
  stop. The definition's location is the `define_method` call, which is where
  the method is written.

- **Existing databases reindex once**: the extractor's output changed.

- `--index` gathers full statistics (`ANALYZE`) after a run that actually read
  something. `PRAGMA optimize` on close only re-analyses a table whose size has
  moved since the last analysis, which never fired across hundreds of checkouts
  accumulated a few at a time — so the planner was working from statistics
  thirteen rows old. Reported as an `analyze` phase under `--profile`.

- **Fixed: retirement detected a replaced binary and then never left.** Breaking
  out of the request loop is not enough — `IoThreads::join` waits for the reader
  thread, which is parked in a blocking read on stdin, and an editor holds stdin
  open for as long as it is running. The server sat there having logged
  `retire`, still holding the old build: the exact symptom retirement was
  written to remove, now with a log line claiming it had worked. It closes the
  descriptor before leaving, which turns that read into EOF.

- `--def --context CHECKOUT` answers a position as if asked from that checkout.
  Only meaningful inside a **gem**, which is otherwise answered from whichever
  app most recently indexed it — a pick that follows your work, which is right
  for a person and wrong for a measurement (DEC-029).

- `--usage` reports the **first request of a session apart from the rest**. That
  request pays for a cold page cache and a tree build; blending it into the
  median made the headline a measure of the disk rather than of trekr. On the
  real log it moves `definition` from a reported **415 ms median to 88 ms**,
  with the five session-openers shown separately at 451 ms.

- Residue candidates are ordered by **directory affinity**: a definition that
  shares directories with the call site ranks above one across the tree — the
  "same file" signal, graded instead of binary. Measured against the candidate
  pool as it exists after gem context: truth ranked first **49.6 % → 52.8 %**,
  MRR 0.648 → 0.666. `correct` and `confidently wrong` are unchanged, which is
  what a ranking feature is allowed to do. `TREKR_RANK_OFF=affinity` switches it
  off for re-measurement.

- The namespace fixpoint revisits only the declarations whose placement can
  still change. A declaration written with plain names is placed by string
  arithmetic alone, so round one settles it forever; only a **compact path**
  (`class A::B`), whose prefix goes through constant lookup, can move. On
  discourse that is 9,100 of 69,305 declarations, and the fixpoint falls from
  **97 ms to 46 ms** (rails 22 → 9 ms). The assembled namespace is
  byte-identical on rails, discourse and widget_shop.

- **A position inside a gem is answered from an app that resolves it.** A gem
  is indexed as a checkout of its own, so on its own it is a tree of one gem
  plus Ruby core — and every method it gets from a sibling gem was unreachable
  by construction (DEC-029). `--index` now records which gems a bundle
  resolves, and a query inside gem source picks the **most recently indexed**
  app that has it. The answer carries `context` naming the checkout that
  answered, and `--explain` prints it; a gem no indexed app resolves keeps the
  one-gem tree and says so by naming itself.
- A tree reads its gem roots from the store instead of re-locating them on
  disk. Locating gems means a lockfile and ~200 stats against `GEM_HOME` and
  friends, and doing it per query made the tree depend on the environment the
  query ran in — a query with a different `GEM_HOME` than the index silently
  lost every gem. Gem roots are also canonicalized at index time now, like
  every other checkout root.
- **Existing databases reindex once**: the schema gained the gem-ownership map.

- `owner` now names where Ruby's lookup actually landed. A method reached
  through a `self.table_name` override reported the *carrier* class the
  convention invents (`LegacyPost`) — a name no code declares and no agent can
  look up. It reports the model. For every other method the two were already
  the same.

- `--serve` retires itself when its binary is replaced. After each request it
  checks whether the executable on disk is newer than the one it is running;
  if so it finishes the answer, logs a `retire` event and exits cleanly, so the
  editor spawns the new build. A server answering with a stale binary until
  somebody remembers to kill it is silent staleness, which is the bug class
  this engine hunts everywhere else. `--usage` counts the retirements.
- The serve log's `start` event records which binary it is running.

- **Methods are loaded on demand, by name.** A tree no longer fetches and
  indexes every method in the checkout and its gems — 84,052 of them on rails,
  which was 76 % of the build. It loads the names a query actually asks about.
  Measured: rails tree build **310 ms → 73 ms**, discourse **643 → 259**; a
  rails `--def` **0.31 s → 0.09 s**, `--refs` **~0.40 s → 0.13 s**, and the LSP
  first query **508 ms → 85 ms** (discourse 975 → 272). Accuracy is unchanged —
  the gold set is identical before and after.

- `tests/testbed/` — ten accumulated corner cases as drop-in fixtures with one
  iterating harness, so adding the next costs no Rust: the ancestor cycle that
  killed the process, a Sorbet stub shadowing real source, resolved-vs-ambiguous
  receiver-name pairs, the same-file path boundary, macro-as-call, the delegate
  prefix, finder typing, and the exclusion count `--refs` exists for.

- An `ambiguous` answer now lists the definitions it beat. Competitors are what
  made it ambiguous, so showing them is the disclosure, not a hedge — and only
  residue used to carry a candidate list. `--explain` renders them with the
  reason; this checkout's own code ranks before a dependency's.

- `--profile` now reports where a **query's** time went, not just an index's:
  the tree build's phases, on stderr. `TREKR_PROFILE=1` does the same for a
  process that cannot pass the flag. This is what showed that methods are 76 %
  of a rails tree build.

- `--def --explain` renders the disclosure `--json` has always carried: the
  rung that resolved the receiver, the confidence and what graded it, the
  ancestors that could not be seen, and the ranked candidates behind a residue
  with the reason each ranked where it did. Promised in PLAN and CLAUDE.md
  since the start; only the rendering was missing. Every line restates a field
  of the answer, so the two surfaces cannot drift.

- **`status` gains `ambiguous`**, the third value the docs always promised. A
  `receiver_name` answer says `resolved` only when the name is the whole story;
  when other classes define the same method it says `ambiguous` (DEC-027).
  `@account.local?` was `resolved` at 0.03 confidence and is now `ambiguous`;
  `@widget.supplier_region` stays `resolved · 0.5`. Exit codes treat
  `ambiguous` as a match.
- Confidence is rounded where it is built, to the precision two counts carry —
  `0.03`, not `0.03225806451612903`.

- `goToImplementation` on a **method** now answers with its overrides. It only
  ever understood class and module names, so standing on an abstract method
  returned nothing. Asked the way Ruby answers it — for every type carrying the
  owner, whose definition actually wins — so it finds an override in a *sibling
  module* (`SQLite3::DatabaseStatements` beside the abstract one), which a
  subclass search misses. `write_query?` on Rails' abstract adapter now returns
  the SQLite3, PostgreSQL and MySQL definitions.
- `callHierarchy/incomingCalls` names each caller by the method it sits in
  (`Job#run`, `Job.sweep`) instead of by the callee's owner, which was the same
  string on every row. It also asks about the method the item names rather than
  the bare name, which is what the `confirmed` tier needs — without an owner it
  could not confirm anything.

- `trekr --usage` summarizes what `--serve` has been asked, from its own log:
  calls per operation, how often the answer was empty, and median/p90 latency.
  Honors `--json`/`--ndjson` like every other command. The log was written to
  debug a defect; this is the other half of why it exists.

- A receiver named after its class is now **typed** by that name, not merely
  ranked by it: `@widget.supplier_region` resolves to `Widget`'s delegate when
  nothing else typed the receiver. Reported as `resolved_via: receiver_name`,
  with confidence graded by the ambiguity it resolved — never the 1.0 of a rung
  that read the answer out of the code. Three corroborations are required, and
  a competing definition in the enclosing scope's own chain blocks it.
  Measured: app-code sites promoted from offered to resolved **61 %**, with
  **no new confidently-wrong answers on either corpus**.

- **Fixed a crash.** `--def` aborted with a stack overflow on some positions in
  a Sorbet-covered checkout. Resolving a constant *path* asks for a name's
  ancestors, and that name can be one already being linearized — at which point
  `ancestors` began a fresh recursion and the per-call cycle guard could not see
  it. The real instance was `File` → `IO` → `IO::EAGAINWaitReadable` → `File`,
  from Ruby core plus committed RBIs. Re-entry now answers empty, as a
  visible cycle already did.

- Four path-comparison bugs fixed, all one shape — a prefix or suffix test with
  no boundary (DEC-026): a checkout claimed files in a sibling whose name
  extended it (`widget_shop` vs `widget_shop-nosorbet`, both present here); the
  same-file ranking signal matched `b.rb` against `ab.rb`; `checkout_containing`
  used SQL `LIKE`, where `_` is a wildcard; and a git-sourced gem claimed a
  checkout of any gem whose name extended its own. Comparisons now go through
  audited helpers whose tests use real absolute paths.

- Two named signals now order residue candidates: **the receiver's name**
  (`@widget.foo` ranks `Widget#foo` first — a convention, so it ranks and never
  promotes) and **this checkout before its dependencies**. Measured on the
  no-Sorbet corpus, where the true definition was offered: app-code first-place
  50 % → 67 % (6 of 9), gem 60 % → 64 % (34 of 53), MRR 0.62 → 0.76 and
  0.72 → 0.76. `--def` says which signal fired in each candidate's `why`.
- Fixed: the "same file" ranking signal compared a checkout-relative path
  against an absolute one and could never fire.

- Real source now wins the **whole ancestor chain** before an `.rbi`
  declaration wins any of it. Tapioca describes methods in owners that do not
  exist at runtime (`Widget::CommonRelationMethods`), and those sit early in
  the chain, so a stub won the lookup outright even after the same-owner rule.
  `Widget.find` answered from the RBI where Ruby dispatches to
  `ActiveRecord::Core::ClassMethods`. Measured on a Sorbet-covered app: correct
  42.9 % → 46.0 %, confidently wrong 7.9 % → 4.8 %.

- A local assigned from an ActiveRecord finder is now typed: `w = Widget.find(id)`
  makes `w` a `Widget`, so calls on it resolve. Reported as `resolved_via:
  finder`, not `sig` — it is a convention, not a declaration, and it is the last
  rung tried. `where`/`all`/`order` are excluded: they answer with a relation.
  Measured across rails, discourse and mastodon: 4,333 assignments of this shape
  would newly type **12,005 call sites**, about half the reach of the `.new`
  rung already shipped.

- An `.rbi` is a declaration, never an implementation: a method with real
  source and a Sorbet stub now answers with the source. An app that commits
  `sorbet/rbi/gems/` holds a stub for every gem method it calls, and those beat
  the gem itself — `belongs_to` landed on `activerecord@8.1.3.1.rbi` instead of
  `associations.rb`. Measured on app code: correct 19 % → 43 %, confidently
  wrong 32 % → 8 %.

- A Rails macro is now recorded as a **call site** as well as a generator of
  the methods it implies. `belongs_to`, `has_many`, `scope`, `delegate`,
  `after_save`, `attr_reader`, `private` — asking what any of them is used to
  answer "no name at this position", because consuming the macro swallowed the
  call with it. Measured on widget_shop's app code, that was the single largest
  miss: 12 of 63 call sites, in a Rails class body that is mostly macros.
- **Existing databases reindex once** (DEC-013): the extractor emits more.

- Fixed: a definition resolving into a **different checkout** — a gem, almost
  always — was handed back rooted on the repo being asked about, naming files
  that do not exist. `@account.local?` in mastodon offered
  `mastodon/lib/prism/string_query.rb`; the real definition is in the prism
  gem, and mastodon has no `lib/prism`. A tree spans a repo and every gem it
  resolves, so a site's path is now absolute from the store outward rather than
  relative to a checkout the caller has to guess.

- A position inside **gem source** now answers. Gems are indexed but are not git
  repositories, so `repo_root` could not place a file in one and `--def` (and
  the LSP surface) refused with "not a git repository" — one step after
  following a definition into a gem, which is where an agent routinely is. The
  checkout is now the file's git repo *or*, failing that, the longest indexed
  root containing it.
- An older trekr meeting a **newer** database now refuses instead of dropping
  it. A version mismatch reindexes (DEC-009), which is right in one direction
  only; in the other, a stale install silently destroyed a newer index and then
  looked like it had never been run.

- Fixed: a `--serve` session went on answering from a tree assembled **before**
  an edit that had been reindexed underneath it. The rebuild key was (schema
  version, file count), and editing a file moves neither — only adding or
  removing one did, which is what hid it. The key is now the checkout's
  *surface*: every file's path folded together with a digest of the
  tree-relevant facts of its blob, computed once at index time and read as one
  row per request.
- A blob now carries a `surface` digest — its definitions and ancestry, the
  only facts the tree layer reads. Measured over 5,158 modified blobs across
  500 commits of rails, discourse and CRuby, **71 % of edits leave it
  unchanged**, so most edits need no tree rebuild at all.
- **Existing databases reindex once**: the schema gained those two columns.

- `--symbols FILE` parses the file instead of querying the index. It was the
  one query verb that did not — `--def` and `--refs` both reparse so an
  unindexed edit still answers — so an outline could be stale, and on a repo
  nobody had indexed it printed `no symbols … (indexed? try --index)`. Now any
  readable Ruby file outlines, in a repo or not, matching the LSP surface
  (DEC-024). Exit 1 is reserved for a file that really defines nothing.

- `concerning :Name do … end` is a module definition and an `include`, so it
  now emits both: the block's methods own themselves as `Enclosing::Name`
  rather than landing on the class, and the class reaches them. Measured yield
  on the bench corpora is near zero — three occurrences across rails,
  discourse and mastodon, two of them inside Rails' own test for the feature —
  so this is correctness for apps that write the idiom, not a win here.
- `delegate … prefix:` now defines the prefixed name — `prefix: true` takes the
  `to:` target (`supplier_region`), a symbol is used as written. It used to
  refuse the whole delegation rather than guess; the rename is a rule Rails
  follows exactly, so there was nothing to guess. A *computed* prefix is still
  refused. 24 of the 301 delegations in rails, discourse and mastodon carry a
  prefix, and none of them was modelled before.
- **Existing databases reindex once** (DEC-013): the extractor changed.

- Fixed: `--serve` answered **nothing at all** for a file outside the client's
  workspace root — which is every file, when the client is Claude Code and its
  root is whatever directory the session started in. The session now holds a
  tree per checkout and finds the one a file belongs to (DEC-024). Outlining a
  file and reporting its syntax errors need no checkout at all now, and work on
  a loose `.rb` that is in no repository.
- `workspaceSymbol` searches every indexed checkout when the client's root is
  not one of them, rather than answering nothing.

- Fixed: `--def FILE:LINE:COL` resolved against the **current directory's**
  checkout rather than the file's own. Asking about another repo's file — which
  is what an agent does constantly — silently answered `residue`, because the
  tree it consulted had never heard of that file. The unit is now the file's
  enclosing repository, whatever directory the process is standing in.

- `trekr --serve` logs what it did, as ndjson: the client's `initialize` root,
  one line per request with the file, line, duration and **how much came back**,
  and the notifications. Default `~/.local/share/trekr/serve.log` (beside the
  database, so `$TREKR_DB` moves it too); `TREKR_LOG` takes a path, `-` for
  stderr or `off`, and `--serve --profile` (or `TREKR_LOG_LEVEL=debug`) adds the
  wire-level params. Never stdout — that is the LSP wire.

- `goToDefinition` returns ranked candidates when the receiver does not
  resolve, up to five, ordered by proximity — the answer the CLI always gave
  and the LSP surface was discarding. `hover` at the same position reports
  `Residue` and `confidence: 0.00`, so a guess is legible as one.
- Core definitions now have a location: `core.rb` is written beside the
  database, so `require` and `Array#each` land on a readable stub instead of
  answering nothing.
- A model overriding `self.table_name` gets that table's columns.
- Measured, with discourse's bundle installed: the chain-truncated bucket
  **disappeared** (24 of 120 samples → 0 of 60), `self` inside a class went
  52 % → 83 %, and overall resolution 31 % → 43 %. Session 5 could only confirm
  the gem hypothesis negatively; this confirms it positively.
- Measured: goToDefinition coverage on the baseline's 45 positions went
  **19/45 → 44/45**, against ruby-lsp's 33/45. Details and the hand-adjudicated
  losses in `docs/BASELINE.md`.

- `trekr --serve`: LSP over stdio. goToDefinition, findReferences (confirmed
  ordered before possible), documentSymbol, workspaceSymbol, hover,
  goToImplementation, call hierarchy, and Prism syntax diagnostics. The editor
  owns the process — no auto-spawn, no lockfile. Completion, rename,
  formatting and semantic tokens are deliberately not announced.
- Warm latency on rails: goToDefinition **0–1 ms** (463 ms first call, which
  builds the tree), documentSymbol and hover **0 ms**, references **25 ms**
  against ~245 ms for the same query on the CLI.

- Rails class macros now define methods in the index: `delegate` (including
  `delegate(*CONST, to: :x)` where the constant is a literal symbol array in
  the same file), the association family, `scope`, `class_attribute`,
  `mattr`/`cattr` accessors, `attribute`, `store_accessor`, `alias_attribute`.
  A singular association's reader carries a **type**, so `belongs_to :user`
  makes `user` a typed receiver.
- A concern's nested `ClassMethods` now reaches the class that includes it —
  `ActiveSupport::Concern` extends it with no `extend` ever written, so it is a
  tree fact by construction.
- Measured: on the same twelve heavy-collision names, `--refs` confirmed rose
  **32 % → 47 %** and the weak `no_such_method` exclusion reason fell from 82 %
  of exclusions to 42 %. `--def` on rails rose 39 % → 42 %.
- **Existing databases reindex once** (DEC-013): the extractor changed.

- `--refs 'Owner#method'` narrows references by receiver: **confirmed** (the
  receiver's type resolves and Ruby's lookup lands here), **possible** (untyped
  receiver, ranked by proximity, never dropped), and **excluded** — not listed
  but counted, because that count is what a grep cannot produce.
  `Owner.method` asks the class-method question instead, and a bare name keeps
  the whole-mention view with each call site now naming the owner it reaches.
- Measured on rails over twelve heavy-collision method names: of 25,297
  same-name call sites, **32 % confirmed, 43 % possible, 24 % excluded** —
  where `rg -w` returns all of them undifferentiated. A refs query costs
  360–400 ms, of which 210 ms is the tree build.
- `--refs --include-excluded` lists the ruled-out sites with their reason, so
  the count is auditable rather than asserted. Exclusions are reported by
  reason, because only one of the three is positive evidence (DEC-021).

- Three new receiver-typing rungs: `sig:param` (a parameter's declared class,
  from the `params(...)` half of a signature), `literal` (`out = []` is an
  Array), and `sig:step` (one call on an already-typed local, and only one).
- A method whose only definition is a Tapioca `sorbet/rbi/dsl/` file now
  answers with the **model**, not the `.rbi`, and reports `resolved_via:
  rbi_dsl`.
- Constants a declaration implies but nothing declares — `ActivityPub` in
  `class ActivityPub::TagManager`, which Rails' autoloader creates from the
  directory — now resolve, carrying no sites because nothing declares them.
- `make bench` gained mastodon and graph_weaver, excludes `sorbet/` from
  sampling, and splits method residue by whether the ancestor chain was
  complete. Measured: **0 of 71 chain-truncated call sites resolve**, which
  confirms the gem hypothesis without needing an installed bundle.

- Measured, after core and gems: **98 % of rails constant references resolve**
  (82 % before this session), 91 % discourse, 84 % CRuby. Method resolution
  reached 38 % on rails from 27 %, all of it from core — gems added nothing
  measurable, because the limit has moved from "is it in the index" to "can we
  type the receiver". Details and caveats in `docs/ARCHITECTURE.md`.
- Tree rebuild is now 202 ms on rails (was 120 ms), because it assembles the
  gems too. `--profile` and `make bench` both report it.

- Gems are indexed. `trekr --index` reads `Gemfile.lock`, locates each gem by
  convention, and indexes its `lib/` once per machine — shared by every project
  that resolves the same version. No `bundle`, no `gem`, no Ruby (DEC-016).
  `--no-gems` skips it.
- A gem the lockfile names but disk does not have is **reported**, in the text
  output and as `gems.missing` in `--json`. Path-sourced gems are not counted,
  because their code is inside the checkout already.

- Ruby core is now indexed. `puts`, `raise`, `block_given?`, `Foo.new`,
  a class body's `prepend`, `ArgumentError`, `ENV` and the rest resolve,
  because every class now carries its implicit `Object → Kernel → BasicObject`
  tail and singleton lookup continues into `Class → Module`. Core comes from a
  vendored Ruby stub read by the ordinary extractor (DEC-015), so no RBS gem
  and no Ruby toolchain.
- `--ancestors` output now ends in the core tail, which is real. A module
  still gets none, because a module has no superclass.

- `--jobs N` (and `TREKR_JOBS`; the flag wins) sets the parse worker count.
  `0`, the default, picks the machine's **physical** core count rather than
  rayon's default of logical cores.
- `--index --profile` reports where the time went — per-phase wall time, blobs
  parsed vs already known, bytes read, parse throughput, and the slowest files.
  Human-readable on stderr, and structured on stderr when `--json` is on, so
  `trekr --index --json --profile | jq` still sees only the answer.
- Calls written inside a module now resolve through the class that mixes the
  module in. `ActiveRecord::Transactions#destroyed?` finds `Persistence`
  because `Base` includes both; confidence is the share of mixing-in classes
  that agree, disclosed as `"1/3 includers"`.

- Fixed: a bare call in a class body was looked up as an instance method.
  `self` in a class body is the class, so `validates :name` and `prepend Foo`
  dispatch on it. "Is a `def` here a singleton method" and "what is `self` for
  a call here" are different questions and were sharing one flag.
- `--def` on a call now reports `receiver_kind` and `unresolved_ancestors`.
  A miss inside a **module** is expected rather than a failure — the module is
  not the real receiver, whatever includes it is — and a miss below an
  unindexed gem ancestor is a weaker "no" than one below a complete chain.
- The cache version now covers changes to **what the extractor emits**, not
  just the schema (DEC-013). Facts are keyed by blob OID, so an extractor fix
  otherwise ships dead against an already-indexed repo. **Existing databases
  reindex once.**

- Method resolution at `--def` (`resolve/`): the receiver ladder in
  measured-yield order — implicit/explicit `self`, constant receivers, locals
  and instance variables typed from their assignments, then inline Sorbet
  `sig` returns. An undetermined receiver returns ordered candidates with the
  receiver shape as the reason, never a bare list.
- Singleton chains in the tree layer: `def self.x`, `class << self`, and
  `extend` all feed one lookup that walks the *superclass* chain (included
  modules contribute no class methods) inserting each level's singleton
  methods and extended modules.
- Method tables, keyed by (owner, singleton, name), with arity.
- `call_site` gains a `singleton` column: the same source line means a
  different lookup inside `def self.x` than inside `def x`. **Existing
  databases reindex once** (DEC-009).

- Tree layer (`tree/`): a checkout's constant namespace and ancestor
  linearization, assembled from blob facts. Ruby's own lookup ladder — lexical
  scopes, then the innermost scope's ancestors, then the top level — with
  path segments descending through ancestors only. Constant aliases are
  followed wherever a namespace is wanted.
- `--def FILE:LINE:COL`: what is the name at this position and where is it
  defined. Reparses the one file, so it answers correctly on an unindexed edit.
  Constants resolve exactly; a method call is honest residue carrying its
  receiver shape.
- `--ancestors NAME`: the linearized chain, with anything unresolvable named
  rather than dropped.
- Measured: 82 % of rails constant references resolve (78 % discourse, 73 %
  CRuby), and every unresolved one names a gem or a core class that is not
  indexed — none is a resolver bug. A whole-checkout tree rebuild is 43 ms for
  rails, 73 ms for discourse. `make bench` reproduces both.
- A schema change now drops the database and reindexes instead of migrating
  (DEC-009). The store is a cache of a pure function; **existing databases will
  reindex once** on first use of this version.

- `--refs NAME`: every mention of a name in a checkout — definitions, constant
  references, and call sites — each disclosing what sort of mention it is and,
  for a call, the receiver's shape. Name-level; narrowing is the resolve
  layer's job.
- Fixed: `trekr … | head` panicked instead of exiting, because Rust ignores
  SIGPIPE.
- Fixed: `--refs` on a common name took 90 s. The store now runs
  `PRAGMA optimize` on close, without which SQLite plans the join as a nested
  scan.
- Measured: rails (3.3k files) indexes cold in 1.5 s and reindexes in 61 ms
  with nothing parsed; discourse (11.3k) in 3.2 s / 121 ms; CRuby (7.9k) in
  2.4 s / 98 ms. A second worktree costs ~0.2 s and zero parses. Reproduce with
  `make bench`; caveats in `docs/ARCHITECTURE.md`.
- Scaffolded the crate: single binary `trekr`, modules mirroring PLAN §4's
  layers, `script/check.sh` as the commit gate.
- Blob-layer extraction (`extract/`): Prism reads one blob's bytes into
  definitions, ancestry edges, constant references, and call sites carrying
  receiver shape. Semantics lifted from Shopify's Rubydex (MIT); the crate is
  not a dependency.
- Checkout scan (`scan/`): `git ls-files -s` for tracked blob OIDs, git's own
  `sha1("blob <len>\0" + bytes)` for anything the working tree has changed, so
  an uncommitted edit keys the same as it will once committed.
