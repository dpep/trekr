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
| `status: not_indexed`, exit 2 | only with `--no-index`: nobody has indexed this checkout. The answer names the root and the command. |
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

Through the LSP, findReferences is capped (1,000 by default, confirmed
callers first) and the cut is only announced to editors. Exactly that many
locations means the list was cut.

## `--dead`

`--dead` grades candidates for deletion; it never calls anything dead. It
checks every method defined in scope against references from the whole
checkout (not other indexed repos, so the answer does not depend on them);
then a spec's `let`s, `subject`s, group `def`s (`kind: let|subject|method`,
with `group`) and shared groups (`kind: shared_group`); then every class,
module and constant (`kind: class|module|constant`) no constant reference
resolves to.

Measured against a year of discourse's history, `unreferenced` candidates
were deleted 19.8 % of the time against a 19.0 % base rate — no lift
([BASELINE](BASELINE.md#dead-code-candidates-validated-against-git-history-session-36)). Hence the grading, and why trekr never says
"dead".

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
  (`before_action :x`, `if: :x`, `with: :x`), a controller action only a
  route reaches (`config/routes.rb` is read), or a method a library calls by
  a name it builds: ActiveModel::Serializers' `include_<attr>?`, a Thor
  command (`convention.by` says which).
- `super-only` — reached only by `super` from its overrides (`super_from`).
- `single-caller` — one reference: an inlining candidate. `caller` says
  where, with its own `tier`: whether it certainly reaches the method.

One pass, no cascade: a method whose only caller is itself a candidate is
`single-caller`, and its `reason` says the caller is a candidate. An
`initialize` is reported only when nothing constructs its class.

### Confidence and caveats

Each row is `clear` or `lower` confidence, and says why in words; a `lower`
row's `caveat` names what may still reach it. Any tier other than `override`
that overrides a method is `lower`, and so is a `single-caller` row whose
caller is `possible`.

Three callers trekr cannot see grade a row `lower`: a Haml, Slim, Jbuilder
or Builder template that writes the name ("named in a view (…), which is not
read"; ERB and RABL templates are read, and their calls counted), a Ruby
script with no extension (`exe/tool`, `bin/x`: "named in a script (…), which
is not read"), and Ruby or Rails calling a protocol hook by name —
`marshal_load`, `to_partial_path`, `each`, a job's `perform` ("a hook …
calls by name"; DEC-315). So does a symbol in the method's own file that
trekr does not read as a call (`only: [:archive]`), and a controller action
when a route is built at runtime (DEC-343, DEC-344). And every other way in
the row can name: a name of its shape the checkout builds
(`:"report_#{type}"`, or a string its class is sent,
`Q.new.public_send("list_#{kind}")`; DEC-655) or a computed name sent to its
class (`Mailer.public_send(type)`), a writer `assign_attributes` reaches, an
ancestor trekr has not indexed, a gem of the bundle that calls the name, a
helper another helper calls (DEC-363–368, DEC-370), a YAML config that
writes the name (`generator: pay_schedule_resources`,
`Pkg::Sanitizer.clean_uid`; locale files aside, DEC-402), and a file a
generator wrote (`linguist-generated`, a `generated/` directory, a "DO NOT
EDIT" header; DEC-403).

`clear` on an `unreferenced` row means none of those was found. Measured by
hand on mastodon and discourse (DEC-372), such rows were truly dead 79 % and
100 % of the time where the rules were fitted, and 89 % on discourse rows
held out; on forem, an app no rule was fitted on, 55 of 70 (79 %) — a
Pundit, an algoliasearch callback, a serializer block made most of the
rest (DEC-450). On code unlike an app's (`lib/`), less — read DEC-372 before
trusting it on a CLI or a library.

`visibility` (`public`, `protected`, `private`) says how complete the
evidence is: a private candidate's is complete, a public one's is not.

### `test-only`

A method every call, symbol and `super` of which is written in a test is
`test-only`, however many there are; the `reason` says how many and where
the first is, and `test_refs` counts them. A test is a file under a
`spec/`, `test/` or `tests/` directory at the checkout's root or a nested
package's (`plugins/chat/spec/`), not one inside `lib/` or `app/`
(rack-test's library is `lib/rack/test/`). A route, a library's convention,
an override, a `super` into a method trekr has not indexed, a protocol hook,
or a script that writes the name outranks the tests: such a method keeps the
tier it would have without them. A spec's own helper is never `test-only`.

Graded as the other method tiers are: measured by hand on mastodon and
discourse (DEC-652), a `clear` one was reached only by tests 21 times in 26
(81 %), the misses all names built at runtime
(`public_send("list_#{action}")`), which are now read (DEC-655); a `lower`
one 7 in 25. On a library, a public `test-only` method is usually its API,
so in a gem it is graded `lower` as an entry point (below).

### Entry points

A row reached from outside the repo, where no caller can be seen, keeps its
tier, is graded `lower`, and carries `entry_point` (DEC-657); absent
otherwise. Its `caveat` names the evidence that would settle it.

| `entry_point` | what it is | settle it with |
| --- | --- | --- |
| `api` | a public method, class or module in `lib/` of a checkout with a root `*.gemspec`, unless marked internal (`:nodoc:` on its line; `@api private`, `@private`, `Private:` or `Internal:` in the comment above), named with a leading underscore, or held by a `private_constant` | its dependents, or usage data |
| `route` | a routed controller action, or a controller only routes name | request logs or APM (e.g. Datadog) |
| `job` | a job class nothing outside the tests names | job metrics or logs |

A mark reaches as far as its tool reads it: a `:nodoc:` or TomDoc mark on
a class hides that class and the methods its body defines, not the classes
nested in it (RDoc); `:nodoc: all`, YARD's `@api private` and `@private`,
and a `private_constant` hide everything nested too; a method's own `@api
public` overrides its class's `@api private` (YARD). A method under
`module_function` is public by its singleton copy.

An app's own public methods are not entry points: their callers are in the
checkout. Hand-checked on six gems, 36 of 43 rows it moved from `clear`
were API a user could call.

### Classes, modules and constants

Listed after the methods, named by kind in text (`class Admin::Widget`). One
is a candidate when no constant reference in the checkout resolves to it by
Ruby's own lookup — the views and executable scripts read too. A reference
to `Admin::Widget` is a use of `Admin`, which holds it; one inside its own
body is not a use; a reopened core or gem class is no candidate, nor
anything under the checkout's or a package's `db/`. A class only tests name
is `test-only`, unless a test file defines it: then it is test code in use,
and not listed (DEC-655). One Rails or a library finds by name is
`convention-only`, `convention.by` saying who: a route, a helper, a
concern's `ClassMethods`, a Railtie or generator, a policy, serializer or
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
evidence is `clear` (DEC-654): on gems held out, an `unreferenced`, clear
row was truly unused 25 times in 26, a `test-only` one 21 in 26. An app
nearly always has such a lookup, so its rows stay `lower` — forem's were
truly unused 19 times in 50 (DEC-450). Read a constant row as a lead to
check, not a verdict.

### Example groups' members

A spec's `let`s, `subject`s and group `def`s are read the way RSpec runs
them: by the group's examples and its nested groups', by a hook or `let` of
an enclosing group (which runs for them, and reads a nested group's override
where one runs), by a shared group's body included there — in another file,
through `it_behaves_like`, `include_examples` or `include_context` — by
`super` in an override, by `is_expected` and a bare `should` for the
`subject`, and by what `RSpec.configure` adds — a helper it includes, a
shared context it includes by metadata (`config.include_context "x", :tag`,
or one written `shared_context "x", :tag`), a `config.before` hook — whose
metadata filter is not read, so they may read any group's. `shadowed` is a
member every read of whose name an override answers where the read runs —
an outer `let` each running group redefines; `overridden_by` names them.
`let!` and `subject!` run for every example and are never listed. A shared
group nothing includes by name is a row too (`kind: shared_group`), its own
`let`s with it. `trekr --refs FILE:LINE` on any of them lists each read and
where it was found (`from`), from the same reader.

**Every such row is `lower`** (DEC-496), and its `caveat` says why: a name
sent at runtime in its reach (`send("item_#{n}")`), a group macro trekr does
not read, a string of code it evaluates — or, with none of those, that
held-out suites showed `clear` is not earned here.

Measured against what ran (DEC-492 to DEC-496): mastodon's suite, with
every `let`, `subject` and group `def` traced, then scored. Every `let` and
`subject` row trekr listed never ran in a group that passed an example, and
trekr listed 86 of the 96 `let`s that never did; 20 of 20 of discourse's
rows, sampled and read by hand, were unused too. But each suite held out
from the rules found a way in they did not yet read — a shared context
included by metadata, a shared group written inside a module, a string
`eval`ed — and a row with no caveat was truly unused 79 %, 47 % and 0.2 %
of the time on three such batches before each was read. So every row is
`lower`: run the spec file after deleting one.

### Fields per row

- `tier`, and a `reason` in words.
- `confidence`: `clear`, or `lower` with a `caveat`.
- Per tier: `overridden_by` (`shadowed`), `overrides` (`override`),
  `super_from` (`super-only`), `convention.by` (`convention-only`),
  `caller` with its own `tier` (`single-caller`).
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
- `unlocated` — git and path gems that were not indexed, each with its `why`
  (a git checkout not where bundler puts it, a path outside the checkout).
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
files}`), its Ruby's stdlib (`stdlib: {root, files, hidden}`), how many
other checkouts are indexed (`others`, with `repos`), and `totals` over the
whole store (`blobs`, `defs`, `const_refs`, `calls`); `--status --all`
lists every checkout, each with `kind` (`repo`, `gem` or `stdlib`).
`--status` only reports, never indexes: a checkout nobody indexed is
`status: not_indexed`, exit 2 — `checkouts` is empty and `others` counts the
rest. Outside any checkout the repos are listed.

`storage` is the index's size (DEC-650): `bytes`, its pages as SQLite
counts them; `free_bytes`, those free inside it — both read in one snapshot,
with `cleanup`; `stale_snapshots`
(`files`, `bytes`), tree snapshots no checkout's current key names (a
query's tree over its edits is never counted: only `--gc` removes those); and
`reclaimable_bytes`, the two together — what `--gc --vacuum` would return.
`cleanup` says what the background cleanup last did: `checked_at`, when it
last measured the store (a new store is stamped at creation), `ran_at` and
`reclaimed_bytes`, its last pass and what that returned to the disk, and
`skipped`, why the last pass left free pages it meant to hand back —
`no_room` (a full rewrite would not fit twice on the disk) or `busy` — all `null`
until there is one.
`retained` counts the versions of files no checkout maps any more — one an
edit, a branch switch or `--drop` left, or a query's draft — kept so a switch
back reuses them (DEC-651): `blobs`, stamped or not (one an older trekr
unmapped is stamped by the next background pass); `expired`, those unmapped
longer than the retention, which the next
`--gc` or background cleanup collects; and `retention`, in seconds, `null`
for `never`.

`--gc` collects a checkout by `--older-than` (`older_than`, seconds) and a
version no checkout maps by the retention (`retention`, seconds or `null`
for `never`; DEC-651). `blobs` is every blob it removed — a collected
checkout's, and `expired`: those unmapped longer than the retention.
`--gc` first adds any index this version's schema has and the store lacks —
on a dry run too, since without it a collection scans a table per blob, and
an index removes nothing — and names them in `indexes` (empty when none).
It reports `reclaimed_bytes`, what it returned to the disk: the files it
removed (`snapshots`, `overlays`, `core_files`, `kept`) and, with `--vacuum`,
the pages the index file gave up — counted in pages, since a reader can hold
off the file's own shrink until the log is folded back. `freed_bytes` is the pages its deletions freed
inside the file, which later writes reuse; `free_bytes` is every free page
left in it — `0` after `--vacuum` — which the background cleanup compacts
once it is worth it.

A reindex with nothing changed parses nothing (~60 ms on a 3k-file repo), and
a second worktree of the same repo costs nothing — facts are keyed by git
blob.
