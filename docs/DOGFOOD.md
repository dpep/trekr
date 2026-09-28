# Dogfood misses

Positions where trekr, used on real Ruby, missed the definition, answered only
with guesses, or answered wrong. Each entry: the position, the repo, what was
meant, what came back. A fixed entry names its commit and moves into the
testbed (`tests/testbed/`) or a unit test, and out of this list at the next
cleanup.

Where they come from: the editor's own miss log (`trekr --usage --misses`,
DEC-083), `script/clicks.py` replaying clicks over copies of real repos, and
`script/gold_gem.sh` scoring a gem's traced spec suite. Paths are relative to
the repo named; lines and columns are 1-based, as `--def` takes them.

## Open

| position | repo | meant | got | notes |
|---|---|---|---|---|
| `spec/accord/money_spec.rb:114:21` `parse` | accord | `Accord::Schema.parse` | empty: `Class#parse` not found | `schema = Class.new(Accord::Schema) { … }` is a subclass, typed as a `Class` instance |
| `spec/flipper/adapters/rollout_spec.rb:28:52` `new` | flipper | `Class#new` on the struct class | empty: `Struct#new` not found | `Struct.new(:id)` returns a class, typed as a `Struct` instance |
| `spec/dsl_spec.rb:16:43` `per` | berater | `Integer#per` in `refine Integer` (`lib/berater/dsl.rb`) | empty: `Integer` has no `per` | refinements are not modelled |
| `lib/graph_weaver/codegen/emit.rb:45:7` `@variable_inputs` | graph_weaver | the write in `GraphWeaver::Codegen`, which includes `Emit` | hover "no assignment found", definition empty | an ivar read in a mixin is written by the includer (DEC-064 looks only in the class chain) |
| `lib/graph_weaver/client.rb:10:1` `require_relative` | graph_weaver | `Kernel#require_relative` | residue, 3 guesses | a top-level call outside any block is on `main`, an `Object`; the call does not record whether it is in a block |
| `lib/network_resiliency/syncer.rb:10:14` `synchronize` | network_resiliency | `Mutex#synchronize` | residue, 8 of 25 | `LOCK = Mutex.new`: a value constant is untyped (DEC-082, not done) |
| `spec/accord/types/decimal_spec.rb:5:1` `describe` | accord | RSpec's `describe`, exposed on `main` | residue | a bare top-level `describe` is `main`'s; the stub states only `RSpec.describe` |
| `lib/graph_weaver/tasks.rb:372:3` `namespace` | graph_weaver | `Rake::DSL#namespace` | residue | a rake task file's blocks; honest until blocks carry a `self` |
| `spec/transport_endpoint_spec.rb:96:35` `http_response` | graph_weaver | `def http_response` in the `shared_context` of `spec/support/raw_http_server.rb` | residue, the shared context's method offered first | the context is followed now (DEC-092); the call is in the block of `serving`, the context's own helper, which DEC-084 does not vouch runs on the example |
| `lib/graph_weaver/inflect.rb:18:32` `empty?` | graph_weaver | `String#empty?` | residue, `empty?`'s definitions offered | `&:empty?` is recorded now (DEC-094); `split` returns an `Array` in core's stubs, with no element type |
| `be_empty`, `be_valid` on a `let` whose block is a chain, or an untyped local | accord, berater, meddleware, polyid | the subject's predicate | residue naming the predicate-matcher rule, the predicate's definitions offered | the rule answers when the subject is typed (DEC-090, DEC-096); `subject { [].pluck(0) }` and `expect(input)` are not |

## Fixed

| position | repo | meant | got | fixed in |
|---|---|---|---|---|
| `spec/models/user_spec.rb:54:33` `find` | polyid | `PolyId::Model::ClassMethods#find` | ActiveRecord's `find`, **resolved** | ecb44b5 (testbed 071) |
| `spec/models/cache_spec.rb:81:12` `id_for` | polyid | `PolyId::Model::ClassMethods#id_for` | empty: `User` known, nothing defines it | ecb44b5 (testbed 071) |
| `lib/network_resiliency/power_stats.rb:84:23` `percentile` | network_resiliency | `percentile` in the same class | residue | ca04313 (testbed 059) |
| `include_attrs`, `match_attrs` | webmock-twirp | the group's `matcher :include_attrs` | residue | dd4873d (testbed 057) |
| `be_empty`, `have_key` on a typed subject | — | the subject's `empty?`, `has_key?` | residue, "nothing indexed defines this name" | ac53bc6, ab31ba8, 664f67f (testbed 056, 062) |
| `spec/models/user_spec.rb:1:7` `describe` | polyid | RSpec's `describe` | minitest's `Kernel#describe`, confidence 1 | 15c740c (testbed 051) |
| `spec/accord/types/decimal_spec.rb:10:7` `expect` | accord | `RSpec::Matchers#expect` | residue | 4dedef0, 1d0b84e, 15c740c (testbed 046, 050, 051) |
| `spec/accord/types/decimal_spec.rb:10:14` `type` | accord | the `subject(:type)` on line 6 | residue | 7ed44b8 (testbed 048) |
| `spec/accord/types/decimal_spec.rb:10:36` `to` | accord | `ValueExpectationTarget#to` | residue | 15c740c, e4fce7c (testbed 051) |
| `lib/accord/field.rb:166:7` `nested_schema` | accord | the override the object had | `Field#nested_schema`, confidence 1; now `ambiguous`, naming the overrides | f15c676 (testbed 054) |
| `lib/graph_weaver/codegen/emit.rb:8:20` `Codegen` | graph_weaver | the class being opened | nothing; a click on `GraphWeaver` answered `Codegen` | f2d2b26 |
| `Float::INFINITY`, `Process::CLOCK_MONOTONIC`, `Thread::Mutex`, `Errno::ENOTCONN`, `Time.utc`, `__dir__`, `Module#using` | berater, network_resiliency, graph_weaver, accord, amenable | the core definition | residue or empty | ff6f12a (testbed 045) |
| `lib/graph_weaver/codegen/emit.rb:41:38` `visit` | graph_weaver | `visit = lambda do …` on line 32 | "a variable with no write in reach" | 5b5baeb |
| a hover on a residue call | — | counted `uncertain` in `--usage` | counted `hit` | e482710 |

## The user's own editor, 2026-09-27

Of the 139 definition requests VS Code sent into graph_weaver that came back
empty, 46 were on comment lines, where nothing should answer (holding ⌘ and
moving the mouse sends a request per word, two per gesture). 28 were on
`require` lines, all within the 04:00 UTC hour on build 0.1.5; this build
answers those lines at every column but the space. 6 were in the single
`core.rb` an earlier build wrote. Of the 59 in code, 46 fall in that same
hour; replayed with this build, the names on those lines still empty are the
`visit` (fixed), `@variable_inputs` and `&:empty?` entries above, symbols and
hash keys, and the fixed `Codegen`. The log had file and line but no column —
which is what DEC-083 adds.
