# Reading trekr's answers

A field-by-field reference to what trekr prints, mostly under `--json`. The
[README](../README.md) is the tour and the [Claude skill](../claude/trekr-skill.md)
is the short version an agent loads; this is where the detail lives.

## Statuses and exit codes

| you get | it means |
| --- | --- |
| `status: resolved`, exit 0 | no competitor is known. |
| `status: ambiguous`, exit 0 | a competitor is known: the pick is first, the others are `candidates`. |
| `status: residue`, exit 1 | trekr looked. The receiver is genuinely undetermined — ranked `candidates` say what it might be, each with a `why`. |
| `status: residue`, `reason: "no name at this position"`, exit 1 | `--def` on a line with no name on it (blank, a comment, only punctuation). |
| `status: no_such_method`, exit 1 | `'Owner#name'`: the owner resolved and nothing in its ancestors defines the name — `reason` says so, and nothing is listed. `--refs` adds a `hint` (`trekr --refs name`) for every call site of the name, unnarrowed. A chain with an unindexed ancestor is `residue` instead, naming it; an owner trekr cannot find at all is `residue` with the same `hint`. |
| exit 1, "no mention of …" | indexed, and the name really is not there. |
| `status: not_indexed`, exit 2 | only with `--no-index`: nobody has indexed this checkout, or another trekr's first index of it is still running (its `reason` says which). The answer names the root and the command. |
| `status: incomplete`, exit 2 | the index this query started could not finish (another writer held the store's lock for 10 minutes). Run the `hint`, then ask again. |
| exit 2 while `warming` | with `--no-index`, a miss while a first index is still running. Ask again. |
| exit 64–74, `{"error", "kind", "code"}` | the call failed; see [Errors](#errors). Not an answer about the code. |

| exit | means | do |
| --- | --- | --- |
| `0` | an answer (`resolved` or `ambiguous`, something listed) | read it |
| `1` | nothing found: `no_such_method` (certain), `residue` (it names what it could not see), no mention | read `reason`/`candidates` |
| `2` | `incomplete`; with `--no-index`, `not_indexed` or a miss while `warming` | run the `hint`, ask again |
| `64` | `usage`: the command line is wrong | fix the command; a retry won't help |
| `66` | `not_found`, `not_a_repo`: a path is missing or in no checkout | fix the path |
| `69` | `git`: git could not be run | |
| `70` | `internal`: a trekr bug | |
| `74` | `database`, `io`: the index or a file could not be read or written | check the disk or `$TREKR_DB` |

An `--index` stopped by a signal (Ctrl-C, a caller's timeout) says how far
it got on stderr — and under `--json` as `status: incomplete` — then exits
as that signal does (130 for Ctrl-C), never `0`.

### Errors

When a run fails under `--json`/`--ndjson`, stdout carries one object instead
of an answer:

```json
{ "error": "cannot read app/gone.rb: No such file or directory (os error 2)", "kind": "not_found", "code": 66 }
```

`kind` is stable and `code` is the exit code. The message also goes to stderr;
in text mode stdout stays empty. Codes come from `sysexits(3)`, one per remedy,
as in [rq](https://github.com/dpep/rq):

| `kind` | Exit | Meaning |
| --- | --- | --- |
| `usage` | 64 | The command line is wrong: an unknown flag, a bad value, an input whose shape trekr cannot tell, nothing asked. JSON under `--json` wherever the flag sits. Retrying won't help. |
| `not_found`, `not_a_repo` | 66 | A path the command names does not exist, or no git checkout contains it. |
| `git` | 69 | git could not be run. |
| `internal` | 70 | trekr failed at something that should always work — a bug. |
| `database`, `io` | 74 | The index, or a file it reads, could not be opened, read or written. |

A damaged or unusable index is not an error: see
[where trekr keeps things](../README.md#where-it-keeps-things).

## Fields shared across commands

One name per fact: `query` is what you typed, `fqn` what it resolved to,
`definition` where it is defined (always present, `[]` when unknown),
`receiver`/`receiver_text`/`receiver_type` for a call's receiver,
`unresolved_ancestors` for what could not be seen, and `path` + `root` +
`line` + `col` on everything located. `singleton` is true for a class-side
method (`def self.x`, `class << self`), `end_line` is the last line of a
definition's body, and `nesting` the enclosing class and module names as
written, innermost first. `repo` is a checkout's root.

**`path` is relative to the `root` beside it**: the checkout holding the
file, which for a gem's method is the gem. Join the two for a file to open.
`root` is `null` only for Ruby core. Text output writes a path in the
checkout you asked about relative to it, and anything else absolute.

**`warming`** (`read` of `of` files in) means the answer came from a first
index still running: it may change, and claims nothing certain.

**`--ndjson`**: a row set (`--refs`, `--dead`, `--symbols`, `--usage`) is one
row per line, as the `--json` array holds it, then a last `{"answer": {…}}`
line: the rest of the `--json` answer (`counts`, `summary`, `status`…) and
`rows`, the count. Filter rows with `select(.answer | not)`.

## `status`, `confidence`, `resolved_via`

They answer different questions; read them separately.

- **`status`** — is a competitor *known*? `resolved`: no. `ambiguous`: yes.
  `residue`: the receiver is undetermined.
- **`confidence`** — the share of the evidence that agrees. A local whose
  read three writes can reach — two `Foo.new`, one from an untyped call — is
  `resolved` at 0.67: nothing contradicts `Foo`, but not everything says it.
  No confidence is low enough to turn `resolved` into `ambiguous`. A
  `residue`'s is how often its first candidate ran, on the gold sets, among
  residues resting on the same evidence — 0.7 for a call on `self` or a name
  with at most three definitions, 0.2 for a name many classes define on an
  untyped receiver — and `agreement` says which.
- **`resolved_via`** — the rung that typed the receiver: `self`, `const`,
  `local:new`, `literal`, `sig`, `sig:param`, `sig:step`, `includer`,
  `rbi_dsl`, `super`, and `flow` for a variable; `view`, `sidecar` (a
  component's template, on the component), `controller`, `render` and
  `rabl:object` in templates; `require` for a `require` string. `chain` means the receiver is a
  call whose method's return type is declared (`x.strip.downcase` with `x`
  typed); `chain:name` that its receiver was untyped, so every definition of
  that name was asked — `ambiguous` when some declare no return type. Ruby
  core's return types come from the RBS signatures of the checkout's own
  Ruby, so `x.gsub(a, b).downcase` is `String#downcase`.
  On a constant it is the rung of Ruby's constant lookup that found it:
  `lexical` (an enclosing scope), `ancestor` (the innermost scope's
  ancestors), `root` (the top level, which is Object's constants,
  and Kernel's from a module or the top level), or `path` (a later segment of a path,
  under the one before it).

Beside them, a call's answer carries `receiver_kind` (`class` or `module`:
inside a module an implicit receiver is whatever includes it, so a miss there
is expected), and a constant's or call's `context` names the checkout it was
resolved in. A residue's `candidates[]` each have `owner`, `singleton`,
`why`, `kind` and the `site` they point at. A constant that does not resolve
says how many scopes it tried (`scopes_tried`).

`--explain` renders the same facts as text.

## `kind`: the code, or the line that declared it

```json
{ "status": "resolved", "owner": "Widget", "kind": "declaration",
  "defined_via": "belongs_to",
  "definition": [{"path": "app/models/widget.rb", "root": "/…/app", "line": 7}] }
```

- **`definition`** — the body is there. A `def`, or a `define_method` block
  (`define_method(:x, some_method)` is a declaration: the body is elsewhere).
- **`declaration`** — the name was made or described there and runs
  elsewhere: a macro (`belongs_to`, `has_many`, `enum`, `scope`, `delegate`,
  `def_delegator`, `schema` for a column, `define_model_callbacks`), an alias,
  a bare `private :foo`, or a Sorbet stub (`defined_via: rbi`).
  `defined_via` names which.

Real source always wins over a stub, so an `rbi` answer means the
implementation is not indexed — usually a gem that has not been indexed yet.
A declaration is usually the line a person wants (`belongs_to :supplier`
explains `widget.supplier` better than the `define_method` inside Rails
does), but it is not the code that runs. Residue candidates carry their own
`kind` too.

`kind` on the answer is about the *location*. `kind` inside `definition[]`
and `--symbols` is about the *symbol* — class, module, method, constant.

**`signatures`** — where Sorbet describes the method `definition` names: its
`.rbi` stubs, located like `definition`. Absent when there are none. Real
source beside a signature stays the `definition`; the signature is a
declaration, and it is what the LSP's Go to Declaration answers (Go to
Definition lists it only when it is all there is). On a method's own `def`
(`under: definition`) it is the stubs that describe that method — the
owner, side and name the file writes — read from the index as it stands, so
a checkout not yet indexed shows none. A constant's `.rbi`
reopenings stay in its `definition`, listed after its real ones; a residue's
candidates list a signature of another candidate's method after every
distinct candidate. A stub that is itself the answer — a gem's method only
its RBI describes — is its own signature, so `signatures` then repeats that
`definition` site: merge the two by location, not by concatenation.

## `--def`

**Snapping.** If the column holds no name, trekr answers for the nearest one
on that line and adds `snapped_to` — the name it picked, its column, and the
line's other names as `alternatives` (text says it on the line under the
answer). No `snapped_to` means the column hit the name. The string in
`it_behaves_like "x"`, `include_examples` or `include_context` is not
snapped: it answers the shared group of that name. Nor is a symbol no rule
reads as a method's name (`on: :create`, a hash key): `residue`,
`under: symbol`.

**`require` strings.** Anywhere in the string of a `require`,
`require_relative`, `load` or `autoload`, quotes included, `--def` answers
the file it loads, as Go to Definition opens it: `under: require`,
`resolved_via: require`, `name` the path as written, and `definition` each
file found at its top (`kind: file`), in load-path order — `ambiguous` when
there are several. The stdlib's copy is found only when no gem or path gem
ahead of it on the path has the file, so a bundled `json` answers alone. No file found, or a compiled extension first on the path,
is `residue` with a `reason`.

**Variables.** On a variable, `--def` answers the variable, not the nearest
call: `under: variable`, `resolved_via: flow`, and `definition` is the writes
its value can come from (`kind: assigned`).

| `variable` | where the writes come from |
| --- | --- |
| `local`, `parameter` | flow through the method: both branches of an `if`, a loop's later write, the parameter itself |
| `ivar`, `cvar` | the writes to that `@x` or `@@x` in this file only, and `reason` says so. The LSP also searches the class's other files and its ancestors. |

**`super`** answers the method it runs: the next definition after the
method's owner in the ancestors (prepends, the class, includes, the
superclass chain), per includer when the method is in a module. A `super`
whose owner the source does not name — in a block, or `def obj.x` — is
`residue`, never a guess.

**`X.new`** lands on the `initialize` it runs (on a custom `def self.new`, on
that); `trekr Widget.new` or `--refs Widget.new` asks about it.

**Freshness** (DEC-035). `--def`, `--dead` and every `--refs` compare the
working tree with the index and read each file edited, added (untracked
too) or deleted since, for that answer only. No file map is written; the
facts of new bytes are recorded by their content, and `--gc` collects those
no checkout maps. When anything differed, the answer carries `index`:

```json
"index": { "stale": false, "refreshed": "app/models/user.rb",
           "refreshed_files": ["app/models/user.rb", "app/models/post.rb"],
           "hint": "trekr --index ~/code/app" }
```

- `index.refreshed_files` — every file read as it is now. `refreshed` is
  the file asked about when it is one of them, else `null`.
- `index.busy_files` — changed, but another trekr was writing the index, so
  answered from the indexed version. `busy` is the file asked about among
  them, else `null`. Both appear only when there are any.
- `index.stale` — other files may still differ from what was read: more
  than 32 changed (the file asked about is still read), git failed or took
  over a second, or `busy_files` (another trekr process, an index or a
  sibling query, held the store). `index.cause` says which. `hint` is the
  cure.

No `index` field means the working tree matched the index. `--refs NAME`'s
`--json` is a bare array, so it says this on stderr, and `--ndjson` in its
closing line. `--ancestors` and cards carry no `index` and read only what is
indexed.

## `--refs`

```json
{ "status": "resolved", "owner": "ActiveRecord::ConnectionHandling", "method": "lease_connection",
  "definition": [{"path": "activerecord/lib/active_record/connection_handling.rb", "line": 269,
                  "root": "/…/rails"}],
  "resolves_to": "ActiveRecord::ConnectionHandling#lease_connection", "inherited": false,
  "counts": {"confirmed": 1024, "possible": 84, "excluded": 87,
             "excluded_different_owner": 56, "excluded_no_such_method": 31,
             "excluded_arity": 0},
  "references": [{"path": "actioncable/test/subscription_adapter/postgresql_test.rb",
                  "root": "/…/rails", "line": 26, "col": 26, "tier": "confirmed",
                  "receiver": "const", "receiver_type": "ActiveRecord::Base",
                  "owner": "ActiveRecord::ConnectionHandling",
                  "why": "the receiver's type resolves here"}] }
```

- **confirmed** — the receiver's type resolves and Ruby's lookup lands here.
  For a method the owner inherits (`resolves_to` names where, and
  `inherited` is true), that is a receiver of the owner or a subclass landing
  on it; another class inheriting the same method is excluded.
- **possible** — untyped receiver, nothing rules it out; or one typed as an
  ancestor (`self`, a `sig`'s type) that may be the subclass defining this.
  Ranked, never dropped.
- **excluded** — counted, not listed. `--include-excluded` shows them with
  the reason.

`Owner.method` asks about a class method. A bare `--refs name` keeps the
whole-mention view. A position (`FILE:LINE[:COL]`) asks about what is there:
a method's definition or call as `Owner#method`; a class, module or constant
(its definition or a reference) as `--refs` by its whole name; a variable (a local's mentions in its scope, an `@ivar`'s
across its class's files, each `read` or `write`); or a spec's `let`,
`subject` or group `def` — every read as RSpec runs it (its group's and
nested groups' examples, an enclosing group's hooks, an included shared
group's body in any file, `super`, `is_expected`, a `config.include`d helper,
a shared context included by metadata, a `config.before` hook), each with
`from` saying where. A column on none of these exits 64.

A spec member's answer (`let`, `subject`, group `def`) also counts the
shared groups' bodies and helper modules read for it (`shared_groups_read`,
`helpers_read`), and `caveats` lists what may read it unseen: a name sent at
runtime, a macro not read.

A bare `--refs name` lists every mention of the name: each row says what
sort it is (`role`: `definition`, `call` or `constant`; a definition's
`kind`: `class`, `module`, `constant` or `method`) and the `nesting` it is
written in — `singleton_class` in it for a `class << self` body's constants.

A name written with `::` is a constant's whole name — `Admin::Widget`, or
`::Widget` for a top-level one, `Widget::singleton_class::LIMIT` for one
assigned in `class << self` (a text miss on `Widget::LIMIT` names it): the
same rows, but only its definitions and
the references Ruby's lookup resolves to it through the scopes they are
written in (`Widget` inside `module Admin`, `Admin::Widget`,
`::Admin::Widget`). A path through an includer, a subclass or an alias
(`Host::K` for a `K` that `include Mixin` brings, `Child::K`, `Alias::K`
where `Alias = Admin::Widget`) is not found by name: such a mention is
listed under the name it reaches, and a text miss on the path names that
one — and the ancestor, when it is one (`File::NULL`: "found through
File's ancestor IO, it is IO::NULL"). One the index cannot
place whole is named by the longest leading part it can place plus the rest
as written: `Rack::Utils` inside `module App` is `App::Rack::Utils` once
`App::Rack` resolves, and `--refs App::Rack::Utils` lists it. One it cannot
place at all is listed only when written out in full. `--refs` at a mention
asks for the name the mention is placed as, and lists the mention itself
however it is written.

`--refs 'Widget#initialize'` lists the `new`s whose class runs it (its own or
a subclass that inherits it), each marked `"called_as": "new"`. An untyped
`klass.new` is possible for every `initialize` it fits.

A predicate's references include the RSpec predicate matchers in examples
that call it, each marked `called_as` with the matcher's name:
`have_event_count` for `has_event_count?`, `be_visible`, `be_a_visible` or
`be_an_visible` for `visible?`, and `be_exist` for `exist?` or `exists?`:
RSpec calls `exist?` when the subject has it and `exists?` only when it
does not. Each is tiered by the expectation's subject (`expect(x).to`):
where that is typed, `be_exist` is a call of whichever of the two its type
answers and is excluded for the other; where it is untyped, it is possible
for both. A matcher
the group, RSpec or a gem defines (`be_within`) runs that method instead,
and is excluded. `--dead` and the editor's Find References count them the
same way.

Through the LSP, findReferences is capped (1,000 by default, confirmed
callers first) and the cut is only announced to editors. Exactly that many
locations means the list was cut.

## `--dead`

`--dead` grades candidates for deletion; it never calls anything dead. It
checks every method defined in scope against references from the whole
checkout (not other indexed repos, so the answer does not depend on them),
then a spec's `let`s, `subject`s, group `def`s (`kind: let|subject|method`,
with `group`) and shared groups (`kind: shared_group`), then every class,
module and constant (`kind: class|module|constant`) no constant reference
resolves to. Each row says how much evidence of use was found and how far
to trust it; [what a grade is worth](#what-a-grade-is-worth) was measured.

### Tiers

From least evidence of use to most:

- `unreferenced` — nothing names it.
- `shadowed` — names it, but every such call lands on an override: a
  subclass's, for a method; a nested group's, for a `let` (`overridden_by`).
- `test-only` — a method only specs and tests call, or a class, module or
  constant only they name.
- `override` — nothing names it, but it overrides an ancestor's method
  (`overrides`), so whatever calls that one (often the framework) may run it.
- `convention-only` — named only by a symbol handed to a macro
  (`before_action :x`, `if: :x`, `with: :x`, a guard or numericality bound
  in a validator's options, `validates :n, numericality: { less_than: :x }`,
  a graphql-ruby `field`'s `method:` or an `argument`'s `prepare:`;
  a symbol handed to `send` or
  `try` is a call instead, DEC-664), a controller action only a
  route reaches (`config/routes.rb` is read), or a method a library calls by
  a name it builds: ActiveModel::Serializers' `include_<attr>?`, a Thor
  command, a test case's public `test_` method, which Minitest or
  Test::Unit runs (`convention.by` says which).
- `super-only` — reached only by `super` from its overrides (`super_from`).
- `single-caller` — one reference: an inlining candidate. `caller` says
  where, with its own `tier`: whether it certainly reaches the method.

An `initialize` is reported only when nothing constructs its class.

### Cascades: `cascaded_from`

A method whose only caller is itself a candidate stays `single-caller`,
and its `reason` names that caller. It names none when which `def` holds
the call is unsure: the innermost `def` around it is not a candidate, two
`def`s on its line may each hold it, or a `def` around it ends on its line
(the call may follow its `end`).

When the caller is a `clear` `unreferenced` or `test-only` method, and the
row is `clear` too with no `super_refs` or `symbol_refs`, deleting the
caller leaves this one unreferenced (or reached only by tests). The row
carries `cascaded_from` (`name`, `path`, `line`, `tier`, `root`), and its
reason says "its caller, build, is itself unreferenced". Delete the root
first, then this one. Only one step is named: a method freed through
another candidate, or two that call only each other, carry none, so run
`--dead` again after deleting (DEC-680).

### Caveats

Each row is `clear` or `lower` confidence, and says why in words; a `lower`
row's `caveat` names what may still reach it. Any tier other than `override`
that overrides a method is `lower` ("overrides …"), and so is a
`single-caller` row whose caller is `possible` ("untyped caller"). The files
read are the ones the index lists, untracked files git does not ignore
included (DEC-845).

A method row is `lower` when one of these may reach it:

| what may reach it | the caveat says | DEC |
| --- | --- | --- |
| a Slim, Jbuilder or Builder view that writes the name | "named in a view (…), which is not read" | 846 |
| a Ruby script with no extension | "named in a script (…), which is not read" | |
| a YAML config that writes the name | "named in config (…), which is not read" | 402 |
| Ruby or Rails calling a protocol hook by name | "a hook … calls by name" | 315 |
| a symbol in its file not read as a call, or a route built at runtime | "`x` at line N is not read as a call" | 343, 344 |
| a computed name sent in its file, or its own name sent there | the call | 631, 662 |
| a name of its shape the checkout builds | the shape | 655, 663 |
| its name sent from another file | "… is sent by name at …" | 666 |
| for `single-caller`, a macro of its own class naming it | "a macro names it too, at …" | 666 |
| a writer `assign_attributes` reaches, an unindexed ancestor, a gem of the bundle that calls the name, a helper another helper calls | what it is | 363–368, 370 |
| a file a generator wrote | "in generated code (…), which its runtime may call generically" | 403 |
| for a module's method, a call on a typed receiver with no such method | "called on TopicQuery (…), where trekr finds no such method; …" | 097, 661 |
| an entry point ([below](#entry-points)) | the evidence that would settle it | 657 |

Examples, and what the table leaves out:

- **Views.** A Slim view's Ruby is read for names only, as `--dead Gemfile`
  reads it, so a name there lowers the row instead of counting as a call.
  ERB, Haml and RABL templates are read, and their calls counted.
- **Scripts.** `exe/tool`, `bin/x`.
- **YAML.** `generator: pay_schedule_resources`,
  `Pkg::Sanitizer.clean_uid`. Locale files don't count.
- **Protocol hooks.** `marshal_load`, `to_partial_path`, `each`, a job's
  `perform`.
- **Symbols not read as calls.** `only: [:archive]`.
- **Computed sends in its file.** `send(name)`, `__send__`, `public_send`,
  `try` or `method(name).call`; a symbol is a reference instead. A send
  spelled in any string of the file counts too, as code it may evaluate.
  Its own name sent there as a string or symbol (`send("archive")`,
  `&method(:archive)`) is named beside the call.
- **Built shapes.** `:"report_#{type}"`, a string or symbol its class is
  sent (`Q.new.public_send("list_#{kind}")`, `"#{kind}_attrs"`), or a
  computed name sent to its class (`Mailer.public_send(type)`), in a Ruby
  file or an ERB or Haml template.
- **Sent from another file.** Its name or an alias's, sent as a string or an
  operator, which trekr records no symbol for (`send("archive")`,
  `send(:[], 1)`). It counts when sent to its class or a relative, or to a
  value of any class.
- **A macro beside one call.** `before_action :x`, `validate :x`, a
  serializer's `attributes :x`.
- **Generated code.** `linguist-generated`, a `generated/` directory, a
  "DO NOT EDIT" header.
- **Runtime mixins.** Only on `unreferenced` and `test-only` rows. A module
  mixed in from a block (a plugin's `after_initialize`) is no edge the index
  records.

`clear` on an `unreferenced` row means none of those was found.
`visibility` (`public`, `protected`, `private`) says how complete the
evidence is: a private candidate's is complete, a public one's is not.
Classes, modules and constants carry caveats of their own
([below](#classes-modules-and-constants)), and a spec's members are always
`lower` ([below](#example-groups-members)).

### What a grade is worth

Measured by reading rows by hand, or against what ran:

| row | measured | on | DEC |
| --- | --- | --- | --- |
| method, `unreferenced`, `clear` | truly unused 79 % | mastodon (rules fitted there) | 372 |
| | 100 % fitted, 89 % held out | discourse | 372 |
| | 55 of 70 (79 %) | forem (no rule fitted there) | 450 |
| | less | code unlike an app's (`lib/`) | 372 |
| method, `test-only` | `clear`: reached only by tests 21 of 26; `lower`: 7 of 25 | mastodon, discourse | 652 |
| class or constant, `unreferenced`, `clear` | truly unused 25 of 26 (`test-only`: 21 of 26) | gems held out | 654 |
| class or constant in an app (`lower`) | truly unused 19 of 50 | forem | 450 |
| spec `let` and `subject` | every row listed never ran, and 86 of the 96 that never ran were listed | mastodon, traced | 492–496 |
| | 20 of 20 unused | discourse, sampled | 492–496 |
| | with no caveat: 79 %, 47 %, 0.2 % unused | three held-out batches | 496 |
| gem, `unreferenced`, `clear` | 10 of 10 unused (in-sample) | ten checkouts, 508 gems | |
| gem, `lower` | 8 of 12 in use, 3 not, 1 unsettled | the same | |

- **Methods.** On forem, most misses came from a Pundit policy, an
  algoliasearch callback and a serializer block. Read DEC-372 before
  trusting a grade on a CLI or a library.
- **`test-only`.** The misses were names built at runtime
  (`public_send("list_#{action}")`), which trekr reads (DEC-655).
- **Classes.** An app nearly always looks up a computed constant name, so
  its class rows stay `lower`. Read one as a lead to check, not a verdict.
- **Spec members.** Each held-out suite found a way in the rules did not
  read yet (a shared context included by metadata, a shared group written
  inside a module, a string `eval`ed), so every spec row is `lower`.
- **Gems.** The rules were fitted on the ten checkouts: mastodon,
  discourse, rails, sidekiq, rubocop, faraday, graphql, flipper, webmock,
  widget_shop. The ten `clear` rows were octokit, aws-sdk-sns, flamegraph,
  and seven that another gem keeps in the bundle (`needed_by`).

Git history is no stand-in. Discourse deleted 2.5 % of its methods in a
year, and no tool's candidates (trekr's, Spoom's or sorbet-deadcode's)
stand out from that base rate
([COMPARISON](COMPARISON.md#--dead-against-spoom-and-sorbet-deadcode--discourse-by-git-history)).
Hence the grading, and why trekr never says "dead".

### `test-only`

A method every call, symbol and `super` of which is written in a test is
`test-only`, however many there are; the `reason` says how many and where
the first is, and `test_refs` counts them. A test is a file under a
`spec/`, `test/` or `tests/` directory at the checkout's root or a nested
package's (`plugins/chat/spec/`), not one inside `lib/` or `app/`
(rack-test's library is `lib/rack/test/`). A route, a library's convention,
an override, a `super` into a method trekr has not indexed, or a protocol
hook outranks the tests: such a method keeps the tier it would have without
them. A script or a view that writes the name does not: the row stays
`test-only`, graded `lower`, and says so. A spec's own helper is never
`test-only`. A public `test-only` method in a library's `lib/` is usually
its API, so it is graded `lower` as an entry point (below).
[What it is worth](#what-a-grade-is-worth).

**Its tests** (DEC-730). A `test-only` method row carries `test_sites`:
each call, symbol and `super` a test writes (`path`, `line`, `col`), with
the `example` it is written in (an `it`, `specify`, `its`, Active Support's
`test`, a `test_` method) and the innermost `group` (`describe`, `context`,
a shared group's body), each as `name`, `line`, `end_line`; either absent
where there is none (a hook, a `let`, a helper; a Minitest class is no
group).

### Entry points

A row reached from outside the repo, where no caller can be seen, keeps its
tier, is graded `lower`, and carries `entry_point` (DEC-657); absent
otherwise. Its `caveat` names the evidence that would settle it.

| `entry_point` | what it is | settle it with |
| --- | --- | --- |
| `api` | a library's public API, unless marked internal (below) | its dependents, or usage data |
| `route` | a routed controller action, or a controller only routes name | request logs or APM (e.g. Datadog) |
| `job` | an Active Job or Sidekiq job class nothing outside the tests names | job metrics or logs |

**`api`** is a public or protected method (RDoc documents protected ones), a
top-level `def` (private to Object, so callable anywhere), a class or module, or a constant bound to a constant path, resolved or not
(`Worker = Job`, `Client = OtherGem::Client`), or to a
`DeprecatedConstantProxy`, in a library's `lib/`. Any other value constant
is not API.

A library's `lib/` is the checkout's own, or one beside a nested
`*.gemspec`, in a checkout that is no app. An app has a
`config/application.rb` at its root, gemspec or not, or a `config.ru` or an
`app/` without a root `*.gemspec` — so an engine gem's `app/` or a gem's
demo `config.ru` does not make one. A library needs no gemspec: a Hoe gem
has none until release. An app's own engine is not a library.

A row is not API when its author marks it internal:

- `:nodoc:` on its line (RDoc);
- in the comment above, `@api private` or `@private` opening a line (YARD
  reads a tag only there), or `Private:` or `Internal:` opening the first
  (TomDoc);
- a name with a leading underscore;
- a `private_constant` holding it.

A mark reaches as far as its tool reads it:

- A `:nodoc:` or TomDoc mark on a class hides that class and the methods
  its body defines, not the classes nested in it (RDoc). In such a body,
  the last own-line `# :doc:` or `# :nodoc:` above a method, at its
  indentation, decides for that method.
- `:nodoc: all`, `@api private` and `@private` hide the class and
  everything its marked opening holds, not what another opening of it
  defines.
- A `private_constant` hides everything nested in it, wherever written.
- A method's own `@api public` overrides its class's `@api private` (YARD).
- A private or underscored method marked RDoc's `:doc:` (opening its
  line's comment, or a line of the comment above) is API, as RDoc
  documents it.
- A method under a bare `module_function` is API by its public singleton
  copy, until a bare `private`, `protected` or `public` ends that mode;
  after `public` or `protected` it is an instance method that is API.

An app's own public methods are not entry points: their callers are in the
checkout. Hand-checked on six gems, 36 of 43 rows marked as entry points
were API a user could call.

### Classes, modules and constants

Listed after the methods, named by kind in text (`class Admin::Widget`). One
is a candidate when no constant reference in the checkout resolves to it by
Ruby's own lookup — the views and executable scripts read too. A reference
to `Admin::Widget` is a use of `Admin`, which holds it; one inside its own
body is not a use; a reopened core or gem class is no candidate, nor
anything under the checkout's or a package's `db/`, nor a concern's
`ClassMethods`, reached as its concern is (DEC-720). A class only tests name
is `test-only`, unless a test file defines it: then it is test code in use,
and not listed (DEC-655). One Rails or a library finds by name is
`convention-only`, `convention.by` saying who: a route, a helper, a
Railtie or generator, a policy, serializer or
validator found by its name, a job built from a symbol
(`"Jobs::#{name.camelize}".constantize` with `:send_digest`), an
association, a YAML value or a class-lookup string, a test runner, a
registry hook (DEC-421).

Whatever may still reach it — a constant read on a value
(`self.class::LIMIT`), a namespace whose constants are listed, a factory in
an ancestor, an STI subclass, a gem's routes — is a `caveat`, and grades it
`lower`. So does a lookup of a name computed whole anywhere in the checkout
(`name.constantize`, `Object.const_get(x)`, `klass.const_get(x)`), which may
return any constant; the caveat names where it is. A row with no such
evidence is `clear` (DEC-654). [What it is worth](#what-a-grade-is-worth).

### Example groups' members

A spec's `let`s, `subject`s and group `def`s are read the way RSpec runs
them. A member is read by:

- the group's examples and its nested groups';
- a hook or `let` of an enclosing group, which runs for them (and reads a
  nested group's override where one runs);
- a shared group's body included there, from another file too, through
  `it_behaves_like`, `include_examples` or `include_context`;
- `super` in an override;
- `is_expected` and a bare `should`, for the `subject`;
- what `RSpec.configure` adds: a helper it includes, a shared context it
  includes by metadata (`config.include_context "x", :tag`, or one written
  `shared_context "x", :tag`), a `config.before` hook. Their metadata
  filter is not read, so they may read any group's members.

A member is `shadowed` when, wherever its name is read, an override answers
instead: an outer `let` that each running group redefines. `overridden_by`
names the overrides. `let!` and `subject!` run for every example and are
never listed. A shared group nothing includes by name is a row too
(`kind: shared_group`), with its own `let`s. `trekr --refs FILE:LINE` on any of them lists each read and
where it was found (`from`), from the same reader.

**Every such row is `lower`** (DEC-496), and its `caveat` says why: a name
sent at runtime in its reach (`send("item_#{n}")`), a group macro trekr does
not read, a string of code it evaluates — or, with none of those, that
held-out suites showed `clear` is not earned here. Run the spec file after
deleting one. [What it is worth](#what-a-grade-is-worth).

### Fields per row

- `tier`, and a `reason` in words.
- `confidence`: `clear`, or `lower` with a `caveat`.
- Per tier: `overridden_by` (`shadowed`), `overrides` (`override`),
  `super_from` (`super-only`), `convention.by` (`convention-only`),
  `caller` with its own `tier` (`single-caller`), `cascaded_from`
  when deleting that caller frees it ([above](#cascades-cascaded_from)),
  and `test_sites` (`test-only` methods).
- `visibility`, and `entry_point` when there is one.
- The evidence counted: a method row's `confirmed`/`possible` callers,
  `symbol_refs` (its name as a symbol), `super_refs`, `mentions_by_name`
  (written calls of its name anywhere, capped), `test_refs` (those of its
  calls, symbols and `super`s written in a test; a `test-only` row's are
  all of them) and, when a route reaches it, `route` (`path`, `line`); a
  spec member's `shared_groups_read` and `helpers_read`; a constant's
  `test_refs`, its references from tests.

`summary` counts the rows per tier (`tiers`: `unreferenced`, `test-only`,
`single-caller`, …), per confidence and per kind (`kinds`).

### Gems: `--dead Gemfile`

Handed a `Gemfile`, `gems.rb` or `*.gemspec` at the checkout's root,
`--dead` grades the gems it declares instead of code (DEC-670), one row per
`gem` line (`kind: gem`, `path` and `line` the declaration).
[What it is worth](#what-a-grade-is-worth).

**What is read.** The root manifest, and with it:

- the gemspecs a Gemfile's `gemspec` adds, from the directory its `path:`
  names;
- each file the root Gemfile reads with `eval_gemfile` (`Gemfile.tools`),
  directly or through another: its gems are the root bundle's, each `path`
  the file declaring it, and naming the file grades its own (DEC-820);
- a gemspec in the directory of a path gem the root's lockfile names, or
  with none its Gemfile (rails' `activesupport/activesupport.gemspec`;
  DEC-821), graded against the root bundle and the whole checkout
  (DEC-807);
- a root `Gemfile` that links elsewhere, as the root's.

A gem only the lockfile names is not the app's to remove: it is never a
row, and `summary.transitive` counts them.

**Refused.** Each is a usage error, exit 64 (DEC-771):

| handed | why | the message says |
| --- | --- | --- |
| a lockfile | the manifest declares the gems | pass the manifest, not the lockfile |
| a second bundle's manifest or lock beside the root's (`Gemfile.next`, Appraisal's `gemfiles/*.gemfile`; DEC-806) | its gems were not located | pass the root's Gemfile |
| a manifest in a subdirectory | its own bundle was not located | it is in a subdirectory |
| a manifest beside a code path | code and gems are graded apart | `--dead Gemfile`, then `--dead PATH` |

**What counts as a use.** A gem is used when the checkout:

- names a constant the gem owns;
- requires one of its files;
- calls a method defined by one of the gem's modules (or a class it
  reopens), when no more than three gems and neither the checkout nor Ruby
  define it;
- reopens a class the gem owns; or
- uses a gem only this one brings.

A used gem is not listed. It goes in `referenced` with the first evidence
(`by`: `constant`, `require`, `method`, `declaration`, `dependency`;
`what`, `path`, `line`).

- The checkout here is what the index reads — tracked files and untracked
  ones git does not ignore (DEC-803) — plus the Ruby it does not
  (DEC-770).
- That Ruby: any file with no extension that Prism parses without an error
  (a script, `Dangerfile`, `Vagrantfile`, `fastlane/Fastfile`,
  `Steepfile`; DEC-804) and that holds code, not only bare words (a `TODO`
  reading "remove Tomlrb" is prose; DEC-819); Builder, Arbre, Jb, axlsx and
  Prawn views, Thor files, Capistrano `.cap` tasks, `.irbrc` and `.pryrc`,
  parsed.
- Slim views, whose Ruby — a `-` or `=` line, a tag's `=` output, an
  unquoted attribute or a splat, a `ruby:` block, a `#{}` — is read by its
  words: a constant path, a method name, a `require "…"`; its text, `/`
  comments and quoted attributes are not (DEC-808). A line is split as
  Slim 5 splits it: a whitespace modifier after a tag, a namespaced tag,
  spaces around an attribute's `=`, and a conditional comment's children,
  which are Slim (DEC-815).
- A constant several gems declare is the one whose `lib/` has its file,
  else the one named for it, else the one the others depend on; a module
  none of those picks is no gem's.
- A `require: false` gem's methods count only once a require of it is seen.
- Ruby's methods include those the bundle's own rbs describes under
  `core/`, though core was read from an older one.

**Path gems.** A path gem inside the checkout (a lockfile `PATH` spec,
`path:` or a `path "dir" do` block, not the checkout itself) is graded as a
gem:

- Its files are its own: a use of its constants counts, and its own files
  naming it do not.
- Its code is still the checkout's for every other gem. An engine's use of
  a gem counts, and so does its `require`, once the engine is not itself a
  row (DEC-802). When only the tests use the engine, that is a use by the
  tests (DEC-818).
- Its gemspec is the one in the source whose `name` is the gem's, as
  Bundler picks it (`billing_engine.gemspec` for billing-engine). When the
  name is computed, it is the file named for the gem (DEC-800).
- Two gems found in one directory can't be told apart: each is a row,
  `lower`, "it is found in DIR with …".
- A gem in a `path` block with a source of its own (`git:`, `github:`,
  `source:`) is not a path gem. A block's gem that no gemspec there names
  is not found (DEC-816).

**Tiers**, from least evidence to most:

- `unreferenced` — none of that, and nothing below names it.
- `test-only` — only tests use it, but it is in the default group: move it
  to the test group.
- `convention-only` — no Ruby uses it, but something loads it by name
  (`convention.by`, with `path`, `line` and `text`):
  - `gem`: another gem requires it without depending on it in the lockfile
    (with no lockfile, in its specification), as lograge does with
    `require "logstash-event"`. The requirer is a gem of the bundle that is
    not itself a row, or a row such a gem requires; a row's own require only
    lowers it. A `require_relative` loads the requirer's own file;
  - `symbol`: a symbol naming it to the gem it plugs into
    (`provider :saml`);
  - `config`: a config, CI file, Procfile or a Ruby string in `config/`
    names it, an adapter `database.yml` names (`postgresql` for pg), or a
    file in `config/` is named for it (`config/puma.rb`). A name after a
    `.` is a path's, not the gem's (`.yardoc` names no yard);
  - `command`: a line runs one of its commands.

**Caveats.** Each row is `clear`, or `lower` with a `caveat` naming what
may use it with nothing naming it, and what would settle it:

- not installed (its files were not read), or a path or git gem;
- declared for some platforms only;
- a Railtie or Engine, which runs at boot through `Bundler.require`;
- a method it defines on a class it does not own, a module it mixes into
  one, or an `on_load` hook (a class it declares beside a constant of the
  same name, `class PReadFile` by `PReadFile = ::File`, is its own);
- a command its specification installs that nothing read runs (one that
  lists none installs none, whatever `bin/` holds; a list written any way
  but one literal array, `s.executables << "x"`, is read from `exe/` or
  `bin/`);
- a compiled extension;
- rake tasks or generators it ships;
- a name that makes it a plugin of a bundled gem (`rubocop-rails`);
- the development group;
- a gem it brings that the app uses, which others bring too.

**Fields.** Per row: `version` and `gem_dir` (`null` when not installed),
`groups`, `bundler_require` (`name`, `paths`, or `off` for
`require: false`), `needed_by` (gems that depend on it, so removing the
line keeps it in the bundle; not the gem whose gemspec declares it, by its
`name` or, computed, the file's), `executables`, `test_refs`. A gem line
several manifests reach is one row (DEC-822).

`summary` counts `declared`, `referenced` (gems found in use that are not
rows: a `test-only` row is not one), `transitive`, the rows per tier and
per confidence; text lists `referenced` with `--explain`.
`summary.unread_evals` lists each `eval_gemfile` not followed, whose gems
are not graded: `file` (the one writing it), `eval` (as written) and `why`
— `computed`, a path trekr cannot read without running Ruby (a literal,
`File.expand_path(literal, __dir__)`, `File.join(__dir__, literal)` and
`File.expand_path("../x", __FILE__)` are read), or `missing`, no such file;
absent when every eval was followed. Text says each on a line of its own
(DEC-876).

## Classes split by their superclass

A name declared with two different superclasses is two classes (DEC-072) —
common in a monorepo, where a test fake `Post = Struct.new(…)` sits beside
`class Post < ActiveRecord::Base`. Ruby would refuse to load both, so trekr
keeps them apart: `--ancestors Post` answers `status: ambiguous` with one
entry per variant under `variants` (`ancestors`, `definition`,
`unresolved_ancestors`), and the top-level chain is just `[Post]`; the bare
card `trekr Post` answers the same way. Other queries pick the variant
nearest the file asking; when none is nearest, `--def` is `ambiguous` and
`--refs` tiers the site `possible`.

## A model's table

A model's card (`trekr Post --json`) has `table`: `name`, `inherited_from`
(the base class whose table a single-table-inheritance subclass shares),
`abstract`, and — when the schema dump has it — `path` + `line` of the table,
`primary_key`, `columns` (`name`, `type` as the dump spells it, `class` its
reader returns, `null`, `default`, `line`) and `indexes` (`columns`,
`unique`), and `view` — for a view, its select list's columns,
`unread_columns` counting those it names none for. From `db/schema.rb`, or
`db/structure.sql` when the app dumps SQL; a table in another schema than the
app's is `schema.table`. A class that is not a model has no `table`.

## Ruby core

Core is the checkout's Ruby's: the one `.ruby-version`, `.tool-versions`,
mise or the Gemfile names, else a fallback that meets its
`required_ruby_version` — the lockfile's, the version manager's,
`$GEM_HOME`'s, the `ruby` on `PATH`, the highest installed — read from the
`rbs` gem bundled with it. No Ruby found, or one without rbs, means no core:
`puts` and `"x".upcase` are `residue` whose reason says Ruby core is not
indexed for this checkout. `trekr --index` and `--status` name the Ruby and
rbs used, a named Ruby that is not installed (`ruby_not_found`), and
requirements no installed Ruby meets (`ruby_unmet`).

A core site is the owner's stub, written beside the database: `path:
"String.rb"` with `root` a directory per Ruby's signatures,
`trekr.core/rbs-<version>-<key>/` next to `trekr.db`, so it opens like any
other site.

## `--symbols`

One row per definition in the file: `name`, `kind`, `singleton`,
`visibility`, `line`/`col`/`end_line`, `nesting`; `params`, Ruby's own
`Method#parameters` words (`req`, `opt`, `rest`, `keyreq`, `key`,
`keyrest`, `block`…); `via`, the macro that made it (`attr_reader`,
`alias_method`…); `target`, what it stands for as written (an alias's
method, `Bar = Foo`'s `Foo`); and `sig_returns`, the class an inline Sorbet
`sig` says it returns.

## `--index` and `--status`

`--index`'s `indexed` counts the pass's work, not the checkout: `files`,
`blobs` (distinct contents), `parsed` (contents this machine had never seen;
0 on a reindex with no edits), and the `defs`, `refs` and `calls` read from
them.

`ruby` (top level of `--index`, per checkout in `--status`) is the Ruby the
checkout runs on: `version`, `root` (its stdlib's) and `how` it was chosen —
`named` by `.ruby-version`/`.tool-versions`/mise/Gemfile; else a fallback,
the first carrying rbs that meets the gemspec's `required_ruby_version`:
`lockfile` (`RUBY VERSION`), `manager` (`$RBENV_VERSION`, a version file
above, a manager's global), `gem_home`, `path`, `highest`, `only`; or `kept`
from the last index. `fallback` is true for all but `named`; `null` when none
was found.

`gems`:

- `missing` — gems the lockfile wants and disk lacks: a hole in every answer
  that would have come from them.
- `unlocated` — git and path gems that were not indexed from their source,
  each with its `why` (a git checkout not where bundler puts it, a path
  outside the checkout). Without a lockfile, a git gem with no checkout to
  take is indexed at the installed release that meets its requirements, when
  one is, and listed here too, its `why` saying so (DEC-879).
- `unread_evals` — without a lockfile, each `eval_gemfile` not followed,
  as `--dead Gemfile`'s `summary.unread_evals` writes it: its gems are
  not indexed.
- `resolved_from` — `lockfile`, or `declared` when there was no
  `Gemfile.lock` and the gemspecs' and Gemfile's dependencies were resolved
  to the highest installed versions instead; absent, no gem was indexed.
- `picked` — each gem found as `name version`. Without a lockfile, `ruby`
  says whose Ruby the picks came from and `unread` the requirements trekr
  could not read (the highest installed was taken).
- `found` — gems the lockfile names that are on disk; of those
  `from_git` (bundler's git checkouts), `from_path` (path gems inside the
  checkout, indexed with it), `from_stdlib` (default gems whose code is the
  stdlib), and `already_indexed` (known already, so free); `files` across
  them.
- `other_ruby` — gems found only for another Ruby. Gems are looked for in the
  checkout's own Ruby's directories first.
- `stdlib` — the Ruby standard library indexed with the checkout: `root`, the
  `ruby` it belongs to and how it was chosen, and `hidden`: the default gems
  (json, logger, uri…) the bundle has its own copy of, whose stdlib files
  this app does not see, and `files`. `rbs.chosen` says whose signatures
  serve it: `bundled` with that Ruby, the highest `installed`, or an
  `other` Ruby's. Absent when the checkout names no gem and no Ruby.
  Dev tooling (irb, rdoc, bundler's internals) is not indexed, so a question
  about it is residue. A stdlib class that is partly C (`Pathname`,
  `Monitor`, `OpenSSL::*`) answers residue naming its compiled extension for
  a method its Ruby lacks, rather than "no such method".

`trekr --status` shows the checkout you are in (`repo`, `files`, `blobs`,
`indexed_at` in Unix seconds), its gems counted (`gems: {count, indexed,
files}`, with `missing`, `unlocated` and `unread_evals` as `--index`
writes them when the bundle has a hole, read again as the next index would find it), its Ruby's stdlib (`stdlib: {root, files, hidden}`), how many
other checkouts are indexed (`others`, with `repos`), and `totals` over the
whole store (`blobs`, `defs`, `const_refs`, `calls`); `--status --all`
lists every checkout, each with `kind` (`repo`, `gem` or `stdlib`).
`--status` only reports, never indexes: a checkout nobody indexed is
`status: not_indexed`, exit 2 — `checkouts` is empty and `others` counts the
rest. Outside any checkout the repos are listed.

A reindex with nothing changed parses nothing (~60 ms on a 3k-file repo), and
a second worktree of the same repo costs nothing — facts are keyed by git
blob.

## `--gc` and storage

`--status`'s `storage` is the index's size (DEC-650):

- `bytes`, its pages as SQLite counts them, and `free_bytes`, those free
  inside it, both read in one snapshot with `cleanup`;
- `stale_snapshots` (`files`, `bytes`), tree snapshots no checkout's current
  key names (a query's tree over its edits is never counted: only `--gc`
  removes those);
- `reclaimable_bytes`, the two together: what a compaction would return.

`cleanup` says what the background cleanup last did, every field `null`
until it has run:

- `checked_at`, when it last measured the store (a new store is stamped at
  creation);
- `ran_at` and `reclaimed_bytes`, its last pass and what that returned to
  the disk;
- `skipped`, why the last pass kept free pages it meant to hand back:
  `no_room` (a full rewrite would not fit twice on the disk) or `busy`.

`retained` counts the versions of files no checkout maps any more (left by
an edit, a branch switch, `--drop` or a query's draft), kept so a switch
back reuses them (DEC-651):

- `blobs`, stamped or not (one an older trekr unmapped is stamped by the
  next background pass);
- `expired`, those unmapped longer than the retention, which the next
  `--gc` or background cleanup collects;
- `retention`, in seconds, `null` for `never`.

`--gc` collects a checkout by `--older-than`, and a version no checkout maps
by the retention (DEC-651). Its fields:

- `older_than` and `retention`, in seconds (`retention` is `null` for
  `never`); `dry_run` says nothing was removed.
- `checkouts`, each checkout collected (`repo`, `kind`, `reason`:
  `vanished` or `unclaimed`, `last_seen`), and `files`, their file-map rows.
- `blobs`, every blob removed (a collected checkout's, and `expired`: those
  unmapped longer than the retention), and `facts`, the rows they held.
- `signatures`, the Rubies' signatures no surviving stdlib is served with.
- `indexes`: `--gc` first adds any index this version's schema has and the
  store lacks, and names them here (empty when none). It does so on a dry
  run too: without the index a collection scans a table per blob, and
  adding one removes nothing.
- `reclaimed_bytes`, what it returned to the disk: the files it removed
  (`snapshots`, `overlays`, `core_files`, `kept`), and the index file's
  shrink.
- `freed_bytes`, the pages its deletions freed inside the file. When it
  freed any and the file's free pages come to 32 MB, `--gc` hands them back
  before it reports (DEC-878).
- `free_bytes`, every free page left, which later writes reuse and the
  background cleanup compacts once it is worth it; `db_bytes`, the index
  file's size after.

## `--usage`

`--usage --json` (or `--ndjson`) emits the stored rows themselves, one per
day and combination: `day`, `surface` (`cli` or `lsp`), `feature` (the
command or LSP operation), `flags` (the knobs reached for, never their
values), `origin` (who called), `outcome` (`hit`, `uncertain`, `empty`,
`not-indexed`, `cancelled`, `error:<kind>`), `latency` (a decade bucket,
`<1ms` … `10s+`), `cold` (an LSP session's first request), `version` (the
counting trekr's, `+dev` for a cargo build, `""` for older rows) and
`count`; none, exit 1. [ARCHITECTURE](ARCHITECTURE.md#usage-counts) says what each
holds.
