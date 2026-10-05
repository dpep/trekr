# Changelog

## Unreleased

- **A group whose examples come from a mixin's `included` hook runs them**
  (#10). A nested group with `let(:scheduler_class)` and `include
  Assertions`, whose `self.included` writes the examples with `class_eval`,
  was seen as running none, so an outer group's hook or `def` reading
  `scheduler_class` did not count and the `let` was listed unreferenced.
- **A call through a local holding `self` is a call on `self`** (#10).
  `this = self`, then `this.part_class` inside `Class.new do … end`, now
  resolves as an implicit call where `this` was assigned: `--def` lands on
  the method or `let`, and `--dead` no longer lists a `let` read only that
  way as unreferenced. Only a local whose one assignment is `self`.
- **`--dead` counts a module that mixes itself into another class** (#12).
  `Widget.prepend self` (or `include`, `extend`, `send(:prepend, self)`) in
  a module's body was reported `unreferenced`, though the class holds it as
  surely as `prepend Disabling` written in its body would; the module is
  now used by that class, and by the tests when the call is in one — the
  receiver may be a gem's class trekr has not indexed. The patch's methods
  already overrode the class's, and `--ancestors` already showed it.
- **A failed write no longer leaves the store stuck mid-transaction.** A
  batch of writes whose commit failed — or, in the editor's server, that hit
  a bug — left its transaction open, so later writes on that connection
  joined one that would never commit. It now rolls back.
- **`TREKR_PROFILE=0` is off everywhere.** One of the tree build's reports
  (`fixpoint:`) treated any value, `0` included, as on.
- **The editor recovers when building a checkout's view of the code fails
  partway.** While an index was still filling a checkout, a background
  tree build that crashed was never retried until the index moved on, so
  answers stayed partial and said so. The next request now builds it again.
- **A bug in one editor request no longer takes the language server down.**
  A panic while answering a request or reading an edit — an odd half-typed
  buffer, say — ended `trekr --lsp`, and the editor stopped restarting it
  after a few tries. That request now gets an error, the panic is in the
  server log, and the session keeps answering.
- **A checkout that names no Ruby still gets core** (#9). Without a
  `.ruby-version` (most gems), trekr used `$GEM_HOME`'s or `$PATH`'s Ruby, or
  the only one installed — and with several installed and none of those,
  indexed no core at all. It now falls back, in order, to the lockfile's
  `RUBY VERSION`, the version manager's current choice (`$RBENV_VERSION`,
  `$ASDF_RUBY_VERSION`, `$MISE_RUBY_VERSION`, chruby's `$RUBY_ROOT`, a
  version file above the checkout), `$GEM_HOME`'s, `$PATH`'s, rbenv's,
  mise's or rvm's global, then the highest installed — taking the first
  that carries an rbs gem and meets the gemspec's `required_ruby_version`
  (and a Gemfile `ruby "~> 3.4"`). `.tool-versions` and mise's `[tools]
  ruby` now count as the checkout naming its Ruby. `--index` says
  `Ruby 4.0.6 (fallback: …)` and what it passed over; `--status` gains a line
  per checkout naming its Ruby; the `ruby` object's `how` gains `lockfile`,
  `manager` and `highest`, and a `fallback` boolean. A checkout whose
  lockfile names its Ruby moves to it on the next `--index`.
- **Visibility follows more of Ruby's rules.** `private :x` after `def x`
  now makes `x` private in `--symbols` and completion (class methods too,
  inside `class << self`); an alias takes its original's visibility instead
  of always being public; a `define_method` in a `private` section is
  private; and `private_class_method [:a, :b]` or `*%i[a b]` names each.
- **`--refs` at a call trekr cannot place answers instead of failing.** On
  `x.build` with an untyped receiver it exited 64 as a usage error; it now
  answers `residue` with the reason and a hint to list the name's call
  sites (`trekr --refs build`), exit 1.
- **A superclass is looked up as Ruby looks it up.** `class Pool < Pool`
  inside `class Child < Parent` inherits `Parent::Pool`, found through the
  enclosing class's ancestors, where `--ancestors` stopped and called it
  unresolved; a superclass that names a module (Ruby raises) is reported
  unresolved instead of chaining through the module.
- **`--refs` at a call asks about the method the call runs.** On
  `Base.build("z")` at the top level, or in an instance method, it asked for
  the instance method `Base#build`; on `Base.new.build` in a class method,
  for `Base.build`. The side now comes from what the call lands on, in the
  CLI and in the editor's Find References.
- **`X.new` is a reference to the `initialize` it runs** (#4). `--refs
  'Widget#initialize'` lists each `Widget.new` — and a subclass's that
  inherits it, `self.new` in a class method, `public_send(:new)` — tiered
  as any call, each row marked `"called_as": "new"`; a class whose own
  `def self.new` returns another class rules its sites out, and one whose
  `new` trekr cannot read (Active Record's) leaves them possible. Find
  References on `def initialize` in the editor lists them, and go to
  definition on a plain `X.new` lands on its `initialize`. `--dead` reports
  an `initialize` only when nothing constructs its class, always graded
  lower — before, a real app's were never checked at all. An untyped
  `klass.new`, a `super` trekr cannot place and a macro's symbol keep none
  alive; the caveat names the first of each. In text, `--refs` folds the untyped
  `x.new` rows into one closing line (`… 1104 untyped x.new (possible)`);
  `--json` lists each.

- **`--dead` knows more of the methods Ruby calls by name.** A private
  `instance_variables_to_inspect` (`Kernel#inspect`, Ruby 3.4+) was
  `unreferenced`, clear; it is graded lower now, with the hook named. The
  list was audited against core and the stdlib, and also gained
  `deconstruct`/`deconstruct_keys`, `to_a`, `succ`, `to_regexp`,
  `pretty_print_instance_variables`, the `singleton_method_*` and
  `method_removed`/`method_undefined` hooks, `append_features`/
  `extend_object`/`prepend_features`, `const_added`, a class's
  `method_missing`/`respond_to_missing?`, and `json_create` (#5).
- **ERB templates are read.** Every `*.erb` in a checkout (`show.html.erb`,
  `welcome.text.erb`, `_form.html.erb`) is indexed for the Ruby its tags run,
  at the template's own lines and columns: a call in a view is a call site,
  so `--refs` lists it and `--dead` counts it, and `--def`, hover and
  references work inside a template. `--dead`'s "named in a view, which is
  not read" caveat no longer applies to ERB templates (it stays for Haml and
  Slim). An existing index picks the templates up at its next query.
- **A view's `self` is its view context.** A call in an ERB template under
  `app/views/` resolves on what Rails runs it on: a name a controller's
  `helper_method` exposes, then the app's helpers (all of `app/helpers`, as
  `helper :all` includes them), then `ActionView::Base` — `resolved_via:
  view`. A helper called only from views is a confirmed reference now, and
  `--dead` weighs it so. An exposed name runs on the controller that renders
  the template — its directory's, one whose action renders it by name, or
  for a partial those of the templates that render it — overrides included;
  rendered by several that differ, or by none trekr can name, it is
  ambiguous, and `--refs` lists it as possible for each.
- **Templates, smaller things.** Go to definition on a bare `@title` in a
  template, or RABL's `object @widget`, opens the controller's write
  (in the editor too). `local_assigns` in a partial is a `Hash`.
  Completing a bare word in a tag (`<%= badg`) offers the helpers,
  `helper_method` names and ActionView's methods. A name no view has says
  what was looked through, not that `ERB::Util` makes unnamed methods.
- **A view's `@ivar` is typed from its controller.** `@post` in
  `posts/show.html.erb` is what `PostsController#show` assigns, or a
  `before_action` that runs for it, or an action that `render :show`s;
  mailer templates read their mailer's action (`resolved_via: controller`).
  A template another controller renders by name (`render template:
  "posts/show"`) sees that action's writes too, and types that disagree
  make the answer ambiguous, each a candidate.
- **A `render`'s name opens the partial.** Go to definition on `render
  "row"`, `render partial: "shared/nav"`, `render @posts` or RABL's
  `extends "posts/base"` answers the template file (`under: template`). In a
  partial, a local a `render` hands it (`render "row", widget: @widget`,
  `post` from `render @post`) is defined at those keys, and typed by them.
  `render partial: "row", collection: @widgets` renders `_row` once per
  widget, handing it `row` (or the local `as:` names) with `row_counter` and
  `row_iteration`; `object:` hands one value the same way, and `render
  @posts` hands `post_counter` too.
- **RABL templates are read.** A `*.rabl` view is indexed as Ruby on its
  engine, which hands a name it lacks to the view's helpers. `object
  @post`'s `@post` is typed from the controller; `attributes :title`,
  `child(:comments)` and a hash's keys (`:label => :title`) name methods of
  the object of their scope — `--def` resolves them, `--refs` lists them,
  `--dead` counts them — and a `node` block's parameter is that object. A
  template that `extends` another lends it its object.
- **Editors serve templates.** The VS Code extension (0.5.1, published separately) and the Claude
  Code plugin's language server start for `.erb` and `.rabl` files too. The
  server says it reads them — `initialize` answers with `serverInfo` and
  `capabilities.experimental.trekr.templates: ["erb", "rabl"]` — and the
  extension sends a template only to a server that does, so an older trekr
  is never handed one. A document that is neither Ruby nor a template by its
  name (an `.html` an ERB extension claims) gets no syntax diagnostics.
- This changes what an index records (store v63): trekr reindexes once after
  upgrading.

- **`--dead` no longer credits the enclosing class with a `def` it does
  not own.** A `def` in a block that may run as another object
  (`instance_eval`, `Class.new`, a gem's DSL inside a method) and a
  singleton `def obj.meth` on a local were listed as the class's own,
  unreferenced and clear. One on an object of a known class is listed under
  that class — an `override` when the class has the method — and the rest
  say why they are not trusted; a call of the name on a receiver without
  such a method counts as a possible caller, and none is ever `clear`.
- **A private class method is private.** `private` inside `class << self`
  was dropped, so `--symbols` listed its methods as public and completion
  offered them after `Widget.`; `private_class_method :x` and
  `private_class_method def self.x` were not read at all.
- **`class X < X` inside a namespace inherits the outer `X`.** Ruby reads
  the superclass before the new class exists, so rails'
  `ConnectionAdapters::SchemaDumper < SchemaDumper` is an
  `ActiveRecord::SchemaDumper`; trekr read it as the class itself and ended
  the chain there, without saying so. `--ancestors` gives the whole chain,
  and `--dead` sees the base's calls of the hooks an adapter overrides. A
  superclass that names its own class is listed in `unresolved_ancestors`.

## 0.8.6 — 2026-10-03

- **`--dead`'s output is the same on every run.** A `test-only` constant's
  reason named its "first" test reference in the order the index's rows
  happened to come, which changed from run to run; it is the earliest by
  path and line now.
- **The first query in a repo indexes it.** No `trekr --index` first, and no
  more `not_indexed`: `--refs`, `--dead`, `--ancestors` and cards wait for the
  whole index; a position (`--def`, `FILE:LINE:COL`) answers as soon as its
  file and the files and gems it names are read — with `warming` while the
  rest is read in the background — and a miss there waits for the rest
  rather than asking you to try again. A position the file alone answers
  — nothing under the cursor, a local, a symbol, the definition the cursor
  is on — answers at once, with no index. An index that takes more than a
  second says so once on stderr, and at a terminal shows a progress line that
  clears itself; stdout is still only the answer. Ctrl-C stops the wait, not
  the index. A checkout whose index an upgrade dropped, or one cut short, is
  indexed the same way. `--status` still only reports. An index that
  dies on the way — killed, or a full disk — ends the query with what it
  died of, and the next query finishes it.
- **One first index per checkout.** A query, or a `trekr --index`, that finds
  another process's first index of the checkout under way (the language
  server's, another query's) waits for it instead of answering partial or
  indexing it a second time; `--index` says so on stderr. One left by a
  process that has since died is taken over at once, even when its pid now
  belongs to another program. The index under way reads next the file the
  waiting query asks about, or the editor has open — a `--def` behind the
  editor's first index, or the editor behind an agent's, answers in about a
  second rather than waiting for the whole checkout.
- **For scripts:** `--no-index` or `TREKR_NO_INDEX=1` keeps the old
  behaviour — answer from what is indexed, `not_indexed` (exit 2) where
  nothing is. A query whose index could not get the store's write lock in
  ten minutes answers as `--index` does: `status: incomplete`, exit 2.
- **Hover on a model shows its table.** On an Active Record model's name, or
  any reference to it, the hover lists its table's columns — type as the
  schema spells it, null, default, the primary key first — and its indexes,
  linked to the table's line in `db/schema.rb` or `db/structure.sql`; a wide
  table shows twenty columns and says how many more. The table is worked out
  as Rails does: `self.table_name`, a namespace's `table_name_prefix`, and a
  single-table-inheritance subclass's base table, which the hover says it
  shares. An abstract class says it has no table. Nothing boots the app.
- **Hover on a column's attribute shows the column.** `post.title`, `title`
  inside the model, or the column in `db/schema.rb`: "Column `posts.title`:
  `string`, not null, default `""`", linked to its line, where it said
  "Declared by `schema`". Completion's detail says the same.
- **Apps that keep `db/structure.sql` get their columns.** An app that dumps
  its schema as SQL (Postgres or MySQL) had no attribute methods at all, so
  `user.name.upcase` resolved nothing; its columns are now read like
  `db/schema.rb`'s, typed from their SQL types, for go to definition,
  references, hover, completion and `--dead`. When an app commits both
  files, the one Rails loads is read: `db/schema.rb`, unless
  `config/application.rb` sets `schema_format = :sql`. A view's columns are
  its select list's (a model backed by one says so, and how many it could
  not name); a table inheriting another has the parent's columns too; and a
  table in a schema the app's search path does not find is `schema.table`,
  a model's only through `self.table_name = "schema.table"`.
- **A `date` column reads as Date**, as ActiveRecord casts it, so
  `post.published_on.` offers Date's methods; it was typed Time.
- **An array column is an Array.** `t.bigint "tag_ids", array: true` (or a
  Postgres `bigint[]`) types its reader as Array, not by its elements.
- **A column's definition is its own line.** Go to definition on
  `post.title` lands on `t.string "title"` (or the column in
  `db/structure.sql`), not on `create_table`.
- **`trekr Post --json` carries `table`** for a model: the table's name,
  where the schema declares it, its columns, primary key and indexes.
- This changes what an index records (store v59): trekr reindexes once after upgrading.
- **`--dead` weighs an example group's own `let`s, `subject`s and `def`s.**
  Each is read the way RSpec runs it: by its group's examples and those of
  the groups nested in it, by a hook or `let` of an enclosing group, which
  runs for them, by the body of a shared group included there (in any file,
  through `it_behaves_like`, `include_examples` or `include_context`), by
  `super` in an override, by `is_expected` and a bare `should` for a
  `subject`, and by what `RSpec.configure` adds to every group — a helper
  it includes, a shared group it includes by metadata, a `config.before`
  hook. One
  nothing reads is a row — `kind: let`, `subject` or `method`, with the
  group it is written in (`group`) and how many shared groups and helpers
  were read for it — `unreferenced`, or `shadowed` when every read lands on
  an override (`overridden_by` names them). `let!` runs for every example
  and is never listed. Every such row is `lower`, its `caveat` naming a
  risk where one was found (a name sent at runtime, a group macro trekr
  does not read, a string `eval`ed): on mastodon's suite, run with every
  definition traced, every row was a `let` that never ran and it found 9 in
  10 of them, but suites held out from the rules each read a `let` some way
  they did not yet, so run the spec file after deleting one.
- **`--dead` lists a shared group nothing includes**: a `shared_examples` or
  `shared_context` no `it_behaves_like`, `include_examples` or
  `include_context` names is a row, `kind: shared_group`, and its own
  `let`s are not listed apart from it. One written with metadata is
  `convention-only`, which the groups that match include.
- **`shadowed`, a new `--dead` tier**: a method every call of whose name
  lands on an override in a subclass (an abstract `raise
  NotImplementedError` base), or a `let` every read of which an override in
  a nested group answers. `overridden_by` names the overrides. It was
  `unreferenced`, with nothing to say why the name looked used.
- **A group's `def` is no longer `unreferenced` while it is called.** It was
  weighed against an owner no call resolves to.
- **`--refs FILE:LINE[:COL]`** lists the references of what is at a
  position. On a `let`, `subject` or group `def` (or a read of one), every
  read, tiered, with `from` saying where it was found (`group`,
  `nested_group`, `enclosing_group`, `shared_group`, `includer`, `helper`,
  `super`, `subject`); `--dead` finds the same reads. On a method's
  definition or a call of it, the method's references, as `--refs
  Owner#method` lists them; on a class, module or constant, its mentions,
  as `--refs Name` lists them. The `let` keyword of `let(:x)` means the
  `let` it defines, as Find References in the editor reads it.
- **Find References on a `let` or `subject`** in the editor lists the same
  reads, a shared group's body in another file included.
- **Go to definition on a call in a shared group's body** that the body does
  not define lands on each including group's `let`, `subject` or `def` of
  the name — one answer, or one per includer (`resolved_via: includer`).
  It was residue. A helper `RSpec.configure` includes in every group is
  the answer only for the includers that define nothing of the name.

## 0.8.5 — 2026-10-02

- **Relation chains are typed.** `Post.where(…).order(…).pluck(…)`, a
  `scope`'s result and a `has_many` reader's are ActiveRecord relations, so
  each next call resolves to ActiveRecord's `QueryMethods`, `Calculations`
  or `CollectionProxy` instead of to any class sharing the name. A model's
  own class method or scope called on a relation (`Post.where(…).popular`,
  `user.posts.visible`) resolves to the model's, and a variable assigned a
  chain (`posts = Post.where(…).order(:id)`) is typed as the chain is. A
  `has_many`'s model is its name singularized as Rails does it, irregulars
  included (`people` → `Person`, `responses` → `Response`), and an `enum`'s
  mapping method its name pluralized the same way. This changes what an
  index records.
- **A macro inside `Class.new { … }` is no longer the enclosing class's.**
  A `has_many`, `attr_reader` or `include` in a block whose `self` is another
  object (an anonymous class in a test) made methods on the class the block
  was written in. This changes what an index records.
- **A call at the top of a file runs on `main`.** A top-level `require`
  resolves to `Kernel#require`, a top-level `def` is Object's and a bare
  call to it resolves, and a Minitest spec's bare `describe` is minitest's
  `Kernel#describe`. They were untyped guesses. In a Rake file `main` has
  Rake's DSL first, so `task` is `Rake::DSL#task`; a file another object
  evaluates — a Gemfile, a `config.ru`, a Jbuilder view, or one whose
  top-level calls `main` does not answer, such as a Discourse `plugin.rb` —
  is left untyped, as before.
- **Go to definition no longer jumps to a weak guess.** For a call trekr
  could not resolve, the editor gets its ranked guesses only when the first
  is a fair one; set `initializationOptions.unresolved` (VS Code:
  `trekr.unresolved`) to `peek` for every guess as before, `best` for the
  first, or `none`. On the comparison corpora this halves wrong jumps for
  under a point (discourse) and three points (mastodon) of right ones.
  The Claude Code plugin sets `peek`: an agent can weigh the guesses, and
  cannot learn why a definition came back empty.
- **An invalid `unresolved` setting is logged** (`setting_invalid` in
  `lsp.log`) instead of silently read as `confident`.
- **A residue's `confidence` is graded** (`--json`): how often its first
  candidate ran, on the gold sets, among residues resting on the same
  evidence — 0.7 for a call on `self` or a name with at most three
  definitions, 0.2 otherwise or when the receiver's known type lacks the
  name, 0.0 with no candidate. It was always 0.0.
  `agreement` says what backs it, and `--explain` prints it as `evidence`.
- **A `class_eval` block in a method body no longer replaces the class's
  own method.** `Const.class_eval do def self.x … end end` written inside a
  method reopens the class only if that method is called, so the class's
  unconditional `x` answers. This changes what an index records.
- **A module mixed in by another's `included` hook runs its own hook
  first.** `include Sidekiq::Job` extends `Job::ClassMethods` after the
  `Options::ClassMethods` its inner include brought, as Ruby does, so
  `sidekiq_options` in a worker resolves to Sidekiq's `Job` method rather
  than the one it overrides.
- **`--dead` lists unused classes, modules and constants too**, after the
  methods: one no constant reference in the checkout resolves to (views and
  executable scripts read too). Each row carries a new `kind` field —
  `class`, `module` or `constant`, and `method` on a method's row — and
  `summary.kinds` counts them; a script that took every row for a method
  should read `kind`. A new tier, `test-only`, is a class only specs and
  tests name (`summary.tiers` gains it). A class Rails or a library finds by
  name — a routed controller, a helper, a concern's `ClassMethods`, a
  policy, serializer or validator, an Administrate dashboard, an Action
  Mailbox mailbox, an Action Cable connection, a cop `.rubocop.yml`
  requires, a job built from a symbol or a class's name, an association, a
  YAML value, a registry — is `convention-only`, and what
  may still reach one (`self.class::LIMIT`, a listed namespace, a factory,
  an STI subclass) grades it `lower`. A constant read through a class or
  module that inherits or includes its namespace (`Child::LIMIT`,
  `Store::KEY`) is used. A constant handed to `enum` or an
  `attr_*` macro counts as a reference (this changes what an index
  records).
- **Upgrading drops and rebuilds the index** (store v58): trekr reindexes on
  its own; run `trekr --index` to do it up front.
- **`trekr --index` never exits 0 when it could not finish.** Stopped by
  Ctrl-C or a caller's timeout, it now says how far it got ("index
  incomplete: 180 of 144977 files read …; answers from it are partial
  until: trekr --index …") before exiting as the signal does — also when
  the reader of its output (`| tail`) went with the same Ctrl-C. Behind
  another trekr writer for longer than it waits (10 minutes), it says the
  same and **exits 2** where it exited 74, "database is locked"; under
  `--json`, `status: "incomplete"` with `warming` (whose `interrupted` is
  true only if the writer it waited on is gone). A script that retried on
  74 should retry on 2.
- **The editor's background index asks again when it waited too long.**
  One that outwaited another trekr writing the index used to give up for
  the session while hovers kept saying it was indexing; it now runs again
  until it gets its turn, and a hover says only what is under way. An index
  left partial or failed is said in a message the editor shows ("index cut
  short (N of M files read …) — answers are partial until: trekr --index
  …"), where before VS Code showed nothing, and one killed after its last
  write says "indexed". Hovers, messages and progress give the same,
  current count, of the checkout's files with its gems' and Ruby's.
- **A lambda handed to a callback is read on the instance.** In
  `before_action -> { authorize! unless skip_auth? }` the calls run on the
  controller's instance, as an `if:` lambda's do, so a concern the class
  includes answers them and `--dead` no longer calls such a method
  unreferenced. This changes what an index records.
- **`--dead` grades every class, module and constant row `lower`**, unless
  a convention names it: on forem, an app no rule was fitted on, an
  `unreferenced` one was truly unused 19 times in 50 and a `test-only` one
  4 in 20, and the row's `caveat` says so. A method row's `clear` held 55
  of 70 there (79 %).
- **`--dead` knows Pundit's and gems' base classes.** A policy predicate
  is `convention-only` when Pundit is in the bundle and a controller action
  of its name authorizes a record whose policy it is — in the action, or in
  a `before_action` that runs for it (`authorize @post` in `publish` asks
  `PostPolicy#publish?`). A base policy's predicate that a subclass
  overrides is the subclass's. A method a gem's base class may run by a
  name it computes (a CommonMarker renderer's node callbacks, a Liquid
  drop's methods), and a
  constant a gem's base class reads on `self.class` (Administrate's
  `COLLECTION_ATTRIBUTES`), say so and are graded `lower`.
- **`--dead` says when a method is named in YAML config.** A row whose
  name a tracked `.yml`/`.yaml` file writes — as `generator:
  pay_schedule_resources`, or as the method of `sanitizer:
  Pkg::FooSanitizer.sanitize_uid` — says "named in config (path:line),
  which is not read" and is graded `lower`. Locale files and lockfiles are
  not read, and a plain word (`summary`) is not taken for a method name.
- **`--dead` says when a method is in generated code** — a file
  `.gitattributes` marks `linguist-generated`, one under a `generated/`
  directory, or one whose header says "Generated by"/"DO NOT EDIT" — and
  grades it `lower`: such code is often called generically by its runtime
  (a GraphQL client's `from_response!`).

## 0.8.4 — 2026-10-01

- **The index format changed: trekr reindexes each checkout once** after
  upgrading.
- **A call on a memoized or `attr_reader` accessor is typed.**
  `url_builder.verify(…)` with `def url_builder; @url_builder ||=
  UrlBuilder.new; end` is now confirmed for `UrlBuilder#verify` and ruled
  out for other classes' `verify`; so is a reader of an `@x` every write of
  which (in its file) is `X.new`, and one whose body is `X.new`. Not when
  writes disagree, an `attr_accessor` lets anyone set it, or a subclass
  overrides the reader.
- **A callback or `rescue_from` block in a concern's class method runs on
  the includer's instance**: `rescue_from Error do … respond_error(e) end`
  inside `module ClassMethods` (or `class_methods do`) now counts as a call
  of the concern's `respond_error`, so `--dead` no longer calls it
  unreferenced.
- **A call inside a block handed to a DSL is never ruled out as "no such
  method".** A block passed to anything but Ruby's own methods may run on
  another object (`draw do`, a `scope` proc, a gem's configure block), so
  `--refs` lists such a call as `possible` and `--dead` counts it, instead
  of treating its target as unreferenced.
- `x = Class.new(Base)` no longer types `x` as an instance of `Class`, which
  ruled out the class methods sent to it.

## 0.8.3 — 2026-10-01

- **Every editor request is faster once the index is warm**: a definition on
  discourse takes about half a millisecond, where it took 7 ms re-reading
  the index's list of gems on every request.
- **A file you open while a first index is running is answered within a
  second, too.** It used to wait for the whole checkout to be written — up to
  17 s on a 100,000-file checkout. The index now writes it and the files it
  names to a copy of the index beside it (`trekr.db.early-<pid>/`), which the
  editor reads until the index finishes and removes it. An index stopped
  before it finishes leaves the copy for `--status` to list and `--gc` to
  remove; the editor goes back to the index and runs the index again.
- **Go to definition and references say when they answer from a partial
  index.** The first time either is asked while a checkout's first index is
  running, the editor shows one message saying how much is read and that
  those answers may miss or change until it finishes; hovers already said
  so. Once per checkout per session.
- **A definition into a gem answers sooner while a checkout is first
  indexed.** The index reads the gems the open file names before any other
  gem, and Ruby's signatures just after: on a 100,000-file checkout the
  gem's answer arrives about half a second sooner.
- **`--dead`'s `clear` on an `unreferenced` row now means something
  measured**: hand-checked on mastodon and discourse app code, 79–100 % of
  such rows were truly dead where 11–29 % were before; most former `clear`
  rows are now `lower` or `convention-only`, each saying why. On a CLI or a
  library, less (DEC-372).
- **`--dead` knows Thor runs a command by its name.** A public method of a
  `Thor` or `Thor::Group` subclass (every Rails generator), or one a concern
  defines in its `included do` for such a class, is `convention-only` with
  `convention: {by: "Thor"}`. A method under `no_commands`, and a plain
  module's, are not: Thor never runs them.
- **`--dead` counts `Mailer.action(…)` as a call of the mailer's action.**
  Action Mailer runs the action through its class's `method_missing`, so
  such actions were `unreferenced`.
- **`--dead` says when another helper calls a helper's method with no
  receiver**, as Rails' views do with every helper module mixed in, or a
  module nothing indexed includes does — an `if: -> { ready? }` a module's
  method hands a macro; the row names the call and is graded `lower`.
- **`--dead` says when a gem in the bundle calls an unreferenced
  method's name.** A gem calling `object.type_for_attribute` reaches an app's
  method no app call site writes; the row names the gem and is graded
  `lower`.
- **`--dead` knows a namespace's `self.table_name_prefix`** (and
  `table_name_suffix`, `use_relative_model_naming?`) is called by Active
  Record and Active Model by name, and grades it `lower`.
- **`--dead` says when an ancestor it has not indexed may call a method.**
  An `unreferenced` method whose class inherits from one trekr cannot
  resolve — a gem engine's controller, such as Devise's — or whose body
  calls `super` with nothing found above it, names that in its caveat and
  is graded `lower`.
- **`--dead` grades a model's public writer `lower`**: Active Model's
  `assign_attributes` (`new`, `update`, a form's params) calls `x=` by the
  key it is handed, which no call site writes.
- **`--dead` says where a method's name is built at runtime.** A method
  whose name has the shape of an interpolated symbol the checkout writes
  (`:"report_#{type}"`), or whose class a computed name is sent to
  (`Mailer.public_send(type, …)`), or whose module's methods it lists
  (`Tools.instance_methods`), says so in its caveat and is graded `lower`. Specs, tests and migrations are not read for this.
- **`--dead` knows ActiveModel::Serializers' `include_<attr>?` hooks.**
  With AMS 0.8/0.9 indexed, a serializer's (or its mixin's) `include_x?`
  whose `:x` the serializer or an ancestor declares is `convention-only`,
  with `convention: {by, path, line}` in JSON, instead of `unreferenced`.
- **`--dead` counts a `module_function`'s calls through its module.**
  `Tally.count(…)` runs `Tally#count`'s body, and the method was
  `unreferenced` when that was its only caller.
- **`--dead` reads a Haml template's continued lines and skips its
  comments.** A `-#` comment with an apostrophe hid every name after it in
  the template, and a call on the line under a `= f.input :x,` was not read,
  so helpers a view calls were `unreferenced`, clear. They now say "named in
  a view" and are graded `lower`. A binary file with a template's extension
  is no longer read as a template.
- **An unresolved call offers the method before the macro that forwards to
  it.** With nothing to tell candidates apart, `relation.update_all` listed
  ActiveRecord's `delegate … to: :all` line first; it now lists
  `Relation#update_all`, the code the delegate sends to, and the
  declarations after the definitions.
- **A receiver named after a class that two programs declare is typed again.**
  With a script's `User = Data.define(…)` beside the app's `User` model,
  `@user.id` in the app was unresolved, and its first candidate was the
  script's reader. It now answers the model's method, as it does where the
  name is declared once.
- **Upgrading drops and rebuilds the index** (store v53): trekr reindexes on
  its own; run `trekr --index` to do it up front.
- **`--dead` reads the routes.** A controller's public action that
  `config/routes.rb` (or a file it `draw`s, or an engine's) reaches —
  `to: 'c#a'`, `resources` and their default actions, `member`/`collection`,
  `concern`, `with_options`, `namespace` and `scope module:` — is
  `convention-only`, "named only by a route", with `route` in JSON, instead
  of `unreferenced`; so is an action a controller takes from a concern. A
  test app's routes (`spec/dummy`, `test/dummy`) are not read. Where a route is
  built at runtime, an action no read route reaches says so and is graded
  `lower`.
- **`--dead` no longer says "no call, symbol or `super` names it" of a
  method its own file names by a symbol it does not read as a call** (`only:
  [:archive]`, `opts[:limit]`). The row says the symbol is there, at which
  line, and is graded `lower`. And `--def` on such a symbol answers
  `residue` (`under: symbol`) instead of answering for the nearest other
  name on the line.
- **Calls in a callback's block, a `rescue_from` block and an `if:`/`unless:`
  lambda run on the instance**, as Rails runs them. `after_save do
  normalize end`, `rescue_from E do |e| respond(e) end` and `if: -> {
  ready? }` were read on the class, so `--def` found nothing, `--refs`
  ruled the call out, and `--dead` called `normalize` unreferenced. Reindex
  to pick up the lambdas (`trekr --index`).
- **A call with keywords is no longer ruled out of a method that takes
  them.** `thing.refresh(name: "a")` counted its keywords as a positional
  argument, so `--refs Widget#refresh` excluded it, "the argument count does
  not fit", when `refresh` is `def refresh(name: nil)`.
- **A method named by an option's symbol is a reference.** `rescue_from
  Error, with: :handler`, `before_action :x, if: :ready?`, `validates …,
  unless: :skip?`, `delegate …, to: :target` and an app's own
  `*_method_name: :x` now count, as `before_action :x` always did: `--def`
  on the symbol answers with the method (it used to answer for the nearest
  other name on the line), `--refs` lists it, and `--dead` tiers the method
  `convention-only` instead of `unreferenced` — on a macro written on the
  class, in its body or a `with_options` block. `only:`, `except:` and `on:`
  name actions and events, not calls, and still do not count. Reindex to
  pick it up (`trekr --index`).
- **The first index on a Ruby reads only the standard-library files it
  keeps.** It hashed every file under the Ruby's `lib/` — about 980 for Ruby
  3.4 — to keep 179, and listed them again, hashing them, to read the
  signatures. Nothing to do; trekr reads the signatures once more after this
  upgrade, as it does whenever their reader changes.
- **No editor request waits on the index for more than about a second.** While
  a first index is running, hover, definition and the rest answer from what
  is already read while trekr catches up on another thread, where on a large
  checkout a request could stall for 3–8 s each time the index moved.
  Completion that would wait seconds for its list answers with what it has —
  after a save, the list from before it — marked incomplete, and fills in as
  you type.
- **The editor answers about the file you opened within a second of the
  first index starting**, instead of after it ends. A checkout's first index
  now reads the open files and the files their constants live in first, then
  Ruby and the gems those files name, then the rest. On a 100,000-file
  checkout, definition, hover, references and completion on the open file
  went from 15–20 s to about a third of a second. The finished index is the
  same; `--index --json`'s `parsed` can be lower by the Ruby files the
  checkout also holds, now counted to the Ruby read first.
- **An answer given while a checkout's first index is still running says so.**
  Until its gems are in, an index can look whole and is not: an answer could
  come back `resolved` and change a second later. Now `--json` carries
  `warming` (`read`, `of`, `interrupted`, `hint`), `confidence` is scaled by
  the share read, nothing is called certainly absent (`no_such_method` reads
  `residue`, `--refs` rules out no caller), `--dead` lists nothing until the
  index ends, and a miss exits `2` instead of `1` — ask again when the index
  ends, or run `trekr --index`, which finishes it. `--status` says so on the
  checkout's row (`warming` in JSON), and names `trekr --index` when the index
  was cut short. In the editor, hovers say how much is read. Nothing to do; scripts that treat `2` as "index, then
  ask again" already handle it.
- **A command no longer fails with `database is locked` while another trekr
  upgrades the index.** After `brew upgrade`, a running language server
  switches to the new build and rebuilds the index in one long transaction; a
  command started meanwhile now waits for it, saying so on stderr, instead of
  giving up after five seconds.

## 0.8.2 — 2026-09-29

- **`--status` lists what is kept beside the index, and `--gc` removes it**:
  an index set aside as unusable (`trekr.db.broken-<time>`), at once, and an
  older or newer trekr's own index (`trekr.v51.db`), once it has been idle for
  `--older-than`. Never the index this trekr is using. `--json` carries them
  as `kept` on both.

## 0.8.1 — 2026-09-29

- **A damaged index, or an upgrade that fails, is rebuilt instead of failing
  every command.** Nothing to do: trekr moves the old file aside
  (`trekr.db.broken-<time>`, only the newest kept), says so in one line on
  stderr, and the next index refills it; `--json` answers `not_indexed` with
  the reason, as after an upgrade (DEC-300).
- **An older trekr no longer refuses a newer trekr's index.** It leaves that
  file alone and keeps its own beside it (`trekr.v51.db`), so a brew install
  and a dev build can both be used without rebuilding each other's index. A
  language server whose store another trekr rebuilds or sets aside moves to its
  own and reindexes, where it used to stop answering until restarted.
- **Upgrading drops and rebuilds the index** (store v52): run `trekr --index`
  once per checkout. The first index on each Ruby also reads its standard
  library once — 179 files for Ruby 3.4 — and its `rbs` gem's signatures,
  shared by every app on that Ruby.
- **Core now needs the `rbs` gem**, which every Ruby 3.x installs with
  itself: trekr reads Ruby core's classes, methods and return types from the
  signatures of the app's own Ruby, where they were built into the binary.
  Where no Ruby is found for a checkout, or it has no rbs, nothing is known
  of core — `puts`, `String#upcase` and friends answer residue — and
  `--index` says so. Once found, a checkout's Ruby and signatures are kept
  by every later reindex, whatever that reindex's environment (an editor
  launched from the Dock, the language server's background index); only
  the checkout naming another Ruby in `.ruby-version` or its Gemfile moves
  it, and `trekr --drop` forgets it (DEC-271).

### Added

- **`--index` and `--status` name the Ruby as an object.** `--index
  --json` has a top-level `ruby`, and each `--status --json` checkout one:
  `version`, `root` (its stdlib's) and `how` it was chosen — `named`,
  `gem_home`, `path`, `only` or `kept`; `null` when none was. The sentence
  in `gems.stdlib.ruby` stays (DEC-292).

- **A method card and `--refs` name the method a query resolves to.**
  `resolves_to` is it in Ruby's notation, and `inherited` says the owner
  inherits it rather than defining it; text output adds "resolves to
  Minitest::Assertions#assert_equal, inherited" for such a method.

- **Ruby's standard library is indexed.** `Set`, `Pathname`, `URI`,
  `Logger`, `Tempfile`, `FileUtils`, `Shellwords`, `SecureRandom`, `Open3`,
  `OptionParser`, `Gem::Version` and the rest answer from the Ruby the app
  runs on — the one `.ruby-version` or the Gemfile names, else `$GEM_HOME`'s,
  else the `ruby` on `$PATH` — where they were residue, or "no such method"
  on a class a gem reopens (`Pathname#join`, `URI.parse`, `Time#iso8601`).
  Tooling an app does not call (bundler's and rubygems' internals, irb,
  rdoc, reline, did_you_mean, prism…) and opt-in core extensions
  (`json/add/*`) are left out. An app that bundles its own copy of a default
  gem (json, logger, uri…) sees only its copy. `--index` reports it as
  `gems.stdlib` (`root`, `ruby`, `files`, `hidden`), and a lockfile naming a
  default gem at the version its Ruby ships counts it in `gems.from_stdlib`
  rather than `missing`; `--status` shows it per checkout. In an editor,
  `require "set"` opens that Ruby's file, a vendored bundle included
  (DEC-180).

- **A stdlib class that is partly compiled hedges a name its Ruby lacks.**
  A card or `--def` on a method its RBS signatures do not name either is
  residue naming the extension, not "no such method" (DEC-181).

- **Chains through the stdlib are typed, from its RBS signatures.**
  `Pathname.new(x).join(y).read.upcase`, `Digest::SHA256.hexdigest(s).upcase`,
  `SecureRandom.hex.length` and `Time.parse(s).year` resolve every call. A
  method compiled into the stdlib (`Pathname#exist?`, `Monitor#synchronize`,
  `Date#strftime`) answers with a declaration in `<core>/stdlib/Pathname.rb`
  and friends, `defined_via: "rbs"`, where it was residue; its `--refs` sort
  its sites. A class Ruby builds only in C or at runtime (`Digest::SHA256`,
  `OpenSSL::Digest::SHA1`) is known. A return RBS gives as a union, an
  optional, `bool` or `self` stays untyped — `URI.parse`, `Tempfile#path`,
  `Set#add` — as core's do (DEC-220).

### Changed

- **`--dead` says on the row when a view or a protocol hook may call a
  method.** A candidate whose name a view template's Ruby writes (ERB,
  Haml, Slim, Jbuilder) says "named in a view (…), which is not read", and
  one Ruby or Rails calls by name — `marshal_load`, `to_partial_path`,
  `each`, a job's `perform` — says "a hook … calls by name". Both grade it
  `lower`; the tier is unchanged. ndjson rows carry them in `caveat`
  (DEC-315).

- **`--refs` counts a call of a method the owner inherits.** Asked about
  `Child#save` where only `Base` defines `save`, a call on a `Child` or a
  subclass is `confirmed` where it was excluded as `different_owner`; a
  `Base` or a sibling class running the same method stays excluded. On
  rails, `--refs ActiveSupport::TestCase#assert_equal` goes from 0 to 23,378
  confirmed, and `ActiveRecord::Base.new` from 0 to 1,490. A method the
  owner defines itself answers as before, and `--dead` is unchanged
  (DEC-280).

- **`--def` on a method's own name carries `owner`**, as every other
  method answer does; a top-level def's is `Object`.

- **`--ndjson` streams row sets one row per line.** `--refs Owner#method`
  and `--dead` printed their whole answer on one line, rows as a nested
  array; now each reference or candidate is a line of its own, exactly as
  the `--json` array holds it, and a last line `{"answer": {…}}` carries the
  rest (`counts`, `definition`, `status`; `scope`, `summary`) with `rows`,
  the count. Bare-name `--refs`, `--symbols` and `--usage` already streamed
  their rows and now end with `{"answer": {"rows": N}}` too, written even
  when there are none. A script reading `--ndjson` should skip the line with
  an `answer` key (`jq 'select(.answer | not)'`) or read its tally from it;
  one that parsed `--refs`/`--dead` `--ndjson` as a single object should
  switch to `--json`, which is unchanged (DEC-290).

- **An editor's references answer about twice as fast.** The LSP tiers a
  scan's call sites on every core, as `--refs` does: on rails,
  `Persistence#save` 93 → 44 ms and `QueryMethods#where` 126 → 70 ms for the
  same answer (DEC-264).

- **`--refs` tiers on every core.** The workers share one tree, so its
  warm-up is paid once: a name with hundreds of thousands of call sites
  answers about twice as fast, and a rails query a third faster, for a
  fifth more memory at the largest.

- **Core and the stdlib's signatures come from the app's own Ruby.** They are
  read from the `rbs` gem bundled with that Ruby — else the highest installed
  for it — when its stdlib is indexed, instead of being generated once from Ruby 3.4
  and built in, so a Ruby 3.3 or 3.5 app is answered from its own. Core now
  covers every class and method RBS writes — rbs 3.8.0's 2,236 methods,
  where the built-in stub had 792 — so `File.stat(p).size`, `x.lazy.map` and
  the whole `Errno` family are known. `Mutex` is `Thread::Mutex`, as Ruby
  has it. Which stdlib methods are compiled is inferred from the index
  rather than asked of a Ruby. `--index` reports the gem as
  `gems.stdlib.rbs` (`version`, `path`, `chosen` — `bundled`, `installed` or
  `other` — and `read`), `null` with none, and
  `--status` per checkout; core's files are written under
  `trekr.core/rbs-<version>-<key>/` beside the database (DEC-240, DEC-274).
  The upgrade removes 0.8.0's `core/` directory there, and `--gc` removes a
  Ruby's signatures once no checkout runs on it, with its core files
  (`signatures`, `core_files` in `--json`).

- **Requirements that conflict are said to.** Without a lockfile, a gem
  whose requirements from the Gemfile, a gemspec and a picked gem's
  dependencies no installed version meets together is listed in
  `gems.unlocated`, naming where each was written, rather than "not
  installed" under a merged requirement no version can meet (DEC-276).

- **An editor says when trekr is reindexing after an upgrade.** Until the
  background index refills the store an upgrade emptied, each hover says so
  and how long it has run, and the progress reads "reindexing after an
  upgrade", where every answer read "nothing trekr has indexed defines"
  (DEC-275).

- **The rbs bundled with a Ruby stays its choice after gem maintenance.**
  `gem update --system` or `gem pristine rbs` no longer flips it to a later
  `gem install`'s with a false "none was bundled"; Ruby's own
  `gems/bundled_gems` list is read where an install keeps it (DEC-272).

- **A named Ruby is found wherever a version manager put it.** chruby's
  (`~/.rubies`, `/opt/rubies`), mise's and Homebrew's versioned kegs
  (`Cellar/ruby@3.3`) join rvm, rbenv and asdf, so a `.ruby-version` of
  `3.3` runs on the installed 3.3 rather than `$GEM_HOME`'s Ruby. A named
  version that is not installed is said by `--index` (`gems.ruby_not_found`,
  with the Ruby used instead) and `--status` (DEC-270).

- **A checkout that names no Ruby runs on the one it finds.** A directory
  with no Gemfile and no `.ruby-version` now indexes the stdlib of the Ruby
  `$GEM_HOME` names, else the `ruby` on `$PATH`, else the only Ruby
  installed — and so knows core — where it had none (DEC-242).

- **A definition or reference inside a module answers two to three times
  faster.** A lookup that finds nothing no longer works out every string
  macro's methods, only those of the macros that could make its name:
  `--def` on `respond_to?` in `ActiveSupport::Tryable` on rails 102 → 53 ms,
  on a 100k-file checkout 0.87 → 0.33 s (DEC-235).

- **Indexing a checkout half the size of the store or more is faster.** Such
  a load now rebuilds the store's indexes rather than inserting into them,
  as only a load bigger than the store did: 50k new files into a store of
  50k 20 → 14.5 s (DEC-234).

- **A cold `--index` of an app with a bundle is 4–10 % faster.** The
  bundle's gems are walked, parsed and written as one stream rather than one
  gem at a time: discourse 4.26 → 3.83 s, mastodon 3.42 → 3.14 s (DEC-232).

- **A definition or reference inside a module answers faster again.** A
  method lookup no longer allocates a key per step of the chain it walks:
  `--def` on `respond_to?` in `ActiveSupport::Tryable` on rails 119 → 102 ms,
  on a 100k-file checkout 1.03 → 0.86 s (DEC-231).

- **A long `--refs` answer takes a third of the memory.** Its references,
  and a bare-name listing's rows, are written as they are rendered rather
  than built whole first; the output is byte-identical. `--refs 'Hash#[]'
  --json` over a 100k-file checkout's 441k sites peaked at 1.7 GB and peaks
  at 0.6, and the text answer at 0.64 GB where it took 0.99 (DEC-230).

- **Queries find their checkout without starting git.** The checkout a query
  or an outline is about is read off the disk — the nearest `.git` that is
  recognisably a repository — and git is still asked whenever its answer
  could differ (`GIT_DIR` and friends set, a configured work tree, another
  user's directory, a filesystem boundary). That fork was most of a fast
  answer: `--symbols --json` 22 → 14 ms, level with rq's (DEC-190).

- **`--index --profile` names what follows the rows**: `index-rebuild`,
  `file-map` and `commit` (the app's and the bundle's), and `gem-walk`,
  reading each gem's files, so the phases add up to the wall time.
  `store-write` is now the rows alone; a script graphing it sees it shrink by
  what moved out (DEC-191).

- **Indexing a large repository into a store that already holds a lot is
  minutes faster.** Writing the file map opened a statement journal per file
  whose cost grew with everything already written: 50k new files indexed
  beside 50k known took 245 s, 181 s of it the map, and take 72 s (DEC-191).

- **The first query after `--index` is as fast as the ones after it.** Every
  index now writes the tree snapshot the next query would have assembled —
  only the LSP's background index did: discourse's first query 0.63 → 0.03 s,
  a 100k-file repository's 2.6 → 0.05 s. The index takes that time instead,
  when a declaration moved; `--profile` shows it as `tree` (DEC-192).

- **The index is less than half the size, and a cold index about twice as
  fast.** Call sites are stored as which files call each name rather than one
  row per call — every answer about a call already reread the file. A
  100k-file repository indexes in 20 s where it took 58, into 0.6 GB where it
  took 1.5; discourse with its gems in 5 s and 121 MB. The bare-name `--refs
  NAME` listing reads its call rows from the files, as its tiering already
  did (DEC-193).

- **Editing a method no longer rebuilds the tree snapshot.** The snapshot is
  keyed by what it holds — classes, modules, constants and their ancestry —
  so an edit inside or among methods keeps it: a method edit's `--index` on
  discourse 450 → 142 ms, on 100k files 2.4 → 1.2 s, and an editor's next
  answer after such a save maps the snapshot instead of reassembling it
  (DEC-194).

- **A question about a call inside a module is faster, most of all in a
  large repo.** Finding which classes mix a module in linearized every
  class's whole ancestry afresh; each chain is now built once. `--def` on
  `respond_to?` inside `ActiveSupport::Tryable`: rails 0.30 → 0.06 s,
  mastodon 0.27 → 0.08 s, a 100k-file checkout ten minutes → 0.5 s
  (DEC-200).

- **`--refs` on a much-called method is faster.** What a name's
  definitions return, each name's definitions, method lookups and class-side
  lookup chains are worked out once per query rather than at every call
  site, the local flow analysis runs on the parse workers, and the next
  files are parsed while the last are tiered. rails' 51-query `--refs` set
  21.9 → 10.6 s; `--refs 'Hash#[]'` over a 100k-file checkout's 441k
  call sites 460 → 15 s (DEC-201–205).

- **`--status` in a checkout nobody indexed exits `2` with `status:
  not_indexed`**, the answer a query from there gives, where it listed
  another checkout as `checkouts[0]` and exited `0`. `checkouts` is `[]`;
  `others` and `totals` still summarize the store. A script that read exit `1`
  from `--status` as "nothing indexed here" should read `2` (DEC-170).
  Run from a checkout's top directory, `--status` also listed every indexed
  repo rather than that checkout; it shows the one checkout now.

- **`--status --context DIR`** reports on that checkout, from anywhere; it
  was a usage error (DEC-170).

- **Fewer answers hedge on a `define_method` that cannot have made the
  name.** 0.8.0 made a class that runs `define_method` or a `class_eval`
  string answer residue for every name it lacks, on both sides. Now a name
  the source spells is defined (`define_method(:made)` in a class method, a
  local built from a literal loop's value), a `define_method` run on another
  object marks nothing here, `define_singleton_method` hedges class methods
  only, and a name whose text is partly spelled (`"_render_with_#{key}"`)
  hedges only names of that shape. The reason names every marker in the
  scope with its shape. On mastodon, cards for a name no class has went from
  819 residue to 399 (0.7.0: 380) (DEC-160).

- **Every string of code marks the class it makes methods on.** A
  `class_eval` handed a local, `[…].join`, `format(…)` or a heredoc through
  `.gsub`, `instance_eval` and `eval` of a string, `Target.class_eval "…"`,
  and `self.class.class_eval` in an instance method made methods the card
  called absent, exit 1. They now hedge like any other marker, `eval` in a
  class body and a heredoc through `.strip` are read as code, and
  `instance_eval` hedges class methods only (DEC-161).

- **A macro's methods hedge the classes that call it.** A `class_eval`
  string or `define_method` in a `ClassMethods` method (or any method a
  class body calls on itself) makes methods on the calling class; the card
  said "no such method", exit 1, and `--def` blamed a gem. The calling class
  is now marked for the names its call hands the macro (`add_reader :color`
  hedges `color`), and a name defined nowhere that the call's own file may
  make says so (DEC-162).

- **A string of code is read with the names it is handed.** Where a class
  body calls, with literal names, a method in the same file whose
  `class_eval` string interpolates its parameters (`add_helper :color`,
  `make :fast`), the string is read there: `Widget#color_helper` is a method
  of `Widget`. A call the string names by interpolating a loop's value
  (`helper_#{n}`) is a call of each value's name, so `--refs` and `--dead`
  count it; one no value fills gives `--dead` a caveat on the methods of
  its shape (DEC-163).

- **A string of code that would make more than 2,000 methods is marked
  instead of written out** (20,000 per file), and the reason says how many:
  300 names over 1,000 `def`s took 7 s to index (DEC-164).

- **A custom `new` is read by every path it returns.** `return super() if
  flag; Engine.new` typed every `Guarded.new` as an `Engine` at confidence
  1; the value is now either, and a call on it is ambiguous. A subclass's
  `def self.new; super; end` follows `super` to its parent's `new`, so
  `Reset.new` makes what `Factory.new` makes. A local whose writes disagree
  answers with the type that has the called name, ambiguous, where it was
  residue (DEC-165).

- **`--refs` follows a `delegate`.** A call that lands on `delegate :name,
  to: :x` counts toward the method `x`'s type runs: `Person.delete_by`,
  through ActiveRecord's `delegate … to: :all`, is a confirmed call of
  `ActiveRecord::Relation#delete_by`, where it was excluded
  (`different_owner`). A target of no known type makes the site `possible`
  (DEC-166).

- **An editor answers inside a `class_eval` string trekr reads.** A local
  there gets hover, highlight and go-to-definition, and a hover on a `def`
  that makes one method per value (`def #{n}_x` in a loop) lists every
  method, where it showed the first (DEC-167).

- **`--refs` counts a call whose receiver is typed as an ancestor.** A
  `sig`'s parameter or return, a finder's result, a `rescue`'s class, or the
  type a variable's name suggests is a type the object conforms to, and it
  may be a subclass: `context.admin?` with `context: Lib::Context` runs
  `App::Context#admin?` when handed one. Such a site was excluded
  (`different_owner`, or `no_such_method` when the ancestor lacks the name)
  from `--refs App::Context#admin?`, and a method only a subclass defines,
  reached that way, was `unreferenced` in `--dead`. It is now `possible`;
  `X.new`, a literal and a constant still name the exact class. The site
  stays `confirmed` for the ancestor's own method (DEC-140).

- **`--refs` counts it for a module a subclass mixes in, too.** A call
  on `self`, or on a receiver declared as `T`, is `possible` for a module's
  method when a subclass of `T` includes that module and runs its method:
  `AbstractController::Base#process`'s `process_action` for
  `AbstractController::Callbacks#process_action` (DEC-213).

### Fixed

- **`--dead` counts calls of a method's alias.** A method reached only as
  `array?`, through `alias :array? :array`, was `unreferenced`; its alias's
  callers are now its callers (DEC-316).

- **Kernel's methods answer inside a module no class is seen including.**
  `Pathname(path)`, `Integer(x)` or `raise` in a Rails helper module was
  residue; a call on `self` there runs on some object, so where no
  includer answers, `Object`'s chain does (`resolved_via: "object"`,
  DEC-314).

- **A class macro's `include` reaches the class that calls it.** draper's
  `delegate_all`, a class method whose body is `include
  Draper::AutomaticDelegation`, left `--ancestors CommentDecorator` without
  it. A class body's call of a macro that unconditionally includes,
  prepends or extends a constant now gives that class the mixin (DEC-313).

- **Text output says a little more.** A gem list cut short in `--index`
  ends "(--json lists all)"; `--index` says how many files repeat another's
  bytes when there are fewer blobs than files; and `--refs Owner#method
  --include-excluded` always prints its tally, "0 excluded" included.

- **A Gemfile's git gem is no longer dropped without a lockfile.** `gem
  "rack", github: "rack/rack"` in a checkout with no `Gemfile.lock` was in
  none of `missing`, `unlocated` or `picked`. It is now the one checkout of
  that repository in `bundler/gems` (picked as `rack <revision>`, counted
  in `from_git`), or listed in `gems.unlocated` saying there is none, or
  several and nothing to choose by. `--index` also says when the Gemfile is
  newer than `Gemfile.lock`, which is what is read (DEC-293).

- **Gems come from the checkout's Ruby.** A checkout naming a Ruby in
  `.ruby-version` (or kept from its last index) had its stdlib from that
  Ruby and its gems from the shell's `$GEM_HOME`, another Ruby's:
  `SecureRandom.hex` answered from rvm's 3.4.9 in an app on rbenv's 3.4.10.
  That Ruby's gem directories are now searched first, lockfile or not. A gem
  found only for another Ruby is still indexed and is listed in
  `gems.other_ruby`, and `--index` says so (DEC-291).

- **`--help` explains `--dead`'s tiers and confidence**, says `--json`
  carries more than the text summary, and says a usage error is JSON
  wherever `--json` sits on the line. The README's usage-error row no
  longer reads as if a flag before `--json` were itself an error.

- **A macro's own names are no longer its callers.** `--refs` counted the
  symbol in `scope :ordered`, `attr_reader :url_prefix` or `alias_method
  :new, :old` as a possible call of that name, so another model's
  `scope :ordered` was listed under `User.ordered`; and a symbol compared
  with `==` or used as a hash key counted too. Neither does now (DEC-312).

- **`--dead` counts the callers of a top-level `def`.** A spec/support
  helper defined outside any class was `unreferenced` with clear confidence
  while `--refs` listed its callers; an implicit call that finds no other
  method of the name now counts as a possible caller (DEC-311).

- **A development tool's patch on `Object` no longer hedges every missing
  method.** minitest's `infect_an_assertion` and pry's `__binding__` made
  every `no_such_method` on every class residue — "Object defines methods
  its source does not name" — so `Flipper::Gate#zz` was never certain. A
  name in `defined?(…)` is no call, a `class_eval` of a constant the file
  assigns code reads that code's `def`s, and a comment line in a string of
  code is not code (DEC-310).

- **StringIO, Zlib, Etc and StringScanner are known.** A stdlib library
  written only in C, with no Ruby file, is declared from its RBS signatures
  when its extension is there, as a partly compiled one already was:
  `StringIO.new(s).read` resolves where it was "no indexed constant"
  (DEC-262).

- **A bundled json gem no longer gives every object `to_json` from
  `json/add/`.** The opt-in extensions the stdlib's copy leaves out are left
  out of the gem too, so `Time.now.to_json` no longer lands on
  `json/add/time.rb` in an app that never requires it (DEC-180).

- **A call through a `SimpleDelegator` may run the delegated method.** A
  `method_missing` that sends the name on to another object — `Delegator`'s,
  a proxy's, `ActiveRecord::Migration`'s — makes `--refs` count a call its
  class lacks as possible for a method of any class, and `--def` say so,
  where both called it "no such method" and `--dead` called the method
  unreferenced. ActiveModel's `method_missing`, which sends nothing on, still
  excludes (DEC-261).

- **A call in a `test "…" do` block runs on the test, not its class.** A
  block a class body hands a macro that makes a method of it —
  `ActiveSupport::Testing::Declarative#test`, Minitest's `it`, an app's own
  `define_method(name, &block)` macro — is that method's body, so its calls
  on `self` are the instance's. They were read on the class side, and
  `--refs` excluded them as "no such method": rails' `assert_equal` lost
  5,619 such sites, and `--dead` called a helper only tests call
  unreferenced (DEC-260).

- **A method in `class << Time` inside `class Time` is `Time`'s.** A
  constant in that body made the method land on a `Time::Time` nothing
  declares, so `Time.parse` from Ruby's own `time.rb` was not found (DEC-241).

- **A call in an `ActiveSupport.on_load` block runs on the hooked class.** A
  bare call in `on_load(:active_record) do … end` is `ActiveRecord::Base`'s
  class method, and one in a `def` there its instance method, for `--def`,
  `--refs`, hover and VS Code's completion, which offered `Object`'s methods
  there. A hook two classes run is ambiguous between them (DEC-214).

- **A macro in another file defines the methods its callers name.** A class
  body's `add_helper :color`, where `add_helper` (in another file) writes
  `def #{name}_helper` in a `class_eval` string, makes `color_helper` a method
  of the class, at the macro's `class_eval`: `--def`, cards and `--refs`
  resolve it where they answered residue — every `define_callbacks :save`'s
  `_run_save_callbacks`, for one. The calls inside the string are still not
  read there (DEC-212).

- **`--def` on `Person.delete_by` reaches `Relation#delete_by`.** A call that
  lands on a `delegate … to: :all` (or any `to:` whose reader declares a type)
  answers the method that type runs, with the delegate as the second site;
  `resolved_via` is `delegate`, and `reason` names the delegate. Where a
  subclass of the target type overrides the name, the answer is `ambiguous`
  and names it. Hover says the call was sent on, and go-to-definition offers
  both lines (DEC-211).

- **The app's own method wins over a gem's that reopens the same class.**
  The app was layered before its gems, because it is indexed first, so where
  both define one method a card, `--def` and `--refs` answered the gem's.
  Ruby loads the bundle first; the app's definition is the one that runs
  (DEC-210).

- **A class that names a mixin through its own ancestors includes it.**
  `include Kramdown::Parser::Html::Parser` inside `class
  Kramdown::Parser::Kramdown` (which had just done `include ::Kramdown`)
  resolves as Ruby resolves it, so the class's own `--ancestors` lists the
  module rather than reporting it unresolved; its subclasses already did.
  A class's chain no longer depends on which class was asked about first
  (DEC-200).

- **A variable named `object` no longer types its receiver.** The naming
  rung read `object` as class `Object`, so `object.inspect` was a confirmed
  reference to `Kernel#inspect` and excluded from a module's own `inspect`.
  A class every object is (`Object`, `Kernel`, `BasicObject`) narrows
  nothing, and the call is now untyped: `possible` for every `inspect`
  (DEC-172).

- **A queued `--index`, `--drop` or `--gc` says what it is waiting for.**
  Behind another writer it waited up to ten minutes with no output; after a
  second it now prints "waiting for another trekr writer …" on stderr, then
  again every 10 s on a terminal (every minute otherwise). stdout is
  unchanged: a `--json` caller still gets one answer when the write lands
  (DEC-171).

- **Gems from git are indexed.** A lockfile's `GIT` sources are found where
  bundler checks them out, `bundler/gems/<repo>-<sha>/` — the locked revision,
  not whichever one is on disk — and a monorepo's gems (`gem "rails",
  github: …`) each from their own subdirectory's gemspec. They were reported
  "not installed", and everything from them answered from RBI stubs or as
  residue. `BUNDLE_PATH` (`.bundle/config`, the environment, `~/.bundle/config`)
  is searched first. `--index` counts them (`gems.from_git`, and
  `gems.from_path` for path gems inside the checkout), and a git or path gem
  that is not indexed is listed in `gems.unlocated` with the reason — a
  checkout not where bundler puts it, or a path outside the checkout — rather
  than as not installed (DEC-150).

- **A position inside a git gem answers from the app**, in the CLI and the
  editor. Bundler's checkout has a `.git`, so it was taken for a checkout of
  its own that was never indexed. `--index` on one now says it is a gem, as
  for any gem (DEC-150).

- **Without a lockfile, a gem's pick meets every requirement on it.** A
  dependency's own `>= 0` no longer picks past the gemspec's `~> 5.25`
  (minitest 6 instead of 5.26); a requirement held in a constant, or a gem
  named in a `%w[…].each` loop, is read, and `ENV["V"] || "7.1"` reads as
  its default; a Gemfile's `if`/`else` takes the default branch instead of
  merging both. A requirement trekr cannot read
  (`version`, an interpolation) is listed in `gems.unread` rather than
  silently taken as any. `gems.picked` lists the version of every gem found,
  and the text says the picks when there is no lockfile (DEC-151).

- **Without a lockfile, gems come from one Ruby.** The picks are made among
  the gems installed for the Ruby `.ruby-version` (or the Gemfile's `ruby`)
  names, else `$GEM_HOME`'s, else the `ruby` on `$PATH` — not the newest
  copy any Ruby on the machine has. `--index` says which in the text and as
  `gems.ruby` (DEC-152).

- **A default gem says where its code is.** One at the version its Ruby
  ships (json 2.9.1, logger, uri, psych…) has an empty gem directory and its
  code in the stdlib; it was counted resolved with 0 files read. It is now
  listed in `gems.unlocated` as a default gem, with the stdlib directory
  (DEC-153).

## 0.8.0 — 2026-09-28

- **Upgrading drops and rebuilds the index** (store v39): run `trekr --index`
  once per checkout.

### Added

- **A checkout with no `Gemfile.lock` gets its gems anyway**: what its
  gemspecs and Gemfile declare, each at the highest installed version that
  meets it, with their runtime dependencies. A gem's own specs now see
  rspec-core — `describe`, `it_should_behave_like`, matchers — where every one
  was residue. `--index` says so, and `--json` has `gems.resolved_from`
  (`lockfile` or `declared`) (DEC-134).

- **`--context DIR` points a name query at a checkout**: `trekr Widget#save
  --context ~/app`, `--refs … --context`, `--ancestors … --context`, from any
  directory. It already did for a position (DEC-135).

- **`--dead` rows say `visibility`** — `public`, `protected` or `private`, and
  text marks the non-public ones — since whether a deletion can break a caller
  outside the checkout turns on it.

### Changed

- **`--dead` has an `override` tier.** A method nothing calls by name that
  overrides one an indexed ancestor defines — `readonly?` in a model, a
  visitor's `visit_X`, a module's `extended` — was `unreferenced` with clear
  confidence, though whatever calls the ancestor's method, framework code
  included, runs it. It is now `override`, graded `lower`, and a candidate in
  another tier that overrides a method is graded `lower` too, its `caveat`
  naming it. Every row has an `overrides` array in JSON. A script that
  switches on `tier` must handle the new value (DEC-121).

- **`--dead` names each candidate as Ruby's documentation does**:
  `Widget#save`, or `Widget.build` for a class method, where the text showed
  the bare name and hid the difference. JSON rows gain `singleton`.

- **`--dead` counts what it found.** The text ends with a line per tier and
  confidence — `141 candidates in 33 file(s): 6 unreferenced, 7 override, …
  (52 clear, 89 lower)` — and the JSON object gains `summary`
  (`candidates`, `tiers` with every tier present, `confidence`). A script
  reading the text rows must stop at the blank line before it.

- **`--status` shows the checkout you are in**, with its gems counted —
  `+ 304 gems, all indexed (11502 files)` — and a count of what else is
  indexed, where it listed every gem checkout on the machine (hundreds of
  lines for one Rails app). Outside any checkout it lists the repos and
  counts the gems. `--status --all` lists everything, as before. In JSON,
  `checkouts` holds the same rows the text shows, each now with `kind`
  (`repo` or `gem`) and, for a repo, `gems: {count, indexed, files}`, and
  `others: {repos, gems}` counts the rest. **A script that read every
  checkout from `--status --json` must add `--all`.** An empty store says
  why in `reason` — an upgrade that dropped the index, or nothing indexed
  yet — where it was a bare `checkouts: []` (DEC-125).

- **`--refs Owner#method` always ends with its tally**, `1 confirmed, 0
  possible, 0 excluded of 1 same-name call sites`; text left it out when
  nothing was excluded, which is when it is most worth saying.

- **`--index PATH` says it indexed the checkout containing `PATH`** when `PATH`
  is inside one rather than its root. It always indexed the whole checkout.

### Fixed

- **A shared group's name answers the group.** A click on the string in
  `it_behaves_like "a widget"`, `include_examples` or `include_context`
  snapped to the method and landed in rspec-core; it answers the
  `shared_examples`/`shared_context` of that name, in `--def`, hover and
  the editor's go-to-definition. A name nothing indexed defines says so
  (DEC-124).

- **A snapped `--def` says so on stdout**, under the answer: ``snapped_to `it`
  at column 3: no name at column 2``. It was a stderr note a pipe
  dropped. `--help` and the README now say that a column on no name snaps.

- **A name nothing indexed defines says so.** `before_action
  :authenticate_user!` in a Devise app answered "the receiver's type is
  known, and nothing indexed in its ancestors defines this name", which
  reads as a method missing from the controller. The residue now says no
  indexed file defines the name anywhere — the checkout, its gems, Ruby
  core — and that a gem may generate it at runtime. The ancestors reason is
  kept for a name that exists elsewhere (DEC-126).

- **In a module, completion offers what its includers have.** A bare word
  in a concern's `included do` block is offered the including classes'
  class methods, and one in the module's own methods their instance
  methods, after the module's own; it offered only the module's. And a
  residue there says which side was asked: `stamp!` in `included do`,
  where the includer has only an instance `stamp!`, said "no class that
  includes it defines `stamp!`", which was false; it says none has a class
  method of that name (DEC-127).

- **A call on `described_class` resolves.** `described_class.blocked?`
  answers the described class's own `blocked?`, and `described_class.new.x`
  its instance method, `resolved_via: described_class`; both were residue,
  though `described_class` itself was followed. `--refs` confirms them
  (DEC-120).

- **A module's `self` call skips a method its includer shadows.** In a
  class that includes `Formatting` and then `Labeled`, `Labeled#show`
  calling `label` runs `Labeled#label`, and `--def` said so, but `--refs
  Formatting#label` counted the call possible and `--dead` called
  `Formatting#label` single-caller. A method another of the includer's
  modules defines counts only when it comes ahead of what the call finds
  (DEC-122).

- **A string that mentions `.must_` no longer makes a spec Minitest's.**
  `expect(events).to include("accord.parse.must_be_positive")` turned the
  whole of accord's `instrumentation_spec.rb` into residue, its bare
  `describe` read as Minitest's. Only a call counts now — `x.must_equal`,
  `require "minitest/spec"`, `Minitest::Spec` (DEC-123).

- **A method made from a name the source does not state is no longer "not
  there".** Where a class, or one of its ancestors, defines methods with
  `define_method` from a computed name or with a `class_eval` string, the card
  and `--refs Owner#name` answer `residue`, naming that class and line, instead
  of `no_such_method`, and `--refs` lists the call sites on it as possible. It
  was faraday's `Connection#get` and flipper's `Wrapper#enable` (DEC-130).

- **`METHODS.each { |m| define_method(m) { … } }` defines each name**, where
  `METHODS` is a literal list of names the same file assigns: flipper's
  `Wrapper#enable` resolves (DEC-131).

- **A `class_eval <<-RUBY … RUBY` string is read as code.** Its `def`s are the
  class's methods (once per value when it loops over a literal list), its calls
  count, and `--def` inside it answers. faraday's `run_request` is no longer
  single-caller: two of its three calls are in such strings. A string trekr
  cannot render is flagged in `--dead` as `lower confidence: class_eval string`
  (DEC-132).

- **`X.new` makes what a custom `new` says it makes.** When `X`'s class side
  has a `new` whose `sig` or last line (`Other.new(…)`) names another class,
  `X.new` is an instance of that class. flipper's
  `described_class.new(adapter)` is a `Flipper::DSL`, so `flipper[:search]` is
  `DSL#[]`, not `Flipper.[]` (DEC-133).

- **A `scope`'s body is a relation only in a model.** A Mongoid document's or a
  plain class's `scope` no longer answers `ActiveRecord::Relation#where`, and a
  model class method Kernel also has (`display`) called in a scope is the
  model's (DEC-136).

- **`x.class` is `x`'s class**, so `self.class.statuses` finds the enum's class
  method instead of residue (DEC-137).

- **A method Rails generates loses to the class's own and its modules'.** A
  hand-written `def status` above `enum :status` is the one that runs, and an
  `enum` in a concern's `included do` beats the schema column, so its reader is
  a String (DEC-138).

- **Concurrent `--index` runs wait for each other** instead of one failing with
  "database is locked" (exit 74) after 5 s (DEC-139).

## 0.7.0 — 2026-09-28

- **Upgrading drops and rebuilds the index** (store v38): run `trekr --index`
  once per checkout.

### Added

- **`enum` generates what Rails generates.** The attribute's reader (a
  String, whatever the column stores) and writer, a `not_` scope beside each
  member's scope, and the names `prefix:` and `suffix:` build — `true` takes
  the attribute's name, a symbol is used as written. `scopes: false` and
  `instance_methods: false` are honoured, and every attribute of a Rails 6
  `enum a: {…}, b: {…}` is read. A prefixed or suffixed enum used to define
  no member methods at all (DEC-110).

- **More Rails macros declare their methods**, each answering as a
  declaration by its macro: `has_secure_password` (`authenticate`,
  `password=`, `password_confirmation=`, the reset token), `has_secure_token`
  (`regenerate_token`), `has_one_attached` and `has_many_attached` (the
  reader, typed as Active Storage's proxy, the attachment and blob
  associations, `with_attached_x`), `accepts_nested_attributes_for`
  (`x_attributes=`), and `store`'s `accessors:`. A `belongs_to` adds
  `x_changed?`, `x_previously_changed?` and `reset_x`; a schema column, an
  `attribute` and an `alias_attribute` add the dirty tracking code calls
  (`x_changed?`, `x_was`, `saved_change_to_x?`,
  `will_save_change_to_x?`, `x_before_last_save`, `x_previously_changed?`)
  (DEC-111).

- **`delegate_missing_to :account`** hands a name the class lacks to
  `account`: when its reader has a type (a `belongs_to` does), the call
  resolves to that class's method, `resolved_via: delegate_missing_to`, and
  `--refs` confirms it; when it has none, or the target lacks the name too,
  the residue says where the name went (DEC-112).

- **A block handed to a shared group's own helper** sees the group's other
  methods: in `serving { |s| s.write(http_response(500)) }`, where
  `serving` and `http_response` are both the shared context's,
  `http_response` resolves to it; it was residue. And `--refs` on a method
  of a shared group, or any group's `let` or `def`, confirms the calls that
  reach it through the group, where it counted them possible (DEC-113).

- **RSpec's implicit subject.** With no `subject` written, `subject` and
  `is_expected` are an instance of the class the group describes, as
  `described_class.new` is, so `is_expected.to be_empty` reaches its
  `empty?` and `subject.save` resolves (DEC-114).

- **A bare top-level `describe`** in a spec answers `RSpec.describe`, as
  RSpec's `expose_dsl_globally` makes it (`main.describe` sends to
  `RSpec`), `resolved_via: main`; it was residue. `shared_examples` and
  `shared_context` likewise, and a click on one no longer lands on the
  module it declares (DEC-115).

### Fixed

- **A `scope`'s body runs on the relation.** `scope :cheap, -> { where(…) }`
  answered the class's delegating `where` (`ActiveRecord::Querying`),
  confidently and wrongly; it is the relation's `QueryMethods#where`, and a
  name the relation lacks — another scope, a class method — goes to the
  model, `resolved_via: scope` (DEC-116).

- `class_attribute`'s and the `mattr` family's `instance_accessor:`,
  `instance_reader:`, `instance_writer:` and `instance_predicate:` options
  are read: `class_attribute :x, instance_writer: false` declared an
  instance writer Ruby does not have. `thread_mattr_accessor` and its kin
  declare what `mattr_accessor` does (DEC-111).

- `store_accessor :settings, :theme` declared a `settings` accessor: the
  first argument is the store, not a key. `prefix:`/`suffix:` now rename its
  keys. `alias_attribute :new, :old` declared `old` as well as `new`
  (DEC-111).

### Fixed

- **A mixin sent from a loop over a literal list of classes** reaches each of
  them: `[Hash, Array].each { |klass| klass.include(Encoder) }`, or the same
  over a constant this file assigns such a list. ActiveSupport sends its
  `to_json` to ten core classes this way, so `{ … }.to_json` answered the json
  gem's method, or nothing, where a Rails app runs ActiveSupport's (DEC-100).

- **A mixin into a singleton class gives class methods.**
  `X.singleton_class.include(M)` and `class << X; include M; end` extend `X`
  with `M`; `X.singleton_class.prepend(M)` puts `M`'s methods ahead of `X`'s
  own class methods, which is how a gem wraps another's. They were ordinary
  calls, and `include` inside `class << self` was read as an instance-side
  include, so `Widget.new.label` found a method Ruby raises on (DEC-101).

- **`def self.included(base)` hooks are read.** `base.extend(ClassMethods)`,
  `base.include(M)`, `base.send(:include, M)` and `base.singleton_class.
  prepend(M)` in a module's `included`, `extended` or `prepended` hook land on
  whatever mixes the module in, and a `def self.x` in `base.class_eval do`
  is its class method. It is how every gem written before
  ActiveSupport::Concern gives its includers class methods, and
  `Widget.track` found nothing (DEC-102).

- `--refs` and `--dead`: a call on `self` in a module is a possible reference
  to a method another module of the same includer defines — a module calling
  what it expects its includer to provide. It was excluded, and `--dead`
  called the method unreferenced (DEC-081, amended).

- **`include` and `prepend` in a concern's `included do`** mix into the
  includer, not the concern: a module prepended there now comes ahead of the
  includer's own methods, as in Ruby, instead of behind them (DEC-103).

- **`ActiveSupport.on_load` blocks, the rest.** A mixin sent to the block's
  parameter (`on_load(:x) { |base| base.include(M) }`, and with `yield:
  true`) lands on the hooked class, and a `def` in the block defines on it,
  in a module `on_load(:x)` the class prepends; that `def` used to be the
  top level's (DEC-104).

- **A concern's `ClassMethods` are no longer its own class methods.**
  `Api.build`, for a concern `Api` whose `ClassMethods` defines `build`,
  resolved, where Ruby raises NoMethodError; so did the same on a concern
  that includes `Api`. A call on `self` in the concern's body still finds
  them, since its `included do` runs on the includer (DEC-105).

## 0.6.0 — 2026-09-28

- **Upgrading drops and rebuilds the index** (store v36): run `trekr --index`
  once per checkout.

### Added

- **Custom RSpec matchers.** `RSpec::Matchers.define :name` (and
  `define_negated_matcher`, `alias_matcher`) declares a matcher `name` that
  every spec can call, and `matcher :name` in a group body declares one for
  that group; a click on the matcher in an expectation now lands on the line
  that defines it (DEC-091).

- **Shared contexts across files.** The `let`s and methods of a top-level
  `shared_context` or `shared_examples` are found from any group that pulls it
  in with `include_context`, `include_examples` or `it_behaves_like`, wherever
  it is written — a helper in `spec/support` included by a spec no longer
  answers residue (DEC-092).

- **A symbol that names a method** resolves to it: the first symbol of
  `send`, `public_send`, `__send__`, `method`, `public_method` and
  `respond_to?` on their receiver (typed as any receiver is), and a symbol
  handed to a class-level call — `before_action :x`, `after_save :x`,
  `alias_method :new, :old`, `private :x` — on the class's instances. A
  click on the symbol answered residue (DEC-093).

- `items.map(&:name)` records `name` as a call on each element: a click on it
  found no name at all. The elements' class is known for a literal list of
  one class (`%w[a b].map(&:upcase)` is `String#upcase`); otherwise the
  answer is residue with the method's definitions as candidates (DEC-094).

- **A `let` is typed by what its block returns**, as an assignment is:
  `let(:widget) { Widget.new }` makes `widget.save` resolve to `Widget#save`,
  and `described_class.new` is the class the group describes. `subject`
  likewise, and `is_expected` expects it, so `is_expected.to be_empty` and
  `expect(widget).to be_valid` reach the predicate. In a hook or another
  `let`, a nested group's override of the `let` makes the answer `ambiguous`
  (DEC-096).

- **RSpec predicate matchers.** `be_empty`, `be_valid` and `have_key` have no
  method of their own; RSpec answers them by calling `empty?`, `valid?` and
  `has_key?` on the expectation's subject. Where `expect(x)` types `x`, the
  matcher now resolves to that predicate (`be_exist` to `exists?`, as RSpec
  falls back); otherwise the residue names the rule and offers the predicate's
  definitions (DEC-090).

- **A mixin sent to a class is its ancestor.** `Widget.include(Helpers)`,
  `Widget.prepend(Patch)`, `Widget.extend(Finder)` and `Widget.send(:include,
  Helpers)` add to `Widget`'s chain as the same line in its body would, so
  `Widget.new.help` resolves, and a `super` in a prepended module lands on the
  class's own method (DEC-097). Only a mixin that runs as its file loads
  counts: one under an `if`, inside a method or in a block may not have run.

- **`ActiveSupport.on_load` blocks mix into the class that runs the hook.**
  `ActiveSupport.on_load(:active_record) { include Tracking }` adds `Tracking`
  to whatever calls `ActiveSupport.run_load_hooks(:active_record, …)` — read
  from the indexed gems, not a table — so a gem's model methods and overrides
  resolve on every model (DEC-098). An `include` in such a block was
  previously credited to the class the block was written in.

- **`extend` and `def self.` inside a concern's `included do`** belong to the
  class that includes the concern, where its class-level macros already went:
  `included do extend ActiveModel::Naming end` gives every includer
  `model_name` (DEC-099).

### Fixed

- A Minitest spec's `it`, in a checkout that bundles rspec-core too,
  answered rspec-core's `it` with confidence 1, and every call in its blocks
  was looked up on RSpec's example group. A file that writes Minitest's
  expectations (`must_equal`, `wont_be`) or requires `minitest/spec` is no
  longer read as RSpec (DEC-095).

## 0.5.0 — 2026-09-27

- **Upgrading drops and rebuilds the index** (store v34): run `trekr --index`
  once per checkout.

### Added

- `rescue WidgetError => e` types `e` as a WidgetError, and a bare
  `rescue => e` as a StandardError, so `e.message` resolves (DEC-089).

- **RSpec.** A call inside a block RSpec runs — a `describe` or `context`
  body, an `it`, `before` or `let` — is looked up on RSpec's example group, in
  the rspec-core your bundle holds: `let`, `before`, `described_class` and
  `is_expected` now resolve where they answered residue. Without rspec-core in
  the index, the answer says that is what is missing (DEC-084).

- **`let`, `subject` and a group's own `def`** are methods of their example
  group: a click on a let's name in an example goes to the `let`, the
  innermost group's first, and a click on the symbol in `let(:name)` is the
  definition.

- A class method that hands its first parameter to `define_method` is read
  as a macro: `define_example_method :it` declares `it`, so `it`, `describe`
  and `context` in a spec land on rspec-core's own lines (DEC-085).

- A `def` inside `Target.class_eval do … end` (or `class_exec`,
  `module_eval`, `module_exec`) is `Target`'s method, also when the receiver
  is a parameter defaulting to a constant, as rspec defines `expect`, `allow`
  and `receive`. It was recorded on the scope around the block (DEC-086).

- `config.include Helpers` and `config.extend Macros` in `RSpec.configure`
  mix those modules into every example group, so a spec's helpers — FactoryBot's
  `create`, a support module's methods — resolve. A metadata filter on the
  include is not read (DEC-088).

- **`trekr --usage --misses`** lists the editor's definitions and hovers that
  came back empty or unsure — file, line, column, the token and trekr's reason
  — so a miss rate in `--usage` comes with the positions behind it. Kept in
  `lsp.log`, on this machine, and written after the answer is sent (DEC-083).

### Fixed

- A call on `self` whose method a subclass overrides is `ambiguous` in
  `--def` and the editor, naming the overrides, instead of resolved at
  confidence 1 to the base's method, which is not what runs for those
  subclasses (DEC-081).

- `RSpec.describe` answered minitest's `Kernel#describe` with confidence 1
  wherever minitest was in the bundle. It now answers RSpec's, from a stub of
  what rspec-core builds when a suite boots — as do `eq`, `be`, `raise_error`,
  `double` and the rest of `RSpec::Matchers` and rspec-mocks inside a spec,
  and `.to` after `expect(…)` or `is_expected` (DEC-087).

- A method with no return type takes one from an `.rbi` that declares it, as
  Sorbet does, so a chain through it is typed.

- A lambda or block that calls the variable holding it
  (`visit = lambda { … visit.call … }`) now finds that assignment; definition
  and hover on the inner `visit` came back empty.

- Core now has names real code reaches for and the stub lacked:
  `Float::INFINITY` and its siblings, `Process::CLOCK_MONOTONIC`,
  `Thread::Mutex` (and `Queue`, `SizedQueue`, `ConditionVariable`),
  `SystemCallError` as every `Errno` class's parent and the network `Errno`s,
  `Time.utc`/`gm`/`local`/`mktime`, `Kernel#__dir__`, `Module#using` and
  `Module#protected_instance_methods`. Each answered nothing before.

- On a compact `class A::B` or `module A::B`, a click on `B` found nothing,
  and a click on `A` answered `B`. `B` now answers the definition and `A` the
  namespace it is opened in, in the editor and in `--def`.

- A hover on a call trekr could not settle counts as `unsure` in `--usage`,
  as the definition at the same position does; it counted as a hit.

## 0.4.0 — 2026-09-27

- **Upgrading drops and rebuilds the index** (store v33): run `trekr --index`
  once per checkout, and restart any editor still running a trekr from
  before 0.3.0. Such an editor used to keep writing into the new store after
  an upgrade, and a file it saved kept its old facts through every reindex
  and `--drop`; it now fails to write instead.

### Changed

- **JSON fields are named the same in every command** (DEC-080). Update a
  consumer that reads the old names:
  - `--def`: `sites` is now `definition`, and it is always present (`[]` for
    a residue). `query` is what you typed; for a variable it was the
    absolute path.
  - `--ancestors`: `name` is now `query`, and `unresolved` is now
    `unresolved_ancestors`, as on the card.
  - `variants[].unresolved` (`--ancestors` and the card) is now
    `variants[].unresolved_ancestors`.
  - bare `--refs NAME` rows: `recv` is now `receiver`, `recv_text` is now
    `receiver_text`, as in `--refs Owner#m` and `--def`.
  - A Ruby core site is `path: "String.rb"` with `root` the `core/`
    directory beside the index, where it was `<core>/String.rb` with
    `root: null`: the stubs are written there, so it opens like any site.
    Text shows the file's full path. A consumer matching `<core>` should
    match that `root` instead.

- **An error's message under `--json` no longer starts with `trekr:`.** The
  prefix tells a terminal whose error it is; stderr keeps it. Outside a git
  checkout the message says so plainly instead of quoting `git rev-parse`.

- **Exit codes, where they were wrong.** A directory given to `--def` or
  `--symbols`, and a position with line or column `0` (`f.rb:0:0`), are
  usage errors (`64`), not `74` and `1`. `--usage` with `TREKR_USAGE=off`
  exits `1` with nothing recorded, not `64`: the command line was fine.
  `--usage` on an editor that only opened and closed reports its sessions
  instead of a bare heading.

- **A method query no Ruby could mean is a usage error** (`64`):
  `trekr 'Foo::Bar#baz#qux'`, `--refs widget#save`, `trekr thing.rb`. They
  answered "no indexed constant", which reads as a finding about the code.

- **A split name says it is N different classes**, not "declared with N
  different superclasses": one of them may be a `Struct.new` or a plain
  `class Post` in another program, with no superclass at all.

- **`--dead` weighs an untyped single caller as `lower` confidence**, with
  `caveat: untyped caller`: its only evidence of use is a call that may be
  another method's. It was `clear`.

- **`--dead` counts a file once**, however it is named: a path repeated, a
  directory and a file in it, a symlink and its target. Each doubled every
  candidate and inflated `scope`. Across checkouts, text writes every path
  whole, where it wrote each relative to the first scope's checkout.

- **`--explain` and `--context` take a bare position**: `trekr f.rb:12:5
  --explain` works, since a position is a `--def`. With anything else they
  are a usage error that says so, where clap's said `--def` was required.
  For a receiver typed by a call's return (`resolved_via: chain`,
  `chain:name`), `--explain` says that, not `receiver other → String`.

- **The "index format changed" note stops once anything is indexed.** After
  an upgrade it was attached to every not-indexed checkout for good,
  including ones never indexed and ones dropped since.

- **`--drop <gem dir>` drops that gem**, and `--index <gem dir>` says how to
  pick up an edit to a gem (`--drop` it, then index the app), where both
  failed with git's `rev-parse` error.

- **Completion after a dot leaves out private methods declared by name**:
  `private :helper` hid nothing, and Kernel's module functions (`puts`,
  `require`, `raise`, …) and BasicObject's `method_missing` were offered on
  every receiver. They are still offered bare, inside a method.

- **A definition's highlight in the editor spans the name written there.**
  It was the width of the name asked about, so `map` landing on `def collect`
  lit `col`, and a `delegate :each` lit `:eac`.

- **Hover's "Defined in" link follows a workspace opened through a
  symlink**, as definition locations already did; it opened the file a second
  time under its real path.

- **The editor says so when another trekr rebuilds the index under it**: one
  message on screen, where every request failed with an internal error and
  only `lsp.log` knew why. A core file that cannot be written out is logged
  (`core_files_failed`) at session start instead of silently landing nowhere.

- **A `method_missing` in the owner's chain makes a missing method
  `residue`, not `no_such_method`**: it answers any name. The card and
  `--refs Owner#m` say which class defines it.

### Added

- **`--dead` says why**: every row has a `reason` (text prints it after the
  name), and a `single-caller` row has `caller` — where the one call is, and
  its `tier`, so a confirmed caller and an untyped same-name call look
  different. When that caller is itself a candidate, the reason says so: one
  pass does not cascade. Rows also carry `end_line`.

- `--help` lists `TREKR_DB`, `TREKR_USAGE` and `TREKR_JOBS`, and says that
  `Owner#method` without `--refs` is a card, a summary, which is the `card`
  `--usage` counts. The README says where the index and its neighbours live.

- `--symbols` rows carry `path` and `root`, and `--dead` rows carry `col`,
  like every other located row.

- **`--index` says when there is no `Gemfile.lock`**, so no gem was indexed:
  a `gems — none` line in text, `gems.lockfile: false` in JSON.

### Fixed

- **A call on a constant is a call on what the constant names.** `NAMES =
  %w[a b]; NAMES.join` was excluded from `--refs Array#join` because `NAMES`
  was treated as a class that defines nothing; it is now possible. `Short =
  Router; Short.go` was excluded from `Router.go`; it is now confirmed.

- **A method a superclass calls on `self` is no longer reported dead.** When
  `Base#run` calls `setup` and `Child` overrides `setup`, `--refs Child#setup`
  excluded the call and `--dead` listed `Child#setup` as unreferenced at clear
  confidence. Such calls are now possible references. On rails, 337 methods
  (every validator's `validate_each`, the adapters' `configure_connection`)
  leave the dead list.

- **An engine's patch to an app class is not lost to a neighbouring engine's
  test class.** With `class User` reopened in one engine's `lib/` and declared
  plain in another engine's `test/`, the patch's methods went to the test
  class, so the app's calls found nothing and `--dead` reported them
  unreferenced.

- **In the editor, a gem's file is answered from your workspace's app.** With
  two apps bundling the same gem, definition, references and hover inside the
  gem answered from whichever app was indexed last, and kept doing so for the
  session.

- **Completion after a guessed chain says it is not the whole list.** When
  the receiver's type was one reading of several (`x.strip.` where the app
  also defines a `strip`), the editor was told the list was complete and did
  not ask again as you typed.

