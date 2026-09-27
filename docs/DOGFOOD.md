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
| `spec/models/user_spec.rb:1:7` `describe` | polyid | RSpec's `describe` (made by `define_singleton_method` in `rspec/core/dsl.rb`) | minitest's `Kernel#describe`, **resolved, confidence 1** | the first line of every spec: `RSpec` has no `describe` the index sees, so the lookup falls to `Kernel`, which minitest (bundled through activesupport) patched. 7 of polyid's 13 confidently wrong gold sites |
| `spec/models/user_spec.rb:54:33` `find` | polyid | `PolyId::Model::ClassMethods#find` | ActiveRecord's `find`, **resolved** | same `on_load` include as `id_for` below; 6 of polyid's 13 confidently wrong gold sites |
| `spec/accord/types/decimal_spec.rb:10:7` `expect` | accord | `RSpec::Matchers#expect` | residue, 8 guesses, Minitest's first | Every implicit call in a spec: the block's `self` is an example group. 40% of all click misses, and the largest gold-set bucket |
| `spec/accord/types/decimal_spec.rb:10:14` `type` | accord | the `subject(:type)` block on line 6 | residue | `let`/`subject` define a method named by their symbol |
| `spec/accord/types/decimal_spec.rb:10:36` `to` | accord | `RSpec::Expectations::ExpectationTarget#to` | residue, truth not among the guesses | follows once `expect` resolves and carries a return type |
| `spec/models/cache_spec.rb:81:12` `id_for` | polyid | `PolyId::Model::ClassMethods#id_for` | empty: `User` known, nothing defines it | mixed into `ActiveRecord::Base` by `ActiveSupport.on_load(:active_record) { include PolyId::Model }` |
| `spec/accord/money_spec.rb:114:21` `parse` | accord | `Accord::Schema.parse` | empty: `Class#parse` not found | `schema = Class.new(Accord::Schema) { … }` is a subclass, typed as a `Class` instance |
| `spec/flipper/adapters/rollout_spec.rb:28:52` `new` | flipper | `Class#new` on the struct class | empty: `Struct#new` not found | `Struct.new(:id)` returns a class, typed as a `Struct` instance |
| `spec/dsl_spec.rb:16:43` `per` | berater | `Integer#per` in `refine Integer` (`lib/berater/dsl.rb`) | empty: `Integer` has no `per` | refinements are not modelled |
| `lib/graph_weaver/codegen/emit.rb:45:7` `@variable_inputs` | graph_weaver | the write in `GraphWeaver::Codegen`, which includes `Emit` | hover "no assignment found", definition empty | an ivar read in a mixin is written by the includer (DEC-064 looks only in the class chain) |
| `lib/graph_weaver/inflect.rb:18:32` `empty?` | graph_weaver | `String#empty?` | no name at this position | `&:empty?` is not recorded as a call |
| `lib/graph_weaver/client.rb:10:1` `require_relative` | graph_weaver | `Kernel#require_relative` | residue, 3 guesses | a top-level call outside any block is on `main`, an `Object`; the call does not record whether it is in a block |
| `lib/network_resiliency/power_stats.rb:84:23` `percentile` | network_resiliency | `percentile` in the same class | residue | `alias_method :p, :percentile`, `method(:x)`, `send(:x)` on an implicit receiver name a method on `self` |
| `lib/network_resiliency/syncer.rb:10:14` `synchronize` | network_resiliency | `Mutex#synchronize` | residue, 8 of 25 | `LOCK = Mutex.new`: a value constant is untyped (DEC-082, not done) |
| `lib/accord/field.rb:166:7` `nested_schema` | accord | `Accord::Fields::Array#nested_schema`, the override the runtime object had | `Accord::Field#nested_schema`, **resolved, confidence 1** | a call on `self` whose method a subclass overrides; the answer could name the override as a competitor. Same shape: graph_weaver `install_generator.rb:164:11` `append_to_file` and `railtie.rb:566:19` `env`, overridden by a spec's stand-in |
| `lib/graph_weaver/tasks.rb:372:3` `namespace` | graph_weaver | `Rake::DSL#namespace` | residue | a rake task file's blocks; honest until blocks carry a `self` |

## Fixed

| position | repo | meant | got | fixed in |
|---|---|---|---|---|
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