- **A class method's return type is not overruled by an instance method's.**
  `self.class.build.spin` resolved to `Gadget#spin` because an unrelated
  instance method `Kit#build` returns a Gadget, even though the class method
  `Builder.build` returns a Widget. A disagreement like that now leaves the
  call unresolved.

- **A qualified Sorbet return type keeps its namespace.**
  `sig { returns(Stripe::Customer) }` was read as `Customer`, so inside
  `module Billing` it typed the result as `Billing::Customer`, and `--dead`
  reported the real `Stripe::Customer` methods unreferenced. `::Item` now
  means the top-level `Item` too.

## 0.3.0 — 2026-09-27

- **Navigation works inside a gem's file in the editor.** After following a
  definition into a gem outside the checkout, definition, hover, references
  and completion answered nothing there, while `trekr --def` on the same
  position answered. The server now answers from the app whose bundle holds
  the gem, as the CLI does.

- **`--dead` with scopes in two checkouts weighs each against its own.**
  `trekr --dead lib ../worktree/lib` used the first path's checkout as the
  evidence for both, so the answer depended on argument order. A path that
  does not exist now exits 66 (`not_found`) instead of 1.

- **A `super` is a possible reference only to a method that could be behind
  it.** When a class's ancestors were not all indexed, every `super` in it
  counted as a possible call of any same-named method in the checkout, so
  `--dead` graded unrelated methods `super-only` (on activerecord alone,
  `Promise#pretty_print` from `CollectionProxy` and `Core`). An unindexed
  module can hide another module, and only an unindexed superclass can hide a
  class.

- **`--drop` also removes the checkout's cached trees.** A tree is cached
  under a key made from what the store says, so after a store answered
  wrongly, drop-then-index rebuilt the same key and kept the wrong tree.
  `trekr --drop` then `trekr --index` now repairs a checkout.

- **A Sorbet return type is looked up where the `sig` is written.** Inside
  `module Shop`, `sig { returns(Item) }` means Shop::Item, but a local
  assigned from that method (`i = order.item`) was typed from the caller's
  scope, and could land on a top-level `Item`.

- **Hover names a method as Ruby docs do**: `String#downcase(*options)`, not
  `def String#downcase(*options)`. Completion's detail line matches.

- **Upgrading drops and rebuilds the index** (store v32): run `trekr --index`
  once per checkout. A query on a checkout not yet reindexed now says the
  index format changed, rather than that the checkout was never indexed.

- **Two trekrs opening an older store at once rebuild it once.** Both used to
  drop and recreate it, so one failed with `table checkout already exists`,
  and the store could be left missing indexes or with files pointing at
  another repository's facts.

- **Two `--index` runs over the same files both succeed.** The second failed
  with `FOREIGN KEY constraint failed` when both parsed a file new to the
  store, or with `database is locked` when both created the store.

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
  `Session` and `CallbacksTest` in other gems' tests. One in the gem's `lib/`
  is still a reopen, since that is what other programs load: an engine's
  `class User` adding a method to the app's model.

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

- **A chain is typed from what the previous call returns.** Ruby core's stub
  now carries Ruby 3.4's return types, so `something.gsub(/x/, "").downcase`
  goes to `String#downcase` instead of offering Symbol's beside it, and
  `"x".upcase`, `1.minute` and `[…].flatten.to_set` resolve. When the previous
  call's receiver is untyped, every definition of its name is asked; the answer
  is `ambiguous` when some declare no return type. `resolved_via` is `chain` or
  `chain:name`. On rails, `--refs String#strip` confirms 109 sites, up from 3.
  A `sig` naming its block `NilClass` or several `sig`s on one method are read
  as overloads.

- **Core definitions read as their owner's signatures.** A core site's path is
  `<core>/String.rb` rather than `<core>` (`root` stays `null`), written
  beside the database as `core/String.rb`, and each stub's first line is the
  signature with Ruby's parameter names — an editor's peek list reads
  `String.rb  def downcase(*options)`. The `core.rb` earlier builds wrote
  there is removed.

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
