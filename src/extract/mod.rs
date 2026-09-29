//! Blob → facts. A pure function of bytes, and nothing else.
//!
//! Semantics are lifted from Shopify's Rubydex (MIT) — `docs/ruby-behaviors.md`
//! is the conformance spec and `rust/rubydex/src/indexing/ruby_indexer.rs` the
//! reference implementation. We do not depend on the crate (PLAN §8): its graph
//! is in-memory and its `MethodRef` carries a receiver only when it is a
//! constant, which is exactly the fact this engine is built to have.
//!
//! Traversal is Prism's `Visit` trait with a scope stack on `self`: push the
//! frame, call the free `ruby_prism::visit_*` to descend, pop. rwr generates a
//! 3.8k-line `children()` table instead, but it needs to compare and duplicate
//! trees; one-way extraction does not.

mod enums;
mod line_index;
mod macros;

pub(crate) use macros::{camelize, table_to_class};
mod sig;

use crate::core::*;
pub(crate) use line_index::LineIndex;
use ruby_prism::{Node, Visit};
use std::collections::{HashMap, HashSet};

/// An `ActiveSupport.on_load` block being read (DEC-098).
struct LoadHook {
    name: String,
    /// How many blocks were open, this one included.
    depth: usize,
    /// Registered as its file loads, so its mixins are recorded.
    modelled: bool,
    /// Called with the class instead of evaluated in it (`yield: true`), so
    /// `self` is the caller's.
    yields: bool,
    /// The block's parameter, which is the class either way (DEC-104).
    base: Option<String>,
    /// Where the block opens: its `def`s' module is declared there.
    at: usize,
}

/// A lexical scope in progress.
/// What kind of body a frame opened. It decides two different things that are
/// easy to conflate: whether a `def` inside it is a singleton method, and what
/// `self` is for a *call* inside it.
#[derive(Clone, Copy, PartialEq)]
enum Opens {
    /// A `class` or `module` body. `self` is the class.
    Scope,
    /// A `class << self` body. `self` is the class, and defs are singletons.
    Singleton,
    /// A `def` body. `self` is the class for `def self.x`, an instance
    /// otherwise.
    Method { singleton: bool },
}

struct Frame {
    /// Did this frame push a name onto the nesting stack? `class << self` does
    /// not — it renames nothing, it only flips what `def` means.
    pushed: bool,
    visibility: Visibility,
    /// Inside `class << self`, or `class << Foo`.
    singleton: bool,
    /// Is `self` the class here rather than an instance of it? True in a class
    /// or module body — which is why `validates :name` and `prepend Foo` are
    /// class-method calls — and inside `def self.x`, and false inside `def x`
    /// or at the top level, where `self` is `main`.
    self_is_class: bool,
    /// Inside a `def`, of either kind. Distinct from `self_is_class`, which a
    /// `def self.x` shares with a class body: what a mixin call means turns on
    /// *when* it runs, not on what `self` is.
    in_method: bool,
    /// `module_function` seen with no arguments: every later `def` in this body
    /// becomes both a private instance method and a public singleton one.
    module_function: bool,
    /// The method this body defines, which is the name a `super` in it looks
    /// up. Blocks push no frame, so a `super` inside one still finds it.
    method: Option<String>,
    /// Blocks open in this body. A `def` inside one — `Class.new do`,
    /// `class_eval do`, RSpec's `describe do` — lands on whatever the block is
    /// run against, which the source does not say.
    blocks: usize,
    /// The body of an RSpec example group (DEC-084).
    group: bool,
    /// The block of an example (`it`, `specify`), which runs in its own group
    /// only — unlike a hook's, a `let`'s or a method's, which a nested group
    /// runs too (DEC-096).
    example: bool,
    /// In a class method's body, its first parameter: the name a macro would
    /// hand to `define_method` (DEC-085).
    definer: Option<String>,
    /// In a method's body, its parameters that default to a constant.
    const_defaults: Vec<(String, String)>,
    /// In a method's body, what each of its parameters is of the names a
    /// macro's call hands it (DEC-162): the `k`th positional is `{k}`, a
    /// splat after them `{k*}`.
    handed: Vec<(String, String)>,
    /// A module's `included`/`extended`/`prepended` hook, or a `base.class_eval`
    /// body inside one: code that runs on whatever mixes the module in
    /// (DEC-102).
    mixed: Option<Mixed>,
}

/// Code that runs on whatever mixes its module in (DEC-102).
#[derive(Clone)]
struct Mixed {
    /// How the module is mixed in: `included` runs for an `include`.
    how: Relation,
    /// The hook's parameter, which holds the mixer. `None` in a
    /// `base.class_eval` body, where `self` does.
    base: Option<String>,
    /// The conditionals open around it: one inside it may not run.
    conditional: usize,
}

impl Frame {
    fn new(pushed: bool, opens: Opens) -> Frame {
        Frame {
            pushed,
            // A class or module body starts public; only the file scope is
            // private (Ruby's rule for top-level `def`).
            visibility: Visibility::Public,
            singleton: opens == Opens::Singleton,
            self_is_class: match opens {
                Opens::Scope | Opens::Singleton => true,
                Opens::Method { singleton } => singleton,
            },
            in_method: matches!(opens, Opens::Method { .. }),
            module_function: false,
            method: None,
            blocks: 0,
            group: false,
            example: false,
            definer: None,
            const_defaults: Vec::new(),
            handed: Vec::new(),
            mixed: None,
        }
    }
}

struct Extractor<'a> {
    src: &'a [u8],
    lines: LineIndex,
    nesting: Vec<String>,
    frames: Vec<Frame>,
    /// The Sorbet `sig`s immediately preceding the statement — several are
    /// overloads (DEC-077).
    pending_sigs: Vec<sig::Shape>,
    /// Parameter types from that same `sig`.
    pending_sig_params: Vec<(String, String)>,
    /// Constants in this blob assigned a literal array of symbols.
    ///
    /// Rails' single highest-yield delegation is
    /// `delegate(*QUERYING_METHODS, to: :all)` — roughly sixty of the most
    /// called class methods in any Rails app, and not one of them written as a
    /// literal argument. The names *are* literal, one indirection away, and
    /// that indirection is a pure function of the same bytes.
    symbol_arrays: HashMap<String, Vec<String>>,
    /// Block parameters currently bound to a known list of literals by an
    /// enclosing `[…].each do |v|`. A stack, because these nest.
    loop_values: Vec<(String, Vec<String>)>,
    /// Constants this file assigns a list every element of which is a
    /// literal name, by the scope they are assigned in: what `METHODS.each`
    /// iterates (DEC-131).
    name_arrays: HashMap<(Vec<String>, String), Vec<String>>,
    /// Constants in this blob assigned a literal array of constants.
    constant_arrays: HashMap<String, Vec<String>>,
    /// Block parameters bound to each of a literal list of constants by an
    /// enclosing `[Hash, Array].each do |klass|` (DEC-100).
    constant_loops: Vec<(String, Vec<String>)>,
    /// How many blocks were open, each included, for the blocks that run as
    /// their file loads though handed to a call: a literal list's iteration.
    iterations: Vec<usize>,
    /// How many `included do` blocks we are inside.
    included_depth: usize,
    /// Where the outermost `included do` we are in opens its block.
    included_at: Option<usize>,
    /// The hooks an `on_load` block's `def` has declared a module for.
    hook_modules: std::collections::HashSet<String>,
    /// The concerns `included do` has declared a `ClassMethods` for.
    routed_class_methods: std::collections::HashSet<Vec<String>>,
    /// Example groups named so far, by the nesting they were opened in: RSpec
    /// numbers a repeated name (`WhenValid_2`), and so does this.
    group_names: HashMap<Vec<String>, HashMap<String, usize>>,
    /// Class methods that define a method named by their first argument, by
    /// the nesting they are defined in: whether what they define is a class
    /// method (DEC-085).
    definers: HashMap<(Vec<String>, String), bool>,
    /// How many blocks deep we are in one that runs on the enclosing class's
    /// singleton class — `(class << self; self; end).module_exec do`.
    singleton_exec: usize,
    /// The block parameters of the `RSpec.configure do |config|` blocks we
    /// are in (DEC-088).
    configure_params: Vec<String>,
    /// How many conditionals we are inside — `if`, `unless`, `case`, `&&`,
    /// `||`. A mixin sent from one may never run (DEC-097).
    conditional: usize,
    /// The `ActiveSupport.on_load` blocks we are in (DEC-098).
    load_hooks: Vec<LoadHook>,
    /// The blocks we are in, innermost last: the call each is handed to, or
    /// `None` for one whose `self` is known — a method body, a class body, an
    /// RSpec group or example.
    open_blocks: Vec<Option<Pos>>,
    /// The matcher handed to an expectation, by where its name starts, and
    /// what the expectation was handed (DEC-090).
    matcher_subjects: HashMap<usize, Sent>,
    /// The file is a Minitest spec, whose bare `describe` is not RSpec's.
    minitest: bool,
    /// The class each group we are in describes, innermost last: its
    /// constant argument, or its parent's (`described_class`, DEC-096).
    described: Vec<Option<String>>,
    /// Inside a `scope`'s lambda, which runs on the model's relation (DEC-116).
    scope_body: usize,
    /// The strings of code being read in place of the file, innermost last
    /// (DEC-132). Every offset a node of one reports is into its text.
    evals: Vec<Eval>,
    /// The frame depth of each literal list's iteration open: blocks that
    /// leave `self` alone.
    loop_frames: Vec<usize>,
    /// Blocks open that are evaluated on some object other than `self` —
    /// `mod.singleton_class.instance_eval do` — whose `define_method` is
    /// not this scope's.
    foreign_evals: usize,
    /// Strings of code written in methods that interpolate only the
    /// method's positional parameters, by the method's name: read again
    /// where a class body in this file calls it with literal names (DEC-163).
    string_macros: HashMap<String, Vec<StringMacro>>,
    /// Methods read from such strings so far in this file (DEC-163).
    expanded: usize,
    /// Block variables iterating a macro's splat (`attrs.each do |name|`):
    /// each is one of the names the caller hands it (DEC-162).
    handed_loops: Vec<(String, String)>,
    /// Methods of each scope whose body is a string, as the shape of the
    /// names it spells (DEC-160).
    string_methods: HashMap<(Vec<String>, String), String>,
    /// Markers whose name is such a method's result, which may be written
    /// further down: the edge, the scope, the method.
    pending_shapes: Vec<(usize, Vec<String>, String)>,
    facts: Facts,
}

/// A `class_eval` string rendered as the code it evaluates, and how its bytes
/// map back to the file's (DEC-132).
struct Eval {
    src: Vec<u8>,
    /// Each piece's start in `src`, its start in the file, and whether its
    /// bytes are the file's own. A substituted value maps whole to the `#{`
    /// it replaced.
    pieces: Vec<(usize, usize, bool)>,
}

impl Eval {
    fn origin(&self, offset: usize) -> usize {
        let at = self
            .pieces
            .partition_point(|(start, _, _)| *start <= offset);
        match self.pieces.get(at.saturating_sub(1)) {
            Some((start, origin, true)) => origin + (offset - start),
            Some((_, origin, false)) => *origin,
            None => offset,
        }
    }
}

/// A piece of a string of code: bytes of the file, or an interpolation of a
/// local, maybe through one method that renders a name simply.
#[derive(Clone)]
enum Piece {
    Text {
        start: usize,
        end: usize,
    },
    Local {
        name: Vec<u8>,
        render: Option<String>,
        at: usize,
    },
}

/// What stands for a value no literal states while a string of code is read,
/// so that only what does not depend on it is kept. Never a real name.
const UNSTATED: &str = "trekr_unstated_name";

/// The methods an interpolated name may go through and still be rendered.
const RENDERS: [&str; 5] = ["to_s", "to_sym", "upcase", "downcase", "capitalize"];

/// What a name that stands for a call is really sent to: `x` in
/// `expect(x).to be_empty`, `obj` in `obj.send(:name)`, `self` for
/// `before_action :name`. Untyped when the source does not say —
/// `is_expected`, a matcher not handed to an expectation at all.
#[derive(Clone)]
struct Sent {
    recv: RecvShape,
    recv_text: Option<String>,
    recv_pos: Option<Pos>,
    recv_value: Option<RecvValue>,
    singleton: bool,
}

impl Sent {
    const UNTYPED: Sent = Sent {
        recv: RecvShape::Other,
        recv_text: None,
        recv_pos: None,
        recv_value: None,
        singleton: false,
    };

    /// The example's `subject`.
    const SUBJECT: Sent = Sent {
        recv_value: Some(RecvValue::Subject),
        ..Sent::UNTYPED
    };

    /// `self`, as the class (`singleton`) or an instance of it.
    fn to_self(singleton: bool) -> Sent {
        Sent {
            recv: RecvShape::Implicit,
            singleton,
            ..Sent::UNTYPED
        }
    }
}

/// Prism's syntax errors, with positions.
///
/// Free: the parse already happened. Syntax **only** — everything else this
/// engine knows is a ranked answer with a confidence, and publishing those as
/// diagnostics would turn disclosure into noise in an editor's gutter.
pub(crate) fn syntax_errors(src: &[u8]) -> Vec<(u32, u32, String)> {
    let parsed = ruby_prism::parse(src);
    let lines = line_index::LineIndex::new(src);
    parsed
        .errors()
        .map(|error| {
            let at = lines.pos(error.location().start_offset());
            (at.line, at.col, error.message().to_string())
        })
        .collect()
}

/// Read every fact a blob's bytes declare.
pub(crate) fn extract(src: &[u8]) -> Facts {
    let parsed = ruby_prism::parse(src);
    let lines = LineIndex::new(src);
    let mut ex = Extractor {
        src,
        facts: Facts {
            parse_errors: parsed.errors().count(),
            lines: lines.count(),
            source: Some(src.into()),
            ..Facts::default()
        },
        lines,
        nesting: Vec::new(),
        // The file scope: top-level `def` is private in Ruby.
        frames: vec![Frame {
            pushed: false,
            visibility: Visibility::Private,
            singleton: false,
            // At the top level `self` is `main`, an instance of Object.
            self_is_class: false,
            in_method: false,
            module_function: false,
            method: None,
            blocks: 0,
            group: false,
            example: false,
            definer: None,
            const_defaults: Vec::new(),
            handed: Vec::new(),
            mixed: None,
        }],
        pending_sigs: Vec::new(),
        pending_sig_params: Vec::new(),
        symbol_arrays: HashMap::new(),
        loop_values: Vec::new(),
        name_arrays: HashMap::new(),
        constant_arrays: HashMap::new(),
        constant_loops: Vec::new(),
        iterations: Vec::new(),
        included_depth: 0,
        included_at: None,
        routed_class_methods: std::collections::HashSet::new(),
        hook_modules: std::collections::HashSet::new(),
        group_names: HashMap::new(),
        definers: HashMap::new(),
        singleton_exec: 0,
        configure_params: Vec::new(),
        load_hooks: Vec::new(),
        conditional: 0,
        open_blocks: Vec::new(),
        matcher_subjects: HashMap::new(),
        minitest: minitest_spec(src, &parsed.node()),
        described: Vec::new(),
        scope_body: 0,
        evals: Vec::new(),
        loop_frames: Vec::new(),
        foreign_evals: 0,
        handed_loops: Vec::new(),
        string_macros: HashMap::new(),
        expanded: 0,
        string_methods: HashMap::new(),
        pending_shapes: Vec::new(),
    };
    ex.visit(&parsed.node());
    ex.shape_pending();
    ex.facts
}

impl<'a> Extractor<'a> {
    /// A marker named by a method of its scope that returns a string gets
    /// that string's shape, now every method is seen (DEC-160).
    fn shape_pending(&mut self) {
        for (edge, nesting, method) in std::mem::take(&mut self.pending_shapes) {
            let Some(shape) = self.string_methods.get(&(nesting, method)) else {
                continue;
            };
            let mut maker = Maker::parse(&self.facts.ancestry[edge].target);
            maker.shape = Some(shape.clone());
            self.facts.ancestry[edge].target = maker.encode();
        }
    }

    fn pos(&self, offset: usize) -> Pos {
        let offset = self.evals.last().map_or(offset, |eval| eval.origin(offset));
        self.lines.pos(offset)
    }

    fn text(&self, start: usize, end: usize) -> String {
        let src = self.evals.last().map_or(self.src, |eval| &eval.src[..]);
        String::from_utf8_lossy(&src[start..end.min(src.len())]).into_owned()
    }

    fn frame(&mut self) -> &mut Frame {
        self.frames.last_mut().expect("file frame is never popped")
    }

    fn visibility(&self) -> Visibility {
        self.frames
            .last()
            .map_or(Visibility::Public, |f| f.visibility)
    }

    fn in_singleton(&self) -> bool {
        self.frames.last().is_some_and(|f| f.singleton)
    }

    fn enter(&mut self, name: Option<String>, opens: Opens) {
        let pushed = name.is_some();
        if let Some(name) = name {
            self.nesting.insert(0, name);
        }
        self.frames.push(Frame::new(pushed, opens));
        // A frame's `self` is known, whatever block it sits in.
        self.open_blocks.push(None);
    }

    /// What `self` is for a call written here.
    fn self_is_class(&self) -> bool {
        self.frames.last().is_some_and(|f| f.self_is_class)
    }

    fn in_method_body(&self) -> bool {
        self.frames.last().is_some_and(|f| f.in_method)
    }

    fn in_group_body(&self) -> bool {
        self.frames.last().is_some_and(|f| f.group)
    }

    /// Does code written here run when its file loads? A runtime mixin is
    /// recorded only then (DEC-097), since an edge that may not exist misleads
    /// worse than a missing one. `Patch.prepend(Fix) if defined?(Patch)` is
    /// how an optional integration is written, and sorbet-runtime prepends to
    /// rspec-core's `let` that way only if rspec-core loaded first. A mixin
    /// in a method runs if the method is called: activerecord's
    /// `install_support` includes its encryption queries into every model
    /// only when deterministic encryption is configured. And one in a block
    /// runs when the block's receiver decides, after whatever ran before it:
    /// a Railtie's `initializer do`. An `on_load` block counts as its class's
    /// own file loading, which is when the hook runs it, and so does a
    /// literal list's iteration, which runs where it is written (DEC-100).
    fn runs_as_file_loads(&self) -> bool {
        let loads = |at: usize| {
            self.iterations.contains(&(at + 1))
                || self
                    .load_hooks
                    .iter()
                    .any(|hook| hook.modelled && hook.depth == at + 1)
        };
        self.conditional == 0
            && !self.in_method_body()
            && self
                .open_blocks
                .iter()
                .enumerate()
                .all(|(at, block)| block.is_none() || loads(at))
    }

    /// The hook whose `on_load` block this is, directly: a block or a body
    /// inside it runs as something else.
    fn in_load_hook(&self) -> Option<&LoadHook> {
        self.load_hooks
            .last()
            .filter(|hook| hook.depth == self.open_blocks.len())
    }

    /// The module an `on_load` block's `def`s go to, declared with the edge
    /// that prepends it to the hooked classes the first time (DEC-104).
    fn hook_module(&mut self) -> Option<String> {
        let hook = self
            .in_load_hook()
            .filter(|hook| hook.modelled && !hook.yields && self.runs_as_file_loads())?;
        let (name, at) = (hook.name.clone(), hook.at);
        let module = format!("on_load(:{name})");
        if self.hook_modules.insert(name.clone()) {
            let mut decl = self.def(format!("::{module}"), Kind::Module, at, at);
            decl.nesting.clear();
            decl.via = Some("on_load".to_string());
            self.facts.defs.push(decl);
            let mut owner = vec![crate::core::runtime::hook(&name)];
            owner.extend(self.nesting.iter().cloned());
            let pos = self.pos(at);
            self.facts.ancestry.push(Ancestry {
                owner,
                relation: Relation::Prepend,
                target: format!("::{module}"),
                pos,
            });
        }
        Some(module)
    }

    /// The hook whose block's parameter this receiver is (DEC-104).
    fn load_hook_base(&self, receiver: &Node<'_>) -> Option<&LoadHook> {
        let read = receiver.as_local_variable_read_node()?;
        self.in_load_hook()
            .filter(|hook| hook.base.as_deref().map(str::as_bytes) == Some(read.name().as_slice()))
    }

    /// Is `self` the class or module whose body this is — not a method, a
    /// singleton, or a block that may run elsewhere?
    fn self_is_the_scope(&self) -> bool {
        !self.nesting.is_empty()
            && !self.in_group_body()
            && self.open_blocks.last() == Some(&None)
            && self
                .frames
                .last()
                .is_some_and(|f| !f.in_method && !f.singleton)
    }

    /// Inside `included do … end` of a module that extends `ActiveSupport::Concern`.
    ///
    /// The block is `class_eval`'d into whatever includes the module, so a
    /// class-level macro written here defines methods on **every includer's
    /// singleton** — the same destination Concern gives `ClassMethods`, which
    /// is why routing them there is a restatement rather than an invention.
    /// Gated on the concern because a bare `included do` in some other DSL
    /// makes no such promise.
    fn in_concerns_included_block(&self) -> bool {
        self.included_depth > 0
            && self.facts.ancestry.iter().any(|edge| {
                edge.relation == Relation::Extend
                    && edge.owner == self.nesting
                    && edge.target.ends_with("Concern")
            })
    }

    /// The code here runs on whatever mixes its module in, and runs whenever
    /// that happens: directly in the hook or its `base.class_eval`, under no
    /// condition of its own.
    fn mixed(&self) -> Option<&Mixed> {
        let frame = self.frames.last()?;
        let mixed = frame.mixed.as_ref()?;
        (frame.blocks == 0 && self.conditional == mixed.conditional).then_some(mixed)
    }

    /// A `base.class_eval` body in a hook: a body of the mixer, not the
    /// module. How the module is mixed in, when it is one.
    fn includer_how(&self) -> Option<Relation> {
        self.mixed()
            .filter(|mixed| mixed.base.is_none())
            .map(|mixed| mixed.how)
    }

    /// Code that runs as a body of whatever mixes its module in: a concern's
    /// `included do`, or a hook's `base.class_eval do`.
    fn in_includer_body(&self) -> bool {
        self.in_concerns_included_block() || self.includer_how().is_some()
    }

    /// The owner of an edge that lands on whatever mixes this module in by
    /// `how` (DEC-102).
    fn mixed_owner(&self, how: Relation) -> Vec<String> {
        let mut owner = vec![crate::core::runtime::mixed(how.as_str())];
        owner.extend(self.nesting.iter().cloned());
        owner
    }

    /// Declare the concern's `ClassMethods` that `included do` routes into,
    /// once per concern: it may be written nowhere else, and the module has
    /// to exist for the tree to carry what is routed there.
    fn declare_class_methods(&mut self) {
        if !self.routed_class_methods.insert(self.nesting.clone()) {
            return;
        }
        // A concern's `ClassMethods` reaches its includers by Concern's own
        // rule; a hook's has to be sent there, as `base.extend` would.
        if let Some(how) = self.includer_how() {
            let pos = self.pos(self.included_at.unwrap_or_default());
            self.facts.ancestry.push(Ancestry {
                owner: self.mixed_owner(how),
                relation: Relation::Extend,
                target: "ClassMethods".to_string(),
                pos,
            });
        }
        let at = self.included_at.unwrap_or_default();
        let mut module = self.def("ClassMethods".to_string(), Kind::Module, at, at);
        module.via = Some("included".to_string());
        self.push_def(module);
    }

    /// A class-level macro in an includer body — a concern's `included do`, or
    /// a hook's `base.class_eval` — runs against the *includer*, so a class
    /// method it makes belongs where Concern puts an includer's class methods.
    fn route_to_includer(&mut self, def: &mut Def) {
        if def.singleton && self.in_includer_body() {
            def.nesting.insert(0, "ClassMethods".to_string());
            def.singleton = false;
            self.declare_class_methods();
        }
    }

    fn leave(&mut self) {
        self.open_blocks.pop();
        if let Some(frame) = self.frames.pop()
            && frame.pushed
        {
            self.nesting.remove(0);
        }
    }

    fn push_def(&mut self, mut def: Def) {
        // Only what a group's body writes is the group's (DEC-084). A `def`
        // in a block inside it — `Class.new do`, an example — belongs to
        // whatever that block runs on, and keeps the file's nesting.
        if def.is_group_member() && !self.writes_group_members() {
            def.nesting
                .retain(|scope| !crate::core::rspec::is_group(scope));
        }
        // A top-level shared group's own methods are its module's, which
        // other files include (DEC-092).
        if def.is_group_member()
            && let Some(module) = def
                .nesting
                .first()
                .and_then(|segment| crate::core::rspec::shared_module_of(segment))
        {
            def.nesting = vec![format!("::{module}")];
        }
        self.facts.defs.push(def);
    }

    fn writes_group_members(&self) -> bool {
        self.frames.last().is_some_and(|f| f.group && f.blocks == 0)
    }

    /// A definition with this blob's current nesting and the common defaults.
    fn def(&self, name: String, kind: Kind, start: usize, end: usize) -> Def {
        Def {
            name,
            kind,
            nesting: self.nesting.clone(),
            singleton: false,
            visibility: Visibility::Public,
            params: Vec::new(),
            via: None,
            target: None,
            sig_returns: None,
            target_pos: None,
            sig_overloads: Vec::new(),
            sig_params: Vec::new(),
            value: None,
            pos: self.pos(start),
            end_line: self.pos(end).line,
        }
    }
}

/// A constant path exactly as written: `Foo`, `A::B`, `::Foo`. `None` when any
/// segment is dynamic (`foo::Bar`) — there is no name to record.
fn const_name(node: &Node<'_>) -> Option<String> {
    if let Some(read) = node.as_constant_read_node() {
        return String::from_utf8(read.name().as_slice().to_vec()).ok();
    }
    path_name(&node.as_constant_path_node()?)
}

fn path_name(path: &ruby_prism::ConstantPathNode<'_>) -> Option<String> {
    let last = String::from_utf8(path.name()?.as_slice().to_vec()).ok()?;
    match path.parent() {
        // `::Foo` — rooted at Object, but lexical nesting still applies.
        None => Some(format!("::{last}")),
        Some(parent) => Some(format!("{}::{last}", const_name(&parent)?)),
    }
}

/// Every prefix of a written constant path, with the offset of the segment that
/// ends it: `Foo::Bar` is a lookup of `Foo` and then of `Foo::Bar`, and
/// go-to-definition on either segment has to land somewhere.
fn path_prefixes(path: &ruby_prism::ConstantPathNode<'_>, out: &mut Vec<(String, usize)>) {
    if let Some(parent) = path.parent() {
        if let Some(inner) = parent.as_constant_path_node() {
            path_prefixes(&inner, out);
        } else if let Some(read) = parent.as_constant_read_node()
            && let Ok(name) = String::from_utf8(read.name().as_slice().to_vec())
        {
            out.push((name, parent.location().start_offset()));
        }
    }
    if let Some(full) = path_name(path) {
        let offset = path.name_loc().start_offset();
        out.push((full, offset));
    }
}

/// Is a keyword present in a macro's trailing options hash?
/// The literal class name a keyword names — `class_name: "Widget"`, `to: :all`.
/// `None` when absent or computed, which is how a caller refuses to guess.
fn keyword_literal(args: &[Node<'_>], key: &str) -> Option<String> {
    let value = keyword_value(args, key)?;
    literal_name(&value).or_else(|| const_name(&value))
}

fn keyword_value<'pr>(args: &[Node<'pr>], key: &str) -> Option<Node<'pr>> {
    for arg in args {
        let Some(hash) = arg.as_keyword_hash_node() else {
            continue;
        };
        for element in hash.elements().iter() {
            let Some(assoc) = element.as_assoc_node() else {
                continue;
            };
            let Some(symbol) = assoc.key().as_symbol_node() else {
                continue;
            };
            if symbol.unescaped() == key.as_bytes() {
                return Some(assoc.value());
            }
        }
    }
    None
}

/// The literal text of a symbol or string argument (`:foo`, `"foo"`).
fn literal_name(node: &Node<'_>) -> Option<String> {
    if let Some(sym) = node.as_symbol_node() {
        return String::from_utf8(sym.unescaped().to_vec()).ok();
    }
    let string = node.as_string_node()?;
    String::from_utf8(string.unescaped().to_vec()).ok()
}

fn arg_nodes<'pr>(call: &ruby_prism::CallNode<'pr>) -> Vec<Node<'pr>> {
    call.arguments()
        .map(|a| a.arguments().iter().collect())
        .unwrap_or_default()
}

/// Is the receiver absent or a literal `self`? Every definition-creating macro
/// (`attr_*`, `include`, `private`) only counts in that position.
fn on_self(call: &ruby_prism::CallNode<'_>) -> bool {
    match call.receiver() {
        None => true,
        Some(r) => r.as_self_node().is_some(),
    }
}

/// A call that defines a method by name, whoever it is sent to.
struct Definer<'pr> {
    /// `define_method` or `define_singleton_method`.
    name: String,
    /// Its arguments, after the name `send` is handed.
    args: Vec<Node<'pr>>,
    on: DefinedOn,
}

enum DefinedOn {
    /// `define_method(…)`, `self.send(:define_method, …)`.
    Own,
    /// `singleton_class.define_method(…)`.
    OwnSingleton,
    /// `Target.define_method(…)`, `Target.send(:define_method, …)`.
    Constant(String),
}

/// `define_method(…)` and the ways of sending it: to `singleton_class`, to a
/// constant, or by `send`. `None` for another receiver, which names no class.
fn definer<'pr>(call: &ruby_prism::CallNode<'pr>) -> Option<Definer<'pr>> {
    let mut name = method_name(call)?;
    let mut args = arg_nodes(call);
    if matches!(name.as_str(), "send" | "__send__" | "public_send") {
        name = literal_name(args.first()?)?;
        args.remove(0);
    }
    if !matches!(name.as_str(), "define_method" | "define_singleton_method") {
        return None;
    }
    let on = match call.receiver() {
        None => DefinedOn::Own,
        Some(r) if r.as_self_node().is_some() => DefinedOn::Own,
        Some(r)
            if r.as_call_node().is_some_and(|c| {
                on_self(&c)
                    && method_name(&c).as_deref() == Some("singleton_class")
                    && c.arguments().is_none()
            }) =>
        {
            DefinedOn::OwnSingleton
        }
        Some(r) => DefinedOn::Constant(const_name(&r)?),
    };
    Some(Definer { name, args, on })
}

/// The method a name argument calls on `self` to build the name:
/// `define_method(_renderer_name(key))`.
fn shaping_call(node: &Node<'_>) -> Option<String> {
    let call = node.as_call_node()?;
    on_self(&call).then(|| method_name(&call)).flatten()
}

/// What each parameter of a method is of the names its caller hands it:
/// `{k}` for the `k`th positional, `{k*}` for a splat after `k` of them.
fn handed_params(params: &[Param]) -> Vec<(String, String)> {
    let mut handed = Vec::new();
    let mut k = 0;
    for param in params {
        match param.kind {
            ParamKind::Req | ParamKind::Opt => {
                handed.push((param.name.clone(), format!("{{{k}}}")));
                k += 1;
            }
            ParamKind::Rest => {
                handed.push((param.name.clone(), format!("{{{k}*}}")));
                break;
            }
            _ => break,
        }
    }
    handed
}

/// An interpolation in a macro's string: `{k}` when it is the macro's `k`th
/// positional parameter (through a method that renders a name simply),
/// `*` otherwise.
fn handed_part(part: &Node<'_>, handed: &[(String, String)]) -> String {
    let local = part
        .as_embedded_statements_node()
        .and_then(|e| e.statements())
        .and_then(|s| {
            let body: Vec<Node<'_>> = s.body().iter().collect();
            let [only] = body.as_slice() else { return None };
            match only.as_call_node() {
                Some(call)
                    if method_name(&call).is_some_and(|m| RENDERS.contains(&m.as_str()))
                        && call.arguments().is_none() =>
                {
                    call.receiver()?.as_local_variable_read_node()
                }
                Some(_) => None,
                None => only.as_local_variable_read_node(),
            }
            .map(|read| read.name().as_slice().to_vec())
        });
    local
        .and_then(|name| handed.iter().rev().find(|(p, _)| p.as_bytes() == name))
        .map_or_else(|| "*".to_string(), |(_, template)| template.clone())
}

/// A macro's name argument as a shape: its `k`th parameter is `{k}`,
/// an interpolation of one `{k}` in the spelled text (DEC-162).
fn handed_shape(node: &Node<'_>, handed: &[(String, String)]) -> Option<String> {
    if let Some(read) = node.as_local_variable_read_node() {
        return handed
            .iter()
            .rev()
            .find(|(p, _)| p.as_bytes() == read.name().as_slice())
            .map(|(_, template)| template.clone());
    }
    let parts: Vec<Node<'_>> = if let Some(string) = node.as_interpolated_string_node() {
        string.parts().iter().collect()
    } else {
        node.as_interpolated_symbol_node()?.parts().iter().collect()
    };
    let mut shape = String::new();
    for part in &parts {
        match part.as_string_node() {
            Some(text) => shape.push_str(std::str::from_utf8(text.unescaped()).ok()?),
            None => shape.push_str(&handed_part(part, handed)),
        }
    }
    shape.chars().any(|c| c != '*').then_some(shape)
}

/// A method body that is one string, as the shape of the names it spells.
fn string_shape(body: &Node<'_>) -> Option<String> {
    let statements = body.as_statements_node()?;
    let mut statements = statements.body().iter();
    let only = statements.next()?;
    if statements.next().is_some() {
        return None;
    }
    if let Some(name) = literal_name(&only) {
        return Some(name);
    }
    let parts: Vec<Node<'_>> = only.as_interpolated_string_node()?.parts().iter().collect();
    shape_of(&parts)
}

/// A string's parts as a name shape: the text, `*` for each interpolation.
/// `None` unless some text is spelled.
fn shape_of(parts: &[Node<'_>]) -> Option<String> {
    let mut shape = String::new();
    for part in parts {
        match part.as_string_node() {
            Some(text) => shape.push_str(std::str::from_utf8(text.unescaped()).ok()?),
            None if shape.ends_with('*') => {}
            None => shape.push('*'),
        }
    }
    shape.chars().any(|c| c != '*').then_some(shape)
}

/// A `def`'s first required parameter's name.
fn def_first_param(node: &ruby_prism::DefNode<'_>) -> Option<String> {
    let first = node.parameters()?.requireds().iter().next()?;
    let param = first.as_required_parameter_node()?;
    String::from_utf8(param.name().as_slice().to_vec()).ok()
}

/// `RSpec.configure do |config|` — the name its block gives the
/// configuration.
fn rspec_configure_param(call: &ruby_prism::CallNode<'_>) -> Option<String> {
    if method_name(call).as_deref() != Some("configure")
        || call
            .receiver()
            .and_then(|r| const_name(&r))
            .is_none_or(|r| r.trim_start_matches("::") != "RSpec")
    {
        return None;
    }
    let params = call
        .block()?
        .as_block_node()?
        .parameters()?
        .as_block_parameters_node()?
        .parameters()?;
    let first = params.requireds().iter().next()?;
    let param = first.as_required_parameter_node()?;
    String::from_utf8(param.name().as_slice().to_vec()).ok()
}

/// A `def`'s parameters whose default is a constant: `host = ::Host`.
fn const_defaults(node: &ruby_prism::DefNode<'_>) -> Vec<(String, String)> {
    let Some(params) = node.parameters() else {
        return Vec::new();
    };
    params
        .optionals()
        .iter()
        .filter_map(|p| {
            let p = p.as_optional_parameter_node()?;
            let name = String::from_utf8(p.name().as_slice().to_vec()).ok()?;
            Some((name, const_name(&p.value())?))
        })
        .collect()
}

/// A block evaluated with some object other than `self` as its `self`:
/// `mod.class_eval do`, `x.singleton_class.instance_eval do`.
fn evaluated_elsewhere(call: &ruby_prism::CallNode<'_>) -> bool {
    // `Class.new(self) { define_method … }` defines on the new class.
    let makes = method_name(call).as_deref() == Some("new")
        && call
            .receiver()
            .and_then(|r| const_name(&r))
            .is_some_and(|r| {
                matches!(
                    r.trim_start_matches("::"),
                    "Class" | "Module" | "Struct" | "Data"
                )
            });
    let evaluates = method_name(call).is_some_and(|name| {
        matches!(
            name.as_str(),
            "module_exec"
                | "class_exec"
                | "module_eval"
                | "class_eval"
                | "instance_eval"
                | "instance_exec"
        )
    });
    makes || evaluates && !on_self(call)
}

/// `(class << self; self; end).module_exec do` or `singleton_class.class_eval
/// do`: a block whose `define_method` defines a class method.
fn runs_on_singleton_class(call: &ruby_prism::CallNode<'_>) -> bool {
    let evaluates = method_name(call).is_some_and(|name| {
        matches!(
            name.as_str(),
            "module_exec"
                | "class_exec"
                | "module_eval"
                | "class_eval"
                | "instance_eval"
                | "instance_exec"
        )
    });
    let Some(receiver) = call.receiver() else {
        return false;
    };
    let own_singleton = if let Some(parens) = receiver.as_parentheses_node() {
        parens
            .body()
            .and_then(|b| b.as_statements_node())
            .and_then(|s| s.body().iter().next())
            .and_then(|n| n.as_singleton_class_node())
            .is_some_and(|n| n.expression().as_self_node().is_some())
    } else {
        receiver
            .as_call_node()
            .is_some_and(|c| on_self(&c) && method_name(&c).as_deref() == Some("singleton_class"))
    };
    evaluates && own_singleton
}

fn params_of(node: Option<ruby_prism::ParametersNode<'_>>) -> Vec<Param> {
    let mut out = Vec::new();
    let Some(params) = node else { return out };
    let mut push = |kind: ParamKind, name: String| out.push(Param { kind, name });

    for p in params.requireds().iter() {
        let name = p
            .as_required_parameter_node()
            .and_then(|n| String::from_utf8(n.name().as_slice().to_vec()).ok());
        // A destructuring parameter (`def f((a, b))`) has no name of its own.
        push(ParamKind::Req, name.unwrap_or_else(|| "_".into()));
    }
    for p in params.optionals().iter() {
        if let Some(n) = p.as_optional_parameter_node()
            && let Ok(name) = String::from_utf8(n.name().as_slice().to_vec())
        {
            push(ParamKind::Opt, name);
        }
    }
    if let Some(rest) = params.rest()
        && let Some(n) = rest.as_rest_parameter_node()
    {
        // Anonymous `*` forwards positionally but names nothing.
        let name = n
            .name()
            .and_then(|c| String::from_utf8(c.as_slice().to_vec()).ok());
        push(ParamKind::Rest, name.unwrap_or_else(|| "*".into()));
    }
    for p in params.posts().iter() {
        let name = p
            .as_required_parameter_node()
            .and_then(|n| String::from_utf8(n.name().as_slice().to_vec()).ok());
        push(ParamKind::Post, name.unwrap_or_else(|| "_".into()));
    }
    for p in params.keywords().iter() {
        if let Some(n) = p.as_required_keyword_parameter_node() {
            if let Ok(name) = String::from_utf8(n.name().as_slice().to_vec()) {
                push(ParamKind::Keyreq, name);
            }
        } else if let Some(n) = p.as_optional_keyword_parameter_node()
            && let Ok(name) = String::from_utf8(n.name().as_slice().to_vec())
        {
            push(ParamKind::Key, name);
        }
    }
    if let Some(rest) = params.keyword_rest() {
        if let Some(n) = rest.as_keyword_rest_parameter_node() {
            let name = n
                .name()
                .and_then(|c| String::from_utf8(c.as_slice().to_vec()).ok());
            push(ParamKind::Keyrest, name.unwrap_or_else(|| "**".into()));
        } else if rest.as_forwarding_parameter_node().is_some() {
            push(ParamKind::Rest, "...".into());
        } else if rest.as_no_keywords_parameter_node().is_some() {
            // `**nil` — the method accepts no keywords at all.
            push(ParamKind::Nokey, "nil".into());
        }
    }
    if let Some(n) = params.block() {
        let name = n
            .name()
            .and_then(|c| String::from_utf8(c.as_slice().to_vec()).ok());
        push(ParamKind::Block, name.unwrap_or_else(|| "&".into()));
    }
    out
}

impl<'pr> Visit<'pr> for Extractor<'_> {
    /// Ruby's statement sequence is also where a Sorbet `sig` finds the thing
    /// it describes: the two are always adjacent statements. Walking the list
    /// here — rather than descending blindly — is what makes the pairing free.
    fn visit_statements_node(&mut self, node: &ruby_prism::StatementsNode<'pr>) {
        let body: Vec<Node<'pr>> = node.body().iter().collect();
        for (i, stmt) in body.iter().enumerate() {
            let previous = i.checked_sub(1).map(|p| &body[p]);
            self.pending_sigs = body[..i]
                .iter()
                .rev()
                .map_while(sig::shape)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            self.pending_sig_params = previous.map(sig::params).unwrap_or_default();
            self.visit(stmt);
        }
        self.pending_sigs.clear();
        self.pending_sig_params.clear();
    }

    fn visit_class_node(&mut self, node: &ruby_prism::ClassNode<'pr>) {
        let path = node.constant_path();
        let Some(name) = const_name(&path) else {
            return; // dynamic constant path — nothing nameable to record
        };

        // The superclass is evaluated in the OUTER nesting, so record it before
        // the frame is pushed.
        if let Some(sup) = node.superclass() {
            // `class Foo < Struct.new(:a)` and `class M < AR::Migration[7.0]`
            // both name their real parent as the call's receiver.
            let named = const_name(&sup).or_else(|| {
                sup.as_call_node()
                    .and_then(|c| c.receiver())
                    .and_then(|r| const_name(&r))
            });
            // Anything else — `DelegateClass(Base)` — is a parent no constant
            // names. Recorded as written, so the tree reports an ancestor it
            // cannot see rather than giving the class a bare `Object` chain.
            let named = named.or_else(|| {
                let loc = sup.location();
                let text = self.text(loc.start_offset(), loc.end_offset());
                Some(text.split_whitespace().collect::<Vec<_>>().join(" "))
            });
            if let Some(target) = named {
                let pos = self.pos(sup.location().start_offset());
                // The owner is the class being opened, so it is recorded even
                // though the frame for it does not exist yet.
                let mut owner = self.nesting.clone();
                owner.insert(0, name.clone());
                self.facts.ancestry.push(Ancestry {
                    owner,
                    relation: Relation::Superclass,
                    target,
                    pos,
                });
            }
            self.visit(&sup);
        }

        let loc = node.location();
        let name_start = path.location().start_offset();
        let mut def = self.def(name.clone(), Kind::Class, name_start, loc.end_offset());
        def.pos = self.pos(name_start);
        self.push_def(def);

        // Compact `class Foo::Bar` opens ONE lexical scope, not two: Ruby's
        // `Module.nesting` is `[Foo::Bar]`, so constants inside cannot see
        // `Foo`'s. Pushing the written path whole is what preserves that.
        self.enter(Some(name), Opens::Scope);
        if let Some(body) = node.body() {
            self.visit(&body);
        }
        self.leave();
    }

    fn visit_module_node(&mut self, node: &ruby_prism::ModuleNode<'pr>) {
        let path = node.constant_path();
        let Some(name) = const_name(&path) else {
            return;
        };
        let loc = node.location();
        let def = self.def(
            name.clone(),
            Kind::Module,
            path.location().start_offset(),
            loc.end_offset(),
        );
        self.push_def(def);

        self.enter(Some(name), Opens::Scope);
        if let Some(body) = node.body() {
            self.visit(&body);
        }
        self.leave();
    }

    fn visit_singleton_class_node(&mut self, node: &ruby_prism::SingletonClassNode<'pr>) {
        let expr = node.expression();
        // `class << self` renames nothing — it only makes every `def` inside a
        // singleton method. `class << Foo` additionally moves the owner, and
        // pushing `Foo` is all it takes to say so.
        let attached = const_name(&expr);
        if attached.is_some() {
            self.visit(&expr);
        }
        self.enter(attached, Opens::Singleton);
        if let Some(body) = node.body() {
            self.visit(&body);
        }
        self.leave();
    }

    fn visit_def_node(&mut self, node: &ruby_prism::DefNode<'pr>) {
        let Ok(name) = String::from_utf8(node.name().as_slice().to_vec()) else {
            return;
        };
        let receiver = node.receiver();
        // Singleton either by writing it (`def self.x`, `def Foo.x`) or by
        // sitting inside `class << self`.
        let singleton = receiver.is_some() || self.in_singleton();
        let loc = node.location();
        let name_start = node.name_loc().start_offset();

        let mut def = self.def(name.clone(), Kind::Method, name_start, loc.end_offset());
        def.singleton = singleton;
        def.params = params_of(node.parameters());
        let (returns, overloads) =
            sig::resolve(&std::mem::take(&mut self.pending_sigs), &def.params);
        def.sig_returns = returns;
        def.sig_overloads = overloads;
        // A custom `new` says what it makes by what it ends with (DEC-133).
        if name == "new" && def.sig_returns.is_none() && def.sig_overloads.is_empty() {
            def.sig_returns = node.body().and_then(|body| made_by_new(&body));
        }
        def.sig_params = std::mem::take(&mut self.pending_sig_params);
        // Visibility modifiers never reach `def self.x` — it is public whatever
        // the enclosing `private` says.
        def.visibility = if singleton {
            Visibility::Public
        } else {
            self.visibility()
        };
        if let Some(r) = receiver.as_ref()
            && r.as_self_node().is_none()
        {
            def.target = const_name(r);
        }

        // `def self.x` inside `included do` is the includer's class method,
        // where Concern puts `ClassMethods`' (and the macros above do).
        let on_self = receiver.as_ref().is_none_or(|r| r.as_self_node().is_some());
        if singleton && on_self && self.in_includer_body() {
            def.nesting.insert(0, "ClassMethods".to_string());
            def.singleton = false;
            self.declare_class_methods();
        }

        // A `def` in an `on_load` block defines on the hooked class, after
        // the class body has: a module `on_load(:name)` the class prepends
        // (DEC-104).
        if !singleton && let Some(module) = self.hook_module() {
            def.nesting = vec![format!("::{module}")];
        }

        let module_function = self.frames.last().is_some_and(|f| f.module_function);
        if module_function && !singleton {
            // `module_function` makes one `def` into two methods: a public
            // singleton copy and a private instance one. Emitting both here
            // means no later layer has to know the macro exists.
            let mut copy = def.clone();
            copy.singleton = true;
            copy.visibility = Visibility::Public;
            copy.via = Some("module_function".into());
            self.push_def(copy);
            def.visibility = Visibility::Private;
        }
        self.push_def(def);

        // `super` looks up this name after the method's owner, so it is only
        // recorded where the owner is the scope: not `def obj.x`, and not a
        // `def` inside a block, whose owner is decided when the block runs.
        let owner_is_scope = receiver.as_ref().is_none_or(|r| r.as_self_node().is_some())
            && self.frames.last().is_some_and(|f| f.blocks == 0);

        // Descend for calls and constants in the body — but not through a
        // receiver we already recorded.
        let definer = (singleton && owner_is_scope)
            .then(|| def_first_param(node))
            .flatten();
        // `def self.included(base)`, in a body rather than a method or block.
        let mixed =
            (singleton && owner_is_scope && !self.in_method_body() && !self.nesting.is_empty())
                .then(|| mixin_hook(&name))
                .flatten()
                .and_then(|how| {
                    Some(Mixed {
                        how,
                        base: Some(def_first_param(node)?),
                        conditional: self.conditional,
                    })
                });
        if let Some(shape) = node.body().and_then(|body| string_shape(&body)) {
            self.string_methods
                .insert((self.nesting.clone(), name.clone()), shape);
        }
        self.enter(None, Opens::Method { singleton });
        self.frame().mixed = mixed;
        self.frame().method = owner_is_scope.then_some(name);
        self.frame().definer = definer;
        self.frame().const_defaults = const_defaults(node);
        self.frame().handed = handed_params(&params_of(node.parameters()));
        if let Some(params) = node.parameters() {
            self.visit_parameters_node(&params);
        }
        if let Some(body) = node.body() {
            self.visit(&body);
        }
        self.leave();
    }

    fn visit_constant_read_node(&mut self, node: &ruby_prism::ConstantReadNode<'pr>) {
        if let Ok(name) = String::from_utf8(node.name().as_slice().to_vec()) {
            let pos = self.pos(node.location().start_offset());
            self.facts.const_refs.push(ConstRef {
                name,
                nesting: self.nesting.clone(),
                pos,
            });
        }
    }

    fn visit_constant_path_node(&mut self, node: &ruby_prism::ConstantPathNode<'pr>) {
        let mut prefixes = Vec::new();
        path_prefixes(node, &mut prefixes);
        for (name, offset) in prefixes {
            let pos = self.pos(offset);
            self.facts.const_refs.push(ConstRef {
                name,
                nesting: self.nesting.clone(),
                pos,
            });
        }
    }

    fn visit_constant_write_node(&mut self, node: &ruby_prism::ConstantWriteNode<'pr>) {
        let Ok(name) = String::from_utf8(node.name().as_slice().to_vec()) else {
            return;
        };
        let value = node.value();
        let loc = node.name_loc();
        if let Some(call) = value.as_call_node()
            && let Some(made) = Made::by(&call)
        {
            self.handle_made(name, node, &call, made);
            return;
        }
        if let Some(symbols) = literal_symbol_array(&value) {
            self.symbol_arrays.insert(name.clone(), symbols);
        }
        if let Some(constants) = literal_constant_array(&value) {
            self.constant_arrays.insert(name.clone(), constants);
        }
        if let Some(names) = every_element_literal(&value) {
            self.name_arrays
                .insert((self.nesting.clone(), name.clone()), names);
        }
        let mut def = self.def(name, Kind::Constant, loc.start_offset(), loc.end_offset());
        // `Bar = Foo` is an alias: the tree layer follows it rather than
        // treating `Bar` as a fresh namespace.
        def.target = const_name(&value);
        self.push_def(def);
        self.visit(&value);
    }

    fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
        // Side effects, not consumptions: these are still ordinary calls, they
        // just also say something about the model.
        self.handle_create_table(node);
        self.handle_table_name(node);
        self.note_definer(node);
        self.handle_configure_mixin(node);
        self.handle_sent_mixin(node);
        self.handle_run_load_hooks(node);
        self.handle_define_method(node);
        self.handle_string_eval(node);
        self.handle_group_member(node);
        self.handle_custom_matcher(node);
        self.handle_shared_include(node);
        self.note_matcher_subject(node);
        let consumed = self.handle_macro(node);
        // A macro is *also* an ordinary method call — `belongs_to` really is
        // `ActiveRecord::Associations::ClassMethods#belongs_to`. Consuming one
        // to generate the methods it implies used to swallow the call site
        // with it, so asking what a macro is answered "no name here": the
        // single largest miss on real Rails app code, where the class body is
        // most of the surface.
        self.record_call(node);
        self.expand_macro(node);
        if consumed {
            return;
        }

        if let Some(receiver) = node.receiver() {
            self.visit(&receiver);
        }
        if let Some(args) = node.arguments() {
            self.visit_arguments_node(&args);
        }
        if let Some(block) = node.block() {
            // `[:before, :after].each do |callback| … end` binds `callback` to
            // three known strings for the length of the block, which is what
            // lets a `define_method "#{callback}_action"` inside it be read.
            let loop_depth = self.loop_values.len();
            let bound = self.literal_each(node).inspect(|binding| {
                self.loop_values.push(binding.clone());
            });
            if bound.is_some() {
                self.loop_frames.push(self.frames.len());
            }
            let handed_depth = self.handed_loops.len();
            if let Some(iterated) = self.splat_each(node) {
                self.handed_loops.push(iterated);
            }
            let iterates = self.constant_each(node).inspect(|binding| {
                self.constant_loops.push(binding.clone());
            });
            let included = method_name(node).as_deref() == Some("included")
                && on_self(node)
                && !self.in_method_body();
            self.included_depth += usize::from(included);
            // At `do`, where no name is: a declaration at `included` would
            // answer a click on the call with the module.
            if included && self.included_at.is_none() {
                self.included_at = block
                    .as_block_node()
                    .map(|b| b.opening_loc().start_offset());
            }
            let on_singleton = runs_on_singleton_class(node);
            self.singleton_exec += usize::from(on_singleton);
            let configure = rspec_configure_param(node).inspect(|param| {
                self.configure_params.push(param.clone());
            });
            // A `define_method` block is the method's body: `self` there is
            // an instance, and a `super` looks up the name it defines.
            let owner = node.message_loc().map(|m| self.pos(m.start_offset()));
            match self.defined_method(node) {
                Some(DefinedBy::Scope(method)) => {
                    let singleton = method_name(node).as_deref() == Some("define_singleton_method")
                        || self.in_singleton();
                    self.enter(None, Opens::Method { singleton });
                    self.frame().method = method;
                    self.visit(&block);
                    self.leave();
                }
                // Some other object's method, or one made when a method runs:
                // a `super` in it is not the enclosing method's, and its name
                // is not known here.
                Some(DefinedBy::Elsewhere) => {
                    let singleton = self.self_is_class();
                    self.enter(None, Opens::Method { singleton });
                    self.visit(&block);
                    self.leave();
                }
                None => match self.spec_block(node) {
                    Some(SpecBlock::Group(description)) => {
                        let segment = self.group_segment(&description);
                        let described = arg_nodes(node)
                            .first()
                            .and_then(const_name)
                            .or_else(|| self.described.last().cloned().flatten());
                        self.described.push(described.clone());
                        self.enter(Some(segment), Opens::Scope);
                        self.frame().group = true;
                        if let Some(class) = described {
                            self.facts.described.push((self.nesting.clone(), class));
                        }
                        // `it_behaves_like "x" do … end` is a group that
                        // includes x, and its block customizes that group.
                        if let Some(module) = self.shared_included(node) {
                            self.facts
                                .shared_includes
                                .push((self.nesting.clone(), module));
                        }
                        if let Some(body) = block.as_block_node().and_then(|b| b.body()) {
                            self.visit(&body);
                        }
                        self.leave();
                        self.described.pop();
                    }
                    Some(SpecBlock::Shared(name)) => {
                        // Declared where it is written, so the module has a
                        // site: at the block's opening, since no constant is
                        // written and a click on the call is the call.
                        let at = node.location();
                        let opening = block
                            .as_block_node()
                            .map_or(at.start_offset(), |b| b.opening_loc().start_offset());
                        let mut module = self.def(
                            format!("::{}", crate::core::rspec::shared_module(&name)),
                            Kind::Module,
                            opening,
                            at.end_offset(),
                        );
                        module.nesting.clear();
                        self.facts.defs.push(module);
                        // Its includer's `described_class`, not the file's.
                        self.described.push(None);
                        self.enter(
                            Some(crate::core::rspec::shared_segment(&name)),
                            Opens::Scope,
                        );
                        self.frame().group = true;
                        if let Some(body) = block.as_block_node().and_then(|b| b.body()) {
                            self.visit(&body);
                        }
                        self.leave();
                        self.described.pop();
                    }
                    Some(SpecBlock::Example) => {
                        self.enter(None, Opens::Method { singleton: false });
                        self.frame().example = method_name(node)
                            .is_some_and(|name| !INHERITED_BLOCKS.contains(&name.as_str()));
                        self.visit(&block);
                        self.leave();
                    }
                    None if let Some(how) = self.evaluated_in_mixer(node) => {
                        let conditional = self.conditional;
                        self.enter(None, Opens::Scope);
                        self.frame().mixed = Some(Mixed {
                            how,
                            base: None,
                            conditional,
                        });
                        // Where a `ClassMethods` it routes to is declared.
                        let outermost = self.included_at.is_none();
                        if outermost {
                            self.included_at = block
                                .as_block_node()
                                .map(|b| b.opening_loc().start_offset());
                        }
                        if let Some(body) = block.as_block_node().and_then(|b| b.body()) {
                            self.visit(&body);
                        }
                        if outermost {
                            self.included_at = None;
                        }
                        self.leave();
                    }
                    None => match self.evaluated_in(node) {
                        Some(scope) => {
                            self.enter(Some(scope), Opens::Scope);
                            if let Some(body) = block.as_block_node().and_then(|b| b.body()) {
                                self.visit(&body);
                            }
                            self.leave();
                        }
                        None => {
                            // A hook registered later — in a Railtie's
                            // `initializer`, under an `if` — is still a hook,
                            // and its mixins are not the lexical scope's.
                            let modelled = self.runs_as_file_loads();
                            let hook = load_hook(node);
                            let foreign = evaluated_elsewhere(node) && !on_singleton;
                            self.foreign_evals += usize::from(foreign);
                            self.open_blocks.push(owner);
                            if iterates.is_some() {
                                self.iterations.push(self.open_blocks.len());
                            }
                            let hook = hook.inspect(|(name, yields)| {
                                self.load_hooks.push(LoadHook {
                                    name: name.clone(),
                                    depth: self.open_blocks.len(),
                                    modelled,
                                    yields: *yields,
                                    base: first_block_param(node),
                                    at: block
                                        .as_block_node()
                                        .map_or(0, |b| b.opening_loc().start_offset()),
                                });
                            });
                            self.visit(&block);
                            if hook.is_some() {
                                self.load_hooks.pop();
                            }
                            if iterates.is_some() {
                                self.iterations.pop();
                            }
                            self.foreign_evals -= usize::from(foreign);
                            self.open_blocks.pop();
                        }
                    },
                },
            }
            if configure.is_some() {
                self.configure_params.pop();
            }
            self.singleton_exec -= usize::from(on_singleton);
            self.included_depth -= usize::from(included);
            if self.included_depth == 0 {
                self.included_at = None;
            }
            // With the locals the loop's body built from its values.
            self.loop_values.truncate(loop_depth);
            self.handed_loops.truncate(handed_depth);
            if bound.is_some() {
                self.loop_frames.pop();
            }
            if iterates.is_some() {
                self.constant_loops.pop();
            }
        }
    }

    fn visit_local_variable_write_node(&mut self, node: &ruby_prism::LocalVariableWriteNode<'pr>) {
        if let Ok(name) = String::from_utf8(node.name().as_slice().to_vec()) {
            // `meth = "sanitized_#{m}"` in a literal loop takes a value per
            // name, as `m` does, until the loop ends (DEC-160).
            if let Some(values) = self.interpolated_names(&node.value()) {
                self.loop_values.push((name.clone(), values));
            }
            self.record_assign(name, &node.value(), node.location().start_offset());
        }
        self.visit(&node.value());
    }

    /// `x ||= Foo.new` may be the write a later read sees, so it is one.
    fn visit_local_variable_or_write_node(
        &mut self,
        node: &ruby_prism::LocalVariableOrWriteNode<'pr>,
    ) {
        if let Ok(name) = String::from_utf8(node.name().as_slice().to_vec()) {
            self.record_assign(name, &node.value(), node.location().start_offset());
        }
        self.visit(&node.value());
    }

    fn visit_instance_variable_write_node(
        &mut self,
        node: &ruby_prism::InstanceVariableWriteNode<'pr>,
    ) {
        if let Ok(name) = String::from_utf8(node.name().as_slice().to_vec()) {
            self.record_assign(name, &node.value(), node.location().start_offset());
        }
        self.visit(&node.value());
    }

    /// `rescue WidgetError => e` writes `e`, once per class rescued, since
    /// any of them may be what arrived.
    fn visit_rescue_node(&mut self, node: &ruby_prism::RescueNode<'pr>) {
        if let Some(reference) = node.reference() {
            let target = reference
                .as_local_variable_target_node()
                .map(|t| t.name())
                .or_else(|| {
                    reference
                        .as_instance_variable_target_node()
                        .map(|t| t.name())
                })
                .and_then(|name| String::from_utf8(name.as_slice().to_vec()).ok());
            if let Some(target) = target {
                let classes: Vec<Node<'pr>> = node.exceptions().iter().collect();
                let values: Vec<ValueShape> = if classes.is_empty() {
                    vec![ValueShape::Rescued("StandardError".to_string())]
                } else {
                    classes
                        .iter()
                        .map(|class| {
                            const_name(class).map_or(ValueShape::Other, ValueShape::Rescued)
                        })
                        .collect()
                };
                let pos = self.pos(reference.location().start_offset());
                for value in values {
                    self.facts.assigns.push(Assign {
                        target: target.clone(),
                        value,
                        nesting: self.nesting.clone(),
                        pos,
                    });
                }
            }
        }
        ruby_prism::visit_rescue_node(self, node);
    }

    fn visit_if_node(&mut self, node: &ruby_prism::IfNode<'pr>) {
        self.conditional += 1;
        ruby_prism::visit_if_node(self, node);
        self.conditional -= 1;
    }

    fn visit_unless_node(&mut self, node: &ruby_prism::UnlessNode<'pr>) {
        self.conditional += 1;
        ruby_prism::visit_unless_node(self, node);
        self.conditional -= 1;
    }

    fn visit_case_node(&mut self, node: &ruby_prism::CaseNode<'pr>) {
        self.conditional += 1;
        ruby_prism::visit_case_node(self, node);
        self.conditional -= 1;
    }

    fn visit_and_node(&mut self, node: &ruby_prism::AndNode<'pr>) {
        self.conditional += 1;
        ruby_prism::visit_and_node(self, node);
        self.conditional -= 1;
    }

    fn visit_or_node(&mut self, node: &ruby_prism::OrNode<'pr>) {
        self.conditional += 1;
        ruby_prism::visit_or_node(self, node);
        self.conditional -= 1;
    }

    fn visit_block_node(&mut self, node: &ruby_prism::BlockNode<'pr>) {
        self.frame().blocks += 1;
        ruby_prism::visit_block_node(self, node);
        self.frame().blocks -= 1;
    }

    fn visit_lambda_node(&mut self, node: &ruby_prism::LambdaNode<'pr>) {
        self.frame().blocks += 1;
        ruby_prism::visit_lambda_node(self, node);
        self.frame().blocks -= 1;
    }

    fn visit_super_node(&mut self, node: &ruby_prism::SuperNode<'pr>) {
        let args: Vec<Node<'pr>> = node
            .arguments()
            .map(|a| a.arguments().iter().collect())
            .unwrap_or_default();
        self.record_super(
            node.keyword_loc().start_offset(),
            argc_of(&args),
            node.block().is_some(),
        );
        ruby_prism::visit_super_node(self, node);
    }

    /// Bare `super` passes the method's own arguments on, so its count is
    /// whatever the caller gave — unknowable here.
    fn visit_forwarding_super_node(&mut self, node: &ruby_prism::ForwardingSuperNode<'pr>) {
        self.record_super(node.location().start_offset(), None, node.block().is_some());
        ruby_prism::visit_forwarding_super_node(self, node);
    }

    fn visit_alias_method_node(&mut self, node: &ruby_prism::AliasMethodNode<'pr>) {
        let new = node.new_name();
        let old = node.old_name();
        // `alias a b` writes bare names; `alias :a :b` writes symbols.
        let name = literal_name(&new)
            .or_else(|| new.as_call_node().and_then(|c| method_name(&c)))
            .or_else(|| {
                Some(self.text(new.location().start_offset(), new.location().end_offset()))
            });
        let target = literal_name(&old)
            .or_else(|| old.as_call_node().and_then(|c| method_name(&c)))
            .or_else(|| {
                Some(self.text(old.location().start_offset(), old.location().end_offset()))
            });
        let (Some(name), Some(target)) = (name, target) else {
            return;
        };
        let loc = node.location();
        let mut def = self.def(name, Kind::Method, loc.start_offset(), loc.end_offset());
        def.singleton = self.in_singleton();
        def.via = Some("alias".into());
        self.bind_alias(&mut def, &target);
        def.target = Some(target);
        self.push_def(def);
    }
}

/// What a block handed to RSpec's DSL runs as (DEC-084).
enum SpecBlock {
    /// The body of a new example group, described by this text.
    Group(String),
    /// The body of a top-level shared group, named as `base_name` writes its
    /// name (DEC-092).
    Shared(String),
    /// An example, hook, `let` or `subject`: a method body on an instance of
    /// the group.
    Example,
}

/// Calls whose block is a new example group's body. The `shared_*` forms
/// are module-exec'd into whichever group includes them, and a group is the
/// nearest thing to that the file shows.
const GROUP_METHODS: [&str; 13] = [
    "describe",
    "context",
    "feature",
    "example_group",
    "xdescribe",
    "xcontext",
    "xfeature",
    "fdescribe",
    "fcontext",
    "ffeature",
    "shared_examples",
    "shared_context",
    "shared_examples_for",
];

/// Example-method blocks that a nested group runs as well as their own:
/// hooks, `let`s and `subject`s (DEC-096).
const INHERITED_BLOCKS: [&str; 13] = [
    "before",
    "after",
    "around",
    "prepend_before",
    "append_before",
    "prepend_after",
    "append_after",
    "let",
    "let!",
    "subject",
    "subject!",
    "its",
    "skip",
];

/// Calls whose first symbol names a method of their receiver (DEC-093).
const REFLECTIVE: [&str; 6] = [
    "send",
    "public_send",
    "__send__",
    "method",
    "public_method",
    "respond_to?",
];

/// RSpec's custom matcher DSL: each defines a matcher named by its first
/// symbol (DEC-091). `matcher` is `define`'s alias.
const MATCHER_DEFINERS: [&str; 4] = [
    "define",
    "matcher",
    "define_negated_matcher",
    "alias_matcher",
];

/// Calls whose block is a shared group's body, run in whatever includes it.
const SHARED_METHODS: [&str; 3] = ["shared_examples", "shared_context", "shared_examples_for"];

/// Calls, inside a group, that include a shared group: into the group itself,
/// or (`it_behaves_like`) into a nested group of their own.
const SHARED_INCLUDERS: [&str; 4] = [
    "include_context",
    "include_examples",
    "it_behaves_like",
    "it_should_behave_like",
];

/// Calls, inside a group, whose block is a nested group of its own.
const NESTED_GROUP_METHODS: [&str; 2] = ["it_behaves_like", "it_should_behave_like"];

/// Calls, inside a group, whose block runs on an example.
const EXAMPLE_METHODS: [&str; 27] = [
    "it",
    "specify",
    "example",
    "scenario",
    "focus",
    "fit",
    "fspecify",
    "fexample",
    "fscenario",
    "xit",
    "xspecify",
    "xexample",
    "xscenario",
    "skip",
    "pending",
    "before",
    "after",
    "around",
    "prepend_before",
    "append_before",
    "prepend_after",
    "append_after",
    "let",
    "let!",
    "subject",
    "subject!",
    "its",
];

/// Whose method a `define_method` block is the body of.
enum DefinedBy {
    /// This scope's, with its name when exactly one is knowable.
    Scope(Option<String>),
    /// Another object's, or one made when a method runs.
    Elsewhere,
}

/// A class or module built by a call rather than written with a keyword.
#[derive(Clone, Copy, PartialEq)]
enum Made {
    /// `Struct.new(:a, :b)` — readers and writers for each member.
    Struct,
    /// `Data.define(:a, :b)` — readers only.
    Data,
    /// `Class.new(Base)`.
    Class,
    /// `Module.new`.
    Module,
}

impl Made {
    fn by(call: &ruby_prism::CallNode<'_>) -> Option<Made> {
        let receiver = const_name(&call.receiver()?)?;
        Some(
            match (
                receiver.trim_start_matches("::"),
                method_name(call)?.as_str(),
            ) {
                ("Struct", "new") => Made::Struct,
                ("Data", "define") => Made::Data,
                ("Class", "new") => Made::Class,
                ("Module", "new") => Made::Module,
                _ => return None,
            },
        )
    }
}

/// `-> { }`, `lambda { }` or `proc { }`: a callable written in place.
fn is_lambda(node: &Node<'_>) -> bool {
    node.as_lambda_node().is_some()
        || node.as_call_node().is_some_and(|call| {
            call.receiver().is_none()
                && call.block().is_some()
                && matches!(method_name(&call).as_deref(), Some("lambda" | "proc"))
        })
}

fn method_name(call: &ruby_prism::CallNode<'_>) -> Option<String> {
    String::from_utf8(call.name().as_slice().to_vec()).ok()
}

/// Is this call sent to `ActiveSupport`?
fn on_active_support(call: &ruby_prism::CallNode<'_>) -> bool {
    call.receiver()
        .and_then(|r| const_name(&r))
        .is_some_and(|r| r.trim_start_matches("::") == "ActiveSupport")
}

/// The hook an `ActiveSupport.on_load(:name) { … }` block is registered for,
/// and whether it yields: with `yield: true` the class is the block's
/// argument instead, and `self` is the caller's.
fn load_hook(call: &ruby_prism::CallNode<'_>) -> Option<(String, bool)> {
    if method_name(call).as_deref() != Some("on_load") || !on_active_support(call) {
        return None;
    }
    let args = arg_nodes(call);
    let first = args.first()?;
    let name = first.as_symbol_node().and(literal_name(first))?;
    Some((name, keyword_value(&args, "yield").is_some()))
}

/// The name of a block's first required parameter.
fn first_block_param(call: &ruby_prism::CallNode<'_>) -> Option<String> {
    let block = call.block()?.as_block_node()?;
    let params = block
        .parameters()?
        .as_block_parameters_node()?
        .parameters()?;
    let first = params.requireds().iter().next()?;
    let param = first.as_required_parameter_node()?;
    String::from_utf8(param.name().as_slice().to_vec()).ok()
}

/// The mixin a hook method runs for: `included` for an `include`.
fn mixin_hook(name: &str) -> Option<Relation> {
    match name {
        "included" => Some(Relation::Include),
        "prepended" => Some(Relation::Prepend),
        "extended" => Some(Relation::Extend),
        _ => None,
    }
}

/// The relation a mixin method's name makes. `superclass` is no method.
fn mixin_relation(name: &str) -> Option<Relation> {
    match name {
        "include" | "prepend" | "extend" => Relation::parse(name),
        _ => None,
    }
}

impl<'pr> Extractor<'_> {
    /// Calls that define things rather than do things. Returns `true` when the
    /// call was fully consumed and must not also be recorded as a call site.
    fn handle_macro(&mut self, call: &ruby_prism::CallNode<'pr>) -> bool {
        let Some(name) = method_name(call) else {
            return false;
        };
        // Every macro here is a private method on Module: an explicit receiver
        // other than `self` means it is somebody else's method of the same name.
        if !on_self(call) {
            return false;
        }
        let args = arg_nodes(call);
        match name.as_str() {
            "attr_reader" | "attr_writer" | "attr_accessor" | "attr" => {
                self.handle_attr(call, &name, &args)
            }
            "include" | "prepend" | "extend" => self.handle_mixin(&name, &args),
            "concerning" => self.handle_concerning(call, &args),
            "class_methods" => self.handle_class_methods(call, &args),
            "alias_method" => self.handle_alias_method(call, &args),
            "def_delegator" | "def_instance_delegator" | "def_single_delegator" => {
                self.handle_forwardable(&name, &args, false)
            }
            "def_delegators" | "def_instance_delegators" | "def_single_delegators" => {
                self.handle_forwardable(&name, &args, true)
            }
            "enum" => self.handle_enum(call, &args),
            "store" | "store_accessor" => self.handle_store(call, &name, &args),
            "delegate_missing_to" => self.handle_delegate_missing(&args),
            // Any macro the expansion table knows. The probe argument only
            // asks "is this a macro we model" — the real names come below.
            _ if !macros::generated(&name, "probe").is_empty() => {
                self.handle_dsl(call, &name, &args)
            }
            "private" | "protected" | "public" | "module_function" => {
                self.handle_visibility(call, &name, &args)
            }
            _ if !self.in_method_body()
                && self
                    .definers
                    .contains_key(&(self.nesting.clone(), name.clone())) =>
            {
                self.handle_definer_call(call, &name, &args);
                false
            }
            _ => false,
        }
    }

    fn handle_attr(
        &mut self,
        call: &ruby_prism::CallNode<'pr>,
        macro_name: &str,
        args: &[Node<'pr>],
    ) -> bool {
        if args.is_empty() {
            return false;
        }
        // `attr :a, true` is the one form that also writes; `attr :a, :b` is
        // three readers. Every other `attr_*` reads its arity plainly.
        let writer = match macro_name {
            "attr_writer" => true,
            "attr_accessor" => true,
            "attr" => args.len() == 2 && args[1].as_true_node().is_some(),
            _ => false,
        };
        let reader = macro_name != "attr_writer";
        let sig = sig::resolve(&std::mem::take(&mut self.pending_sigs), &[]).0;
        let visibility = self.visibility();
        let singleton = self.in_singleton();
        let loc = call.location();
        let (start, end) = (loc.start_offset(), loc.end_offset());

        for arg in args {
            let Some(attr) = literal_name(arg) else {
                continue;
            };
            if reader {
                let mut def = self.def(attr.clone(), Kind::Method, start, end);
                def.pos = self.pos(arg.location().start_offset());
                def.via = Some(macro_name.to_string());
                def.visibility = visibility;
                def.singleton = singleton;
                def.sig_returns = sig.clone();
                self.push_def(def);
            }
            if writer {
                let mut def = self.def(format!("{attr}="), Kind::Method, start, end);
                def.pos = self.pos(arg.location().start_offset());
                def.via = Some(macro_name.to_string());
                def.visibility = visibility;
                def.singleton = singleton;
                def.params = vec![Param {
                    kind: ParamKind::Req,
                    name: attr,
                }];
                self.push_def(def);
            }
        }
        true
    }

    fn handle_mixin(&mut self, macro_name: &str, args: &[Node<'pr>]) -> bool {
        let Some(relation) = mixin_relation(macro_name) else {
            return false;
        };
        let Some(mut owner) = self.mixin_owner() else {
            return false;
        };
        // `class << self; include M; end` mixes into the singleton class.
        let Some(mut relation) = (if self.in_singleton() {
            relation.on_singleton()
        } else {
            Some(relation)
        }) else {
            return false;
        };
        // `extend self` — the idiomatic module-function alternative.
        let mut own = Some("self");
        // A hook's `base.class_eval` body mixes into the mixer (DEC-102), and
        // so does a concern's `included do`, but for its `extend`, which
        // Concern's `ClassMethods` carries to the includer (DEC-103).
        if let Some(how) = self.includer_how() {
            owner = self.mixed_owner(how);
            own = None;
        } else if relation != Relation::Extend && self.in_concerns_included_block() {
            owner = self.mixed_owner(Relation::Include);
            own = None;
        }
        // `extend M` inside `included do` extends the includer, which is
        // what Concern does with `ClassMethods`: M is one of its ancestors.
        if relation == Relation::Extend && self.in_concerns_included_block() {
            owner.insert(0, "ClassMethods".to_string());
            relation = Relation::Include;
            self.declare_class_methods();
        }
        let any = self.push_mixins(owner, relation, args, own);
        if any {
            // The argument constants are real references too.
            for arg in args {
                self.visit(arg);
            }
        }
        any
    }

    /// The scope an `include` on `self` written here mixes into.
    fn mixin_owner(&self) -> Option<Vec<String>> {
        // An `on_load` block runs on whatever runs the hook (DEC-098), a
        // `def` around it or not, but only if it is registered at all.
        if let Some(hook) = self.in_load_hook().filter(|hook| !hook.yields) {
            let mut owner = vec![crate::core::runtime::hook(&hook.name)];
            owner.extend(self.nesting.iter().cloned());
            return (hook.modelled && self.runs_as_file_loads()).then_some(owner);
        }
        // A mixin written inside a `def` is not this scope's ancestor. It runs
        // when the method runs, against whatever `self` is then — which is why
        // `has_secure_password` can write `include ActiveModel::Validations`
        // inside a `ClassMethods` body and mean the *model*, not the module.
        // Recording it lexically does not merely miss an edge, it invents one:
        // that single line put ActiveModel::Validations' instance methods into
        // the class-level chain of every ActiveRecord model, where
        // `alias_method :validate, :valid?` then beat the real
        // `ClassMethods#validate`. It stays an ordinary call site.
        if self.in_method_body() {
            return None;
        }
        // A group is a class only its own file sees (DEC-084), and an edge on
        // it would land on the constant scope around it.
        if self.in_group_body() {
            return None;
        }
        Some(self.nesting.clone())
    }

    /// One edge per constant argument, onto `owner`. A `self` argument is
    /// the target `own`, when there is one.
    fn push_mixins(
        &mut self,
        owner: Vec<String>,
        relation: Relation,
        args: &[Node<'pr>],
        own: Option<&str>,
    ) -> bool {
        let mut any = false;
        // `include A, B` inserts B first — Ruby applies multi-arg mixins
        // right to left, and the ancestor order the tree layer builds is
        // exactly this list's order.
        for arg in args.iter().rev() {
            let target = if arg.as_self_node().is_some() {
                own.map(str::to_string)
            } else {
                const_name(arg)
            };
            let Some(target) = target else {
                continue;
            };
            let pos = self.pos(arg.location().start_offset());
            self.facts.ancestry.push(Ancestry {
                owner: owner.clone(),
                relation,
                target,
                pos,
            });
            any = true;
        }
        any
    }

    /// `ActiveSupport.run_load_hooks(:active_record, Base)`: `Base` runs the
    /// hook's `on_load` blocks (DEC-098). The class is a constant, looked up
    /// as a sent mixin's receiver is, or `self` in its own body.
    fn handle_run_load_hooks(&mut self, call: &ruby_prism::CallNode<'pr>) {
        if method_name(call).as_deref() != Some("run_load_hooks") || !on_active_support(call) {
            return;
        }
        let args = arg_nodes(call);
        let [name, base, ..] = &args[..] else {
            return;
        };
        let Some(hook) = name.as_symbol_node().and(literal_name(name)) else {
            return;
        };
        let owner = if base.as_self_node().is_some() {
            self.self_is_the_scope().then(|| self.nesting.clone())
        } else {
            const_name(base).map(|written| self.sent_owner(&written))
        };
        let Some(owner) = owner else {
            return;
        };
        let pos = self.pos(name.location().start_offset());
        self.facts.ancestry.push(Ancestry {
            owner,
            relation: Relation::LoadHooks,
            target: hook,
            pos,
        });
    }

    /// The owner of an edge sent to the constant `written` from here: looked
    /// up where the call is written (DEC-097).
    fn sent_owner(&self, written: &str) -> Vec<String> {
        let mut owner = vec![crate::core::runtime::sent(written)];
        owner.extend(self.nesting.iter().cloned());
        owner
    }

    /// A receiver that is the hook's `base`: how the module it holds is mixed
    /// in (DEC-102).
    fn mixer_named(&self, receiver: &Node<'pr>) -> Option<Relation> {
        let read = receiver.as_local_variable_read_node()?;
        let mixed = self.mixed()?;
        (mixed.base.as_deref()?.as_bytes() == read.name().as_slice()).then_some(mixed.how)
    }

    /// `base.class_eval do … end` in a hook: a body of the mixer (DEC-102).
    fn evaluated_in_mixer(&self, call: &ruby_prism::CallNode<'pr>) -> Option<Relation> {
        let name = method_name(call)?;
        let evaluates = matches!(
            name.as_str(),
            "class_eval" | "class_exec" | "module_eval" | "module_exec"
        );
        if !evaluates || !arg_nodes(call).is_empty() {
            return None;
        }
        self.mixer_named(&call.receiver()?)
    }

    /// The constants a receiver stands for: the one it names, or each of a
    /// literal list's, for the parameter of a block iterating one (DEC-100).
    fn constants_named(&self, receiver: &Node<'pr>) -> Vec<String> {
        if let Some(read) = receiver.as_local_variable_read_node() {
            let local = read.name().as_slice();
            return self
                .constant_loops
                .iter()
                .rev()
                .find(|(param, _)| param.as_bytes() == local)
                .map(|(_, constants)| constants.clone())
                .unwrap_or_default();
        }
        const_name(receiver).into_iter().collect()
    }

    /// `[Hash, Array].each do |klass|`, or `KINDS.reverse_each` for a
    /// constant this file assigns such a list: the block's parameter and the
    /// constants it takes in turn.
    fn constant_each(&self, call: &ruby_prism::CallNode<'pr>) -> Option<(String, Vec<String>)> {
        if !matches!(method_name(call)?.as_str(), "each" | "reverse_each") {
            return None;
        }
        let receiver = call.receiver()?;
        let constants = match literal_constant_array(&receiver) {
            Some(constants) => constants,
            None => self.constant_arrays.get(&const_name(&receiver)?)?.clone(),
        };
        Some((sole_block_param(call)?, constants))
    }

    /// A mixin sent rather than written in a body (DEC-097):
    /// `Widget.include(Helpers)`, `Widget.send(:prepend, Patch)`, and
    /// `send(:include, Helpers)` on `self`. A side effect: the call is still
    /// a call, and its arguments are visited with it.
    fn handle_sent_mixin(&mut self, call: &ruby_prism::CallNode<'pr>) {
        let Some(name) = method_name(call) else {
            return;
        };
        let args = arg_nodes(call);
        let (relation, args) = match name.as_str() {
            "send" | "__send__" => match args.split_first() {
                Some((first, rest)) => (
                    literal_name(first).and_then(|sent| mixin_relation(&sent)),
                    rest,
                ),
                None => return,
            },
            // On `self`, these are the macros `handle_mixin` reads.
            _ if on_self(call) => return,
            _ => (mixin_relation(&name), &args[..]),
        };
        let Some(mut relation) = relation else {
            return;
        };
        // `Adapter.singleton_class.prepend(Retrying)` (DEC-101).
        let singleton = call.receiver().and_then(|r| r.as_call_node()).filter(|r| {
            method_name(r).as_deref() == Some("singleton_class")
                && r.arguments().is_none()
                && r.block().is_none()
        });
        let receiver = match &singleton {
            Some(of) => {
                let Some(on) = relation.on_singleton() else {
                    return;
                };
                relation = on;
                of.receiver()
            }
            None => call.receiver(),
        };
        // Only what runs as its file loads is recorded (DEC-097), or, sent
        // to a hook's `base`, as its module is mixed in (DEC-102).
        let owners: Vec<Vec<String>> = match receiver {
            Some(receiver) if receiver.as_self_node().is_none() => {
                if let Some(hook) = self.load_hook_base(&receiver) {
                    // `on_load(:x) { |base| base.include(M) }` (DEC-104).
                    if !hook.modelled || !self.runs_as_file_loads() {
                        return;
                    }
                    let mut owner = vec![crate::core::runtime::hook(&hook.name)];
                    owner.extend(self.nesting.iter().cloned());
                    self.push_mixins(owner, relation, args, None);
                    return;
                }
                match self.mixer_named(&receiver) {
                    Some(how) => vec![self.mixed_owner(how)],
                    None if self.runs_as_file_loads() => self
                        .constants_named(&receiver)
                        .iter()
                        .map(|written| self.sent_owner(written))
                        .collect(),
                    None => return,
                }
            }
            _ => match self.includer_how() {
                Some(how) => vec![self.mixed_owner(how)],
                None if self.runs_as_file_loads() => self.mixin_owner().into_iter().collect(),
                None => return,
            },
        };
        // `Object.prepend(self)` in a module's body sends the module.
        let own = self
            .self_is_the_scope()
            .then(|| self.nesting.first().cloned())
            .flatten();
        for owner in owners {
            self.push_mixins(owner, relation, args, own.as_deref());
        }
    }

    /// `define_method "#{callback}_action"` inside a literal `each` — a method
    /// whose name is computed, but computed from something the source states.
    ///
    /// Actionpack writes `before_action`, `after_action`, `around_action` and
    /// their `prepend_`/`skip_` variants this way, and ActiveRecord's
    /// `define_model_callbacks` is the same shape. Nothing that reads only the
    /// `def` keyword can see them, so `before_action` in a controller was the
    /// largest block of the one bucket where trekr offers *nothing*: 250 of
    /// discourse's 3,566 declined app sites, none of them with a candidate.
    ///
    /// A **side effect, not a consumption**: `define_method` is a real call
    /// site too, and its block is a method body full of ordinary code.
    fn handle_define_method(&mut self, call: &ruby_prism::CallNode<'pr>) {
        let Some(Definer { name, args, on }) = definer(call) else {
            return;
        };
        let singleton = match (name.as_str(), &on) {
            ("define_singleton_method", _) | (_, DefinedOn::OwnSingleton) => true,
            (_, DefinedOn::Constant(_)) => false,
            _ => self.in_singleton() || self.singleton_exec > 0,
        };
        let at = call.location().start_offset();
        // A block run on some other object — `mod.singleton_class.
        // instance_eval do` — defines on it, and this scope is not it.
        if self.foreign_evals > 0 && matches!(on, DefinedOn::Own | DefinedOn::OwnSingleton) {
            return;
        }
        let Some(first) = args.first() else { return };
        let computed = self.computed_names(first);
        // Another class's methods are marked on it, never defined here,
        // since its own file is where they belong.
        if let DefinedOn::Constant(written) = &on {
            let owner = self.sent_owner(written);
            for shape in self.shapes(computed, first) {
                let maker = Maker {
                    by: name.clone(),
                    singleton: Some(singleton),
                    shape,
                    via: None,
                };
                self.mark_dynamic_on(owner.clone(), maker, at);
            }
            return;
        }
        // Same rule as a mixin (DEC-031): inside a `def` this runs later,
        // against whatever `self` is then. In a class method `self` is the
        // class, so a name the source spells is the class's method once the
        // method runs (DEC-160), unless a block in between may run elsewhere,
        // which marks the name instead. Any other name is marked, by its shape.
        // In an instance method `self` is whatever it runs on: a macro's
        // class, when a class body calls it (DEC-162).
        if self.in_method_body() && !self.self_is_class() {
            let Some(via) = self.frames.last().and_then(|f| f.method.clone()) else {
                return;
            };
            let handed = self.handed();
            let shapes = match computed {
                Some(names) => names.into_iter().map(Some).collect(),
                None => vec![handed_shape(first, &handed)],
            };
            for shape in shapes {
                self.mark_dynamic_shaped(&name, singleton, shape, first, at, Some(&via));
            }
            return;
        }
        let definable = !self.in_method_body() || self.blocks_are_loops();
        let names = match computed {
            Some(names) if definable => names,
            computed => {
                for shape in self.shapes(computed, first) {
                    self.mark_dynamic_shaped(&name, singleton, shape, first, at, None);
                }
                return;
            }
        };
        // `define_method(:x, instance_method(:y))`: the body is that method's,
        // not this line, so the definition is a declaration of it.
        let body_elsewhere = match (call.block(), args.get(1)) {
            (None, Some(body)) => {
                let at = body.location();
                Some(self.text(at.start_offset(), at.end_offset()))
            }
            _ => None,
        };
        // The block *is* the method body, so its parameters are the method's.
        let params = call
            .block()
            .and_then(|b| b.as_block_node())
            .and_then(|b| b.parameters())
            .and_then(|p| p.as_block_parameters_node())
            .map(|p| params_of(p.parameters()))
            .unwrap_or_default();

        let start = call.location().start_offset();
        let end = call.location().end_offset();
        for generated in names {
            let mut def = self.def(generated, Kind::Method, start, end);
            def.singleton = singleton;
            def.params = params.clone();
            // The honest location is where the definition is written, which is
            // this call — the same answer a macro gives (DEC-022, session 15).
            def.via = Some(name.clone());
            def.target = body_elsewhere.clone();
            self.push_def(def);
        }
    }

    /// `class_eval <<-RUBY … RUBY` on `self`: the string is code, read as if
    /// written in the scope (DEC-132). An interpolation must be a local, or a
    /// local through one of `RENDERS`: the code around it is read once, and
    /// a `def` whose name it spells is read once per value of a literal
    /// loop's variable. Whatever else, or a name no loop states, marks the
    /// scope (DEC-130).
    fn handle_string_eval(&mut self, call: &ruby_prism::CallNode<'pr>) {
        let Some(name) = method_name(call) else {
            return;
        };
        // `instance_eval` on a class makes class methods, whatever its `def`s
        // say; `eval` in a body runs there, as `class_eval` does.
        let class_side = match name.as_str() {
            "class_eval" | "module_eval" => false,
            "eval" if call.receiver().is_none() => false,
            "instance_eval" => true,
            _ => return,
        };
        let Some(arg) = arg_nodes(call).into_iter().next() else {
            return;
        };
        let at = call.location().start_offset();
        // Not read at all: its calls are missing too, which `--dead` says.
        let unread = format!("{name} string");
        // In an instance method, `self` is whatever runs it: a macro's class,
        // when a class body calls it (DEC-162).
        let on_self_here = call.receiver().is_none_or(|r| r.as_self_node().is_some());
        if on_self_here && self.in_method_body() && !self.self_is_class() {
            let Some(via) = self.frames.last().and_then(|f| f.method.clone()) else {
                return;
            };
            if self.nesting.is_empty() || self.in_group_body() {
                return;
            }
            match spelled_code(arg) {
                Some(code) => {
                    if !class_side
                        && self.evals.is_empty()
                        && let Some(pieces) = code_pieces(&code)
                        && let Some(handed) = self.handed_locals(&pieces)
                    {
                        self.keep_macro(&name, pieces, handed, None);
                    }
                    let handed = self.handed();
                    let src = self.evals.last().map_or(self.src, |eval| &eval.src[..]);
                    for (singleton, shape) in string_defs(&code, src, &handed) {
                        let maker = Maker {
                            by: name.clone(),
                            singleton: if class_side { Some(true) } else { singleton },
                            shape,
                            via: Some(via.clone()),
                        };
                        self.mark_dynamic_on(self.nesting.clone(), maker, at);
                    }
                }
                None => {
                    let maker = Maker {
                        by: unread,
                        via: Some(via),
                        ..Maker::default()
                    };
                    self.mark_dynamic_on(self.nesting.clone(), maker, at);
                }
            }
            return;
        }
        // Whose methods it makes, and whether it is read: only a string
        // evaluated in the scope it is written in is (DEC-132). Another
        // class's is marked on it, as is this class's from an instance's
        // `self.class` (DEC-161).
        let own = self.nesting.clone();
        let (owner, readable) = match call.receiver() {
            None if self.self_is_class() => (own, !class_side),
            Some(r) if r.as_self_node().is_some() && self.self_is_class() => (own, !class_side),
            Some(r) if is_self_class(&r) && self.in_method_body() && !self.self_is_class() => {
                (own, false)
            }
            Some(r) => match const_name(&r) {
                Some(written) => (self.sent_owner(&written), false),
                // Some other object's: nothing to mark, but the calls its
                // text names by interpolation are not read either (DEC-163).
                None => {
                    if let Some(code) = spelled_code(arg) {
                        let src = self.evals.last().map_or(self.src, |eval| &eval.src[..]);
                        self.facts.unread_calls.extend(string_calls(&code, src));
                    }
                    return;
                }
            },
            _ => return,
        };
        if owner.is_empty() || self.in_group_body() {
            return;
        }
        // `<<~RUBY.strip` is the heredoc's code; `.gsub(…)`, a local,
        // `[…].join` or `format(…)` is code the source does not spell.
        let Some(first) = spelled_code(arg) else {
            let maker = Maker {
                by: unread,
                ..Maker::default()
            };
            self.mark_dynamic_on(owner, maker, at);
            return;
        };
        // A string inside a string is offsets into the outer one's text, not
        // the file's; not worth composing the maps for.
        if !self.evals.is_empty() || !readable {
            self.mark_string_on(owner, &unread, &first, class_side, at, None);
            return;
        }
        let Some(pieces) = code_pieces(&first) else {
            self.mark_string(&unread, &first, at);
            return;
        };
        let Some(mentions) = self.read_code(&name, &pieces, &[], None) else {
            self.mark_string(&unread, &first, at);
            return;
        };
        if mentions.defs.is_empty() && mentions.calls.is_empty() {
            return;
        }
        // A class method's string of its own parameters is a macro: read
        // again where this file's class bodies call it (DEC-163).
        if self.in_method_body()
            && let Some(handed) = self.handed_locals(&pieces)
        {
            self.keep_macro(&name, pieces.clone(), handed, Some(mentions.clone()));
        }
        let locals = locals_of(&pieces);
        let values = match locals.as_slice() {
            [local] => self
                .loop_values
                .iter()
                .rev()
                .find(|(bound, _)| bound.as_bytes() == local.as_slice())
                .map(|(_, values)| values.clone()),
            _ => None,
        };
        let Some(values) = values else {
            if !mentions.defs.is_empty() {
                self.mark_string(&name, &first, at);
            }
            self.facts.unread_calls.extend(mentions.call_shapes);
            return;
        };
        // A bound on what one string makes, so a loop of hundreds of names
        // over hundreds of `def`s is marked rather than written out (DEC-164).
        let making = values.len() * mentions.defs.len();
        if making > EXPANDED_PER_STRING || self.expanded + making > EXPANDED_PER_FILE {
            let by = format!("{name} of {making} methods, too many to read");
            self.mark_string(&by, &first, at);
            self.facts.unread_calls.extend(mentions.call_shapes);
            return;
        }
        self.expanded += making;
        let local = locals[0].clone();
        for value in values {
            self.read_code(&name, &pieces, &[(local.clone(), value)], Some(&mentions));
        }
    }

    /// Each local a string of code interpolates, as the positional parameter
    /// of the method it is written in that hands it: `None` unless every one
    /// is such a parameter (DEC-163).
    fn handed_locals(&self, pieces: &[Piece]) -> Option<Vec<(Vec<u8>, usize)>> {
        let frame = self.frames.last()?;
        frame.method.as_ref()?;
        locals_of(pieces)
            .into_iter()
            .map(|local| {
                let template = &frame.handed.iter().find(|(p, _)| p.as_bytes() == local)?.1;
                let k = template
                    .strip_prefix('{')?
                    .strip_suffix('}')?
                    .parse()
                    .ok()?;
                Some((local, k))
            })
            .collect()
    }

    /// Keep a method's string of code to read at its callers (DEC-163).
    fn keep_macro(
        &mut self,
        by: &str,
        pieces: Vec<Piece>,
        handed: Vec<(Vec<u8>, usize)>,
        named: Option<Mentions>,
    ) {
        let Some(method) = self.frames.last().and_then(|f| f.method.clone()) else {
            return;
        };
        self.string_macros
            .entry(method)
            .or_default()
            .push(StringMacro {
                by: by.to_string(),
                pieces,
                handed,
                named,
                scope: self.nesting.clone(),
            });
    }

    /// A class body's call of a method this file keeps a string of code for,
    /// handed literal names: the string read here, with them (DEC-163). A
    /// class method is its own class's; a macro is reached from a class that
    /// mixes in a module around it, or from `Module` and `Class`, which
    /// every class is.
    fn expand_macro(&mut self, call: &ruby_prism::CallNode<'pr>) {
        if !on_self(call) || !self.self_is_the_scope() || !self.evals.is_empty() {
            return;
        }
        let Some(name) = method_name(call) else {
            return;
        };
        let Some(macros) = self.string_macros.get(&name).cloned() else {
            return;
        };
        let args: Vec<Option<String>> = arg_nodes(call)
            .iter()
            .filter(|arg| arg.as_keyword_hash_node().is_none())
            .map(literal_name)
            .collect();
        for kept in macros {
            let reached = match &kept.named {
                Some(_) => kept.scope == self.nesting,
                None => self.mixes_in_around(&kept.scope),
            };
            if !reached {
                continue;
            }
            let bound: Option<Vec<(Vec<u8>, String)>> = kept
                .handed
                .iter()
                .map(|(local, k)| Some((local.clone(), args.get(*k).cloned().flatten()?)))
                .collect();
            let Some(bound) = bound else {
                continue;
            };
            let making = kept.named.as_ref().map_or(1, |named| named.defs.len());
            if self.expanded + making > EXPANDED_PER_FILE {
                return;
            }
            self.expanded += making;
            self.read_code(&kept.by, &kept.pieces, &bound, kept.named.as_ref());
        }
    }

    /// Does this scope mix in, in this file, a module around `scope` — or
    /// is `scope` `Module` or `Class`, whose methods every class has?
    fn mixes_in_around(&self, scope: &[String]) -> bool {
        if matches!(scope, [only] if matches!(only.trim_start_matches("::"), "Module" | "Class")) {
            return true;
        }
        self.facts.ancestry.iter().any(|edge| {
            matches!(edge.relation, Relation::Include | Relation::Extend)
                && edge.owner == self.nesting
                && edge
                    .target
                    .rsplit("::")
                    .next()
                    .is_some_and(|last| scope.iter().any(|s| s.trim_start_matches("::") == last))
        })
    }

    /// Mark a scope for a string of code it could not read whole: once per
    /// `def` the text spells, by that name's shape and side (DEC-160).
    fn mark_string(&mut self, by: &str, code: &Node<'pr>, at: usize) {
        if self.nesting.is_empty() || self.in_group_body() {
            return;
        }
        self.mark_string_on(self.nesting.clone(), by, code, false, at, None);
    }

    /// The same, on `owner`; `class_side` when every `def` there is a
    /// class method, as in `instance_eval`.
    fn mark_string_on(
        &mut self,
        owner: Vec<String>,
        by: &str,
        code: &Node<'pr>,
        class_side: bool,
        at: usize,
        via: Option<&str>,
    ) {
        let src = self.evals.last().map_or(self.src, |eval| &eval.src[..]);
        self.facts.unread_calls.extend(string_calls(code, src));
        for (singleton, shape) in string_defs(code, src, &[]) {
            let maker = Maker {
                by: by.to_string(),
                singleton: if class_side { Some(true) } else { singleton },
                shape,
                via: via.map(str::to_string),
            };
            self.mark_dynamic_on(owner.clone(), maker, at);
        }
    }

    /// Read a string of code with each local's value (the stand-in for any
    /// not `bound`), in a scope frame, keeping what it adds. With no `named`,
    /// that is whatever does not mention the stand-in, and the answer is where
    /// what does is; with `named`, only the `def`s and calls at those places:
    /// a value's own. `None` when the rendered code does not parse cleanly.
    fn read_code(
        &mut self,
        evaluator: &str,
        pieces: &[Piece],
        bound: &[(Vec<u8>, String)],
        named: Option<&Mentions>,
    ) -> Option<Mentions> {
        let eval = render(pieces, &Values { bound }, self.src);
        // Parsed from its own copy: the nodes borrow it while `eval`, with
        // the offsets they report, sits on the stack.
        let code = eval.src.clone();
        let parsed = ruby_prism::parse(&code);
        if parsed.errors().count() > 0 {
            return None;
        }
        let before = (
            self.facts.defs.len(),
            self.facts.calls.len(),
            self.facts.const_refs.len(),
            self.facts.assigns.len(),
            self.facts.ancestry.len(),
            self.facts.body_calls.len(),
        );
        self.evals.push(eval);
        self.enter(None, Opens::Scope);
        self.visit(&parsed.node());
        self.leave();
        self.evals.pop();

        let facts = &mut self.facts;
        let mut mentions = Mentions::default();
        facts.body_calls.truncate(before.5);
        let added = facts.defs.split_off(before.0);
        for mut def in added {
            // Its body is here, written in a string: the string's evaluator
            // made it, as a macro makes a declaration.
            if def.kind == Kind::Method && def.via.is_none() {
                def.via = Some(evaluator.to_string());
            }
            let spelled = mentions_unstated(&def.name);
            if spelled {
                mentions.defs.insert(def.pos);
            }
            let keep = match named {
                None => !spelled,
                Some(named) => named.defs.contains(&def.pos),
            };
            if keep {
                facts.defs.push(def);
            }
        }
        let tail = facts.calls.split_off(before.1);
        for call in tail {
            let spelled = mentions_unstated(&call.name)
                || call.recv_text.as_deref().is_some_and(mentions_unstated);
            if spelled {
                mentions.calls.insert(call.pos);
                let shape = unstated_shape(&call.name);
                if spells_enough(&shape) {
                    mentions.call_shapes.push(shape);
                }
            }
            let keep = match named {
                None => !spelled,
                // A call a value names, with its name spelled now (DEC-163).
                Some(named) => named.calls.contains(&call.pos) && !spelled,
            };
            if keep {
                facts.calls.push(call);
            }
        }
        if named.is_some() {
            facts.const_refs.truncate(before.2);
            facts.assigns.truncate(before.3);
            facts.ancestry.truncate(before.4);
            return Some(mentions);
        }
        let tail = facts.const_refs.split_off(before.2);
        facts
            .const_refs
            .extend(tail.into_iter().filter(|r| !mentions_unstated(&r.name)));
        let tail = facts.assigns.split_off(before.3);
        facts
            .assigns
            .extend(tail.into_iter().filter(|a| !mentions_unstated(&a.target)));
        let tail = facts.ancestry.split_off(before.4);
        facts
            .ancestry
            .extend(tail.into_iter().filter(|a| !mentions_unstated(&a.target)));
        Some(mentions)
    }

    /// Say that this scope defines methods whose names the source does not
    /// state, so that no answer claims it lacks one (DEC-130). Once per scope
    /// and maker.
    fn mark_dynamic_as(&mut self, maker: Maker, at: usize) -> Option<usize> {
        if self.nesting.is_empty() || self.in_group_body() {
            return None;
        }
        self.mark_dynamic_on(self.nesting.clone(), maker, at)
    }

    /// A marker on `owner`, once per owner and maker. The index of the
    /// edge, which a shape resolved later rewrites.
    fn mark_dynamic_on(&mut self, owner: Vec<String>, maker: Maker, at: usize) -> Option<usize> {
        let target = maker.encode();
        let known = self.facts.ancestry.iter().any(|edge| {
            edge.relation == Relation::Dynamic && edge.owner == owner && edge.target == target
        });
        if known {
            return None;
        }
        let pos = self.pos(at);
        self.facts.ancestry.push(Ancestry {
            owner,
            relation: Relation::Dynamic,
            target,
            pos,
        });
        Some(self.facts.ancestry.len() - 1)
    }

    /// A `define_method` whose name the source does not spell, marked with
    /// the part it does: `"_render_with_#{key}"`, or a method of this scope
    /// that returns such a string, which may be written below (DEC-160).
    fn mark_dynamic_shaped(
        &mut self,
        by: &str,
        singleton: bool,
        shape: Option<String>,
        name: &Node<'pr>,
        at: usize,
        via: Option<&str>,
    ) {
        let maker = Maker {
            by: by.to_string(),
            singleton: Some(singleton),
            shape,
            via: via.map(str::to_string),
        };
        let unshaped = maker.shape.is_none();
        let Some(edge) = self.mark_dynamic_as(maker, at) else {
            return;
        };
        if unshaped && let Some(method) = shaping_call(name) {
            self.pending_shapes
                .push((edge, self.nesting.clone(), method));
        }
    }

    /// The shape of the names a name argument spells: its literal text, with
    /// `*` for each interpolation. `None` when it spells no text at all.
    fn name_shape(&self, node: &Node<'pr>) -> Option<String> {
        let parts: Vec<Node<'pr>> = if let Some(string) = node.as_interpolated_string_node() {
            string.parts().iter().collect()
        } else {
            node.as_interpolated_symbol_node()?.parts().iter().collect()
        };
        shape_of(&parts)
    }

    /// The shapes to mark for a name argument: each name the source spells,
    /// or the one shape its text gives.
    fn shapes(&self, computed: Option<Vec<String>>, name: &Node<'pr>) -> Vec<Option<String>> {
        match computed {
            Some(names) => names.into_iter().map(Some).collect(),
            None => vec![self.name_shape(name)],
        }
    }

    /// What each local here is of the names a macro's caller hands it: the
    /// method's parameters, and a block variable iterating its splat.
    fn handed(&self) -> Vec<(String, String)> {
        let mut handed = self
            .frames
            .last()
            .map(|f| f.handed.clone())
            .unwrap_or_default();
        handed.extend(self.handed_loops.iter().cloned());
        handed
    }

    /// Does a block around here leave `self` alone? Only a literal list's
    /// iteration is known to, among the blocks open in this body.
    fn blocks_are_loops(&self) -> bool {
        let depth = self.frames.len();
        let loops = self.loop_frames.iter().filter(|d| **d == depth).count();
        self.frames.last().is_some_and(|f| f.blocks == loops)
    }

    /// The name a `define_method` block defines, when it defines exactly one —
    /// what a `super` inside it looks up. `None` for anything else, including
    /// a looped name that spells several.
    fn defined_method(&self, call: &ruby_prism::CallNode<'pr>) -> Option<DefinedBy> {
        if !matches!(
            method_name(call)?.as_str(),
            "define_method" | "define_singleton_method"
        ) {
            return None;
        }
        if !on_self(call) || self.in_method_body() {
            return Some(DefinedBy::Elsewhere);
        }
        let Some(first) = arg_nodes(call).into_iter().next() else {
            return Some(DefinedBy::Elsewhere);
        };
        Some(DefinedBy::Scope(
            self.computed_names(&first)
                .filter(|names| names.len() == 1)
                .and_then(|mut names| names.pop()),
        ))
    }

    /// Every name a method-name argument can be: a literal, a string built
    /// from a loop's variable, or the variable itself (DEC-131). `None`
    /// unless the source spells each of them.
    fn computed_names(&self, node: &Node<'pr>) -> Option<Vec<String>> {
        if let Some(name) = literal_name(node) {
            return Some(vec![name]);
        }
        if let Some(read) = node.as_local_variable_read_node() {
            return self
                .loop_values
                .iter()
                .rev()
                .find(|(bound, _)| bound.as_bytes() == read.name().as_slice())
                .map(|(_, values)| values.clone());
        }
        self.interpolated_names(node)
    }

    /// `[:before, :after, :around].each do |callback| … end`, or the same over
    /// a constant this file assigns such a list — the iteration whose body
    /// can be read as if it were written out once per name.
    ///
    /// Deliberately narrow: `each` or `reverse_each`, exactly one required
    /// block parameter, and a list every element of which is a literal name.
    /// A constant another file assigns is that blob's fact, and not read.
    fn literal_each(&self, call: &ruby_prism::CallNode<'pr>) -> Option<(String, Vec<String>)> {
        if !matches!(method_name(call)?.as_str(), "each" | "reverse_each") {
            return None;
        }
        let receiver = call.receiver()?;
        let values = match every_element_literal(&receiver) {
            Some(values) => values,
            None => self.name_array(&const_name(&receiver)?)?,
        };
        Some((sole_block_param(call)?, values))
    }

    /// `attrs.each do |name|` over a method's splat: the block variable,
    /// and the names the splat is handed (DEC-162).
    fn splat_each(&self, call: &ruby_prism::CallNode<'pr>) -> Option<(String, String)> {
        if method_name(call)?.as_str() != "each" {
            return None;
        }
        let read = call.receiver()?.as_local_variable_read_node()?;
        let template = self
            .frames
            .last()?
            .handed
            .iter()
            .find(|(p, t)| p.as_bytes() == read.name().as_slice() && t.ends_with("*}"))?
            .1
            .clone();
        Some((sole_block_param(call)?, template))
    }

    /// The literal list a constant read here names, looked up lexically
    /// among the ones this file assigns.
    fn name_array(&self, constant: &str) -> Option<Vec<String>> {
        (0..=self.nesting.len()).find_map(|depth| {
            self.name_arrays
                .get(&(self.nesting[depth..].to_vec(), constant.to_string()))
                .cloned()
        })
    }

    /// `config.include Helpers` inside `RSpec.configure do |config|` mixes
    /// Helpers into every example group, and `config.extend` extends them
    /// (DEC-088). A metadata filter after the module is not read.
    fn handle_configure_mixin(&mut self, call: &ruby_prism::CallNode<'pr>) {
        let Some(relation) = method_name(call).and_then(|name| mixin_relation(&name)) else {
            return;
        };
        let on_config = call
            .receiver()
            .and_then(|r| r.as_local_variable_read_node())
            .is_some_and(|read| {
                self.configure_params
                    .iter()
                    .any(|param| read.name().as_slice() == param.as_bytes())
            });
        if !on_config {
            return;
        }
        let owner = vec![format!("::{}", crate::core::rspec::EXAMPLE_GROUP)];
        for arg in arg_nodes(call).iter().rev() {
            let Some(target) = const_name(arg) else {
                continue;
            };
            let pos = self.pos(arg.location().start_offset());
            self.facts.ancestry.push(Ancestry {
                owner: owner.clone(),
                relation,
                target,
                pos,
            });
        }
    }

    /// Inside a class method, a call that defines a method named by the
    /// method's first parameter makes the method a macro (DEC-085):
    /// `define_method(name)`, `define_singleton_method(name)`, or another
    /// macro of this scope handed `name`.
    fn note_definer(&mut self, call: &ruby_prism::CallNode<'pr>) {
        let Some(frame) = self.frames.last() else {
            return;
        };
        let (Some(param), Some(method)) = (frame.definer.clone(), frame.method.clone()) else {
            return;
        };
        if !on_self(call) {
            return;
        }
        let hands_param = arg_nodes(call)
            .first()
            .and_then(|arg| arg.as_local_variable_read_node())
            .is_some_and(|read| read.name().as_slice() == param.as_bytes());
        let Some(name) = method_name(call).filter(|_| hands_param) else {
            return;
        };
        let singleton = match name.as_str() {
            "define_singleton_method" => Some(true),
            "define_method" => Some(self.singleton_exec > 0),
            other => self
                .definers
                .get(&(self.nesting.clone(), other.to_string()))
                .copied(),
        };
        if let Some(singleton) = singleton {
            self.definers
                .insert((self.nesting.clone(), method), singleton);
        }
    }

    /// `define_example_method :it` — a macro of this scope, naming the method
    /// it defines. Written where the name is, like any macro (DEC-085).
    fn handle_definer_call(
        &mut self,
        call: &ruby_prism::CallNode<'pr>,
        macro_name: &str,
        args: &[Node<'pr>],
    ) {
        let Some(first) = args.first() else { return };
        let Some(name) = literal_name(first) else {
            return;
        };
        let singleton = self.definers[&(self.nesting.clone(), macro_name.to_string())];
        let at = first
            .as_symbol_node()
            .and_then(|symbol| symbol.value_loc())
            .unwrap_or_else(|| first.location());
        let loc = call.location();
        let mut def = self.def(name, Kind::Method, loc.start_offset(), loc.end_offset());
        def.pos = self.pos(at.start_offset());
        def.singleton = singleton;
        def.via = Some(macro_name.to_string());
        self.push_def(def);
    }

    /// The class a `class_eval`/`module_exec` block is evaluated in, as a
    /// scope to push: the constant it is sent to, or the constant a
    /// parameter defaults to (`def enable(host = ::Host); host.module_exec do`).
    /// A bare name inside another scope is left alone, since only a lookup
    /// could say which constant it is (DEC-086).
    fn evaluated_in(&self, call: &ruby_prism::CallNode<'pr>) -> Option<String> {
        let name = method_name(call)?;
        if !matches!(
            name.as_str(),
            "class_eval" | "class_exec" | "module_eval" | "module_exec"
        ) || !arg_nodes(call).is_empty()
        {
            return None;
        }
        let receiver = call.receiver()?;
        let written = match receiver.as_local_variable_read_node() {
            Some(read) => {
                let local = String::from_utf8(read.name().as_slice().to_vec()).ok()?;
                self.frames
                    .last()?
                    .const_defaults
                    .iter()
                    .find(|(param, _)| *param == local)
                    .map(|(_, constant)| constant.clone())?
            }
            None => const_name(&receiver)?,
        };
        let placeable =
            written.starts_with("::") || written.contains("::") || self.nesting.is_empty();
        placeable.then_some(written)
    }

    /// Whether this call's block is RSpec's, and what it runs as (DEC-084).
    ///
    /// A group opens at `RSpec.describe` anywhere outside a method, at a bare
    /// `describe` at the top of a file, and at either inside another group.
    /// Examples, hooks and `let`s open only inside a group.
    fn spec_block(&self, call: &ruby_prism::CallNode<'pr>) -> Option<SpecBlock> {
        call.block()?.as_block_node()?;
        if self.in_method_body() {
            return None;
        }
        let name = method_name(call)?;
        let name = name.as_str();
        let on_rspec = call
            .receiver()
            .and_then(|r| const_name(&r))
            .is_some_and(|r| r.trim_start_matches("::") == "RSpec");
        let implicit = call.receiver().is_none();
        let in_group = self.in_group_body();
        let at_top = self.frames.len() == 1 && !self.minitest;
        let opens_group = (GROUP_METHODS.contains(&name)
            && (on_rspec || implicit && (in_group || at_top)))
            || (NESTED_GROUP_METHODS.contains(&name) && implicit && in_group);
        if opens_group {
            if SHARED_METHODS.contains(&name)
                && !in_group
                && let Some(shared) = arg_nodes(call).first().and_then(literal_name)
            {
                return Some(SpecBlock::Shared(crate::core::rspec::base_name(&shared)));
            }
            return Some(SpecBlock::Group(self.description(call)));
        }
        (EXAMPLE_METHODS.contains(&name) && implicit && in_group).then_some(SpecBlock::Example)
    }

    /// A group's description: its first argument, as RSpec reads it.
    fn description(&self, call: &ruby_prism::CallNode<'pr>) -> String {
        let Some(first) = arg_nodes(call).into_iter().next() else {
            return String::new();
        };
        if let Some(name) = const_name(&first).or_else(|| literal_name(&first)) {
            return name;
        }
        let at = first.location();
        self.text(at.start_offset(), at.end_offset())
    }

    /// The nesting segment for a group opened here, numbered as RSpec numbers
    /// a name its siblings already have.
    fn group_segment(&mut self, description: &str) -> String {
        let base = crate::core::rspec::base_name(description);
        let seen = self
            .group_names
            .entry(self.nesting.clone())
            .or_default()
            .entry(base.clone())
            .or_default();
        *seen += 1;
        let name = match *seen {
            1 => base,
            n => format!("{base}_{n}"),
        };
        crate::core::rspec::segment(&name)
    }

    /// The shared group a call in a group's body includes, as its module: a
    /// literal name, since that is RSpec's key for it (DEC-092).
    fn shared_included(&self, call: &ruby_prism::CallNode<'pr>) -> Option<String> {
        if call.receiver().is_some() || !SHARED_INCLUDERS.contains(&method_name(call)?.as_str()) {
            return None;
        }
        let name = literal_name(arg_nodes(call).first()?)?;
        Some(crate::core::rspec::shared_module(
            &crate::core::rspec::base_name(&name),
        ))
    }

    /// Where a group names the shared group it includes, so a click on the
    /// literal is a click on the group (DEC-124).
    fn note_shared_name(&mut self, call: &ruby_prism::CallNode<'pr>) {
        if !rspec::in_group(&self.nesting) {
            return;
        }
        let (Some(module), Some(written)) = (
            self.shared_included(call),
            arg_nodes(call).into_iter().next(),
        ) else {
            return;
        };
        let (start, end) = (
            written.location().start_offset(),
            written.location().end_offset(),
        );
        let pos = self.pos(start);
        if self.pos(end).line == pos.line {
            self.facts
                .shared_names
                .push((pos, (end - start) as u32, module));
        }
    }

    /// `include_context "x"` and `include_examples "x"` include a shared
    /// group into the group they are written in (DEC-092).
    fn handle_shared_include(&mut self, call: &ruby_prism::CallNode<'pr>) {
        let into_this_group = method_name(call)
            .is_some_and(|name| matches!(name.as_str(), "include_context" | "include_examples"));
        if !into_this_group || !self.writes_group_members() {
            return;
        }
        if let Some(module) = self.shared_included(call) {
            self.facts
                .shared_includes
                .push((self.nesting.clone(), module));
        }
    }

    /// `RSpec::Matchers.define :name` makes `name` a method of RSpec::Matchers,
    /// which every example group includes; so do `define_negated_matcher` and
    /// `alias_matcher` for their first name (DEC-091). Inside a group, the
    /// same calls define the group's own, as `let` does.
    fn handle_custom_matcher(&mut self, call: &ruby_prism::CallNode<'pr>) {
        if self.in_method_body() {
            return;
        }
        let Some(via) = method_name(call) else { return };
        if !MATCHER_DEFINERS.contains(&via.as_str()) {
            return;
        }
        let on_matchers = call
            .receiver()
            .and_then(|r| const_name(&r))
            .is_some_and(|r| r.trim_start_matches("::") == crate::core::rspec::MATCHERS);
        if !on_matchers {
            return;
        }
        let Some(first) = arg_nodes(call).into_iter().next() else {
            return;
        };
        let (Some(name), Some(at)) = (
            first.as_symbol_node().and_then(|_| literal_name(&first)),
            first.as_symbol_node().and_then(|symbol| symbol.value_loc()),
        ) else {
            return;
        };
        let (start, end) = (call.location().start_offset(), call.location().end_offset());
        let mut def = self.def(name, Kind::Method, start, end);
        def.nesting = vec![format!("::{}", crate::core::rspec::MATCHERS)];
        def.pos = self.pos(at.start_offset());
        def.via = Some(format!("{}.{via}", crate::core::rspec::MATCHERS));
        self.facts.defs.push(def);
    }

    /// What a `let` block returns, as an assignment's value is read, with
    /// `described_class` as the constant the group describes (DEC-096).
    fn let_value(&self, value: &Node<'pr>) -> ValueShape {
        let described = self.described.last().cloned().flatten();
        let is_described_class = |node: &Node<'pr>| {
            node.as_call_node().is_some_and(|c| {
                c.receiver().is_none()
                    && c.arguments().is_none()
                    && method_name(&c).as_deref() == Some("described_class")
            })
        };
        if let Some(class) = described {
            if is_described_class(value) {
                return ValueShape::Const(class);
            }
            if let Some(call) = value.as_call_node()
                && method_name(&call).as_deref() == Some("new")
                && call.receiver().is_some_and(|r| is_described_class(&r))
            {
                return ValueShape::New(class);
            }
        }
        match value_shape(value) {
            // A local read in the block was written in the block, which the
            // file's assignments do not place.
            ValueShape::Same(_) | ValueShape::LocalCall { .. } => ValueShape::Other,
            shape => shape,
        }
    }

    /// `expect(x).to be_empty`: a matcher handed to an expectation asks the
    /// expectation's subject, so remember what that was until the matcher's
    /// call is recorded (DEC-090). `x.should be_empty` is the older spelling.
    fn note_matcher_subject(&mut self, call: &ruby_prism::CallNode<'pr>) {
        let Some(name) = method_name(call) else {
            return;
        };
        let subject = match name.as_str() {
            "to" | "not_to" | "to_not" => {
                let Some(target) = call.receiver().and_then(|r| r.as_call_node()) else {
                    return;
                };
                if target.receiver().is_some() || target.block().is_some() {
                    return;
                }
                match method_name(&target).as_deref() {
                    Some("expect") => match arg_nodes(&target).as_slice() {
                        [value] => self.subject_of(value),
                        _ => return,
                    },
                    Some("is_expected") => Sent::SUBJECT,
                    _ => return,
                }
            }
            "should" | "should_not" => match call.receiver() {
                Some(value) => self.subject_of(&value),
                None => Sent::SUBJECT,
            },
            _ => return,
        };
        let Some(matcher) = arg_nodes(call)
            .into_iter()
            .next()
            .and_then(|a| a.as_call_node())
        else {
            return;
        };
        if matcher.receiver().is_none()
            && let Some(message) = matcher.message_loc()
        {
            self.matcher_subjects
                .insert(message.start_offset(), subject);
        }
    }

    /// An expression as a receiver, as `record_call` reads one.
    fn subject_of(&self, value: &Node<'pr>) -> Sent {
        let (recv, recv_text) = receiver_shape(value);
        Sent {
            singleton: self.self_is_class(),
            recv_pos: (recv == RecvShape::Local).then(|| self.pos(value.location().start_offset())),
            recv_value: (recv == RecvShape::Other)
                .then(|| self.recv_value(value))
                .flatten(),
            recv,
            recv_text,
        }
    }

    /// `let(:x)`, `let!(:x)`, `subject(:x)` and `subject` define a method on
    /// the group, named by the symbol. Written where the name is, so a click
    /// on the symbol is a click on the definition (DEC-084).
    fn handle_group_member(&mut self, call: &ruby_prism::CallNode<'pr>) {
        if !self.in_group_body() || call.receiver().is_some() {
            return;
        }
        let Some(via) = method_name(call) else { return };
        if !matches!(via.as_str(), "let" | "let!" | "subject" | "subject!")
            && !MATCHER_DEFINERS.contains(&via.as_str())
        {
            return;
        }
        let mut names: Vec<(String, usize)> = Vec::new();
        if let Some(first) = arg_nodes(call).into_iter().next()
            && let Some(name) = literal_name(&first)
        {
            let at = first
                .as_symbol_node()
                .and_then(|symbol| symbol.value_loc())
                .unwrap_or_else(|| first.location());
            names.push((name, at.start_offset()));
        }
        // `subject` itself is written at the block, so a click on the word
        // `subject` there still asks what the macro is.
        if via.starts_with("subject")
            && let Some(block) = call.block().and_then(|b| b.as_block_node())
        {
            names.push(("subject".to_string(), block.opening_loc().start_offset()));
        }
        let (start, end) = (call.location().start_offset(), call.location().end_offset());
        let value = matches!(via.as_str(), "let" | "let!" | "subject" | "subject!")
            .then(|| {
                let body = call.block()?.as_block_node()?.body()?;
                let last = body.as_statements_node()?.body().iter().last()?;
                Some(self.let_value(&last))
            })
            .flatten();
        for (name, at) in names {
            let mut def = self.def(name, Kind::Method, start, end);
            def.pos = self.pos(at);
            def.via = Some(via.clone());
            def.value = value.clone();
            self.push_def(def);
        }
    }

    /// Every name an interpolated string can spell, given what the enclosing
    /// loop bound. `None` unless the whole name is knowable.
    ///
    /// One interpolation, and it must be a bare read of a bound block
    /// parameter. `"#{a}_#{b}"`, `"#{thing.name}"` and `"#{CONST}"` all return
    /// nothing: a name half-guessed is worse than a name not offered, because
    /// the lookup would find it and stop.
    fn interpolated_names(&self, node: &Node<'pr>) -> Option<Vec<String>> {
        let parts: Vec<Node<'pr>> = match node {
            _ if node.as_interpolated_string_node().is_some() => {
                node.as_interpolated_string_node()?.parts().iter().collect()
            }
            _ if node.as_interpolated_symbol_node().is_some() => {
                node.as_interpolated_symbol_node()?.parts().iter().collect()
            }
            _ => return None,
        };
        let mut before = String::new();
        let mut after = String::new();
        let mut values: Option<&Vec<String>> = None;
        for part in &parts {
            if let Some(text) = part.as_string_node() {
                let text = String::from_utf8(text.unescaped().to_vec()).ok()?;
                if values.is_none() {
                    &mut before
                } else {
                    &mut after
                }
                .push_str(&text);
                continue;
            }
            let embedded = part.as_embedded_statements_node()?;
            let mut statements: Vec<Node<'pr>> = embedded.statements()?.body().iter().collect();
            if statements.len() != 1 || values.is_some() {
                return None;
            }
            let read = statements.pop()?.as_local_variable_read_node()?;
            let read = String::from_utf8(read.name().as_slice().to_vec()).ok()?;
            values = self
                .loop_values
                .iter()
                .rev()
                .find(|(bound, _)| *bound == read)
                .map(|(_, values)| values);
            values?;
        }
        let values = values?;
        Some(
            values
                .iter()
                .map(|value| format!("{before}{value}{after}"))
                .collect(),
        )
    }

    /// `class_methods do … end` — ActiveSupport::Concern's `module ClassMethods`.
    ///
    /// The block form and the nested-module form are the same declaration:
    /// Concern creates `M::ClassMethods` either way and extends it into every
    /// includer. Leaving the block unmodelled put its methods on the concern
    /// itself, as *instance* methods, where a class-level call cannot reach
    /// them — and worse, a mixin written inside it (`include StepsHelpers`)
    /// became an instance-side edge of the concern rather than a class-side one
    /// of every includer. On discourse that one shape is the largest single
    /// bucket of declined app sites.
    ///
    /// No `include` edge is emitted: Concern *extends* this module, and the
    /// tree layer already does that for whichever classes include the concern.
    fn handle_class_methods(
        &mut self,
        call: &ruby_prism::CallNode<'pr>,
        args: &[Node<'pr>],
    ) -> bool {
        // `class_methods` takes no arguments and a block. Anything else is
        // somebody else's method of the same name.
        if !args.is_empty() || self.nesting.is_empty() || self.in_method_body() {
            return false;
        }
        let Some(block) = call.block().and_then(|b| b.as_block_node()) else {
            return false;
        };
        let name = "ClassMethods".to_string();
        let mut def = self.def(
            name.clone(),
            Kind::Module,
            call.location().start_offset(),
            call.location().end_offset(),
        );
        def.via = Some("class_methods".to_string());
        self.push_def(def);

        self.enter(Some(name), Opens::Scope);
        if let Some(body) = block.body() {
            self.visit(&body);
        }
        self.leave();
        true
    }

    /// `concerning :Name do … end` — Rails' inline concern.
    ///
    /// Two statements written as one: a `module Name` nested in this scope that
    /// extends `ActiveSupport::Concern`, and an `include Name` right after it.
    /// Both halves are facts, so both are emitted — without the module the
    /// methods inside land on the class itself with the wrong owner, and without
    /// the edge the class never reaches them.
    ///
    /// `included do … end` inside the block is left as the ordinary call it is:
    /// its body runs against the including class, which is a tree question, not
    /// a blob one.
    fn handle_concerning(&mut self, call: &ruby_prism::CallNode<'pr>, args: &[Node<'pr>]) -> bool {
        let Some(name) = args.first().and_then(literal_name) else {
            return false;
        };
        // A concern names a constant. Anything else is somebody else's method
        // that happens to share the name.
        if !name.starts_with(|c: char| c.is_ascii_uppercase()) {
            return false;
        }
        let Some(block) = call.block().and_then(|b| b.as_block_node()) else {
            return false;
        };

        let start = args[0].location().start_offset();
        let mut def = self.def(
            name.clone(),
            Kind::Module,
            start,
            call.location().end_offset(),
        );
        def.via = Some("concerning".to_string());
        self.push_def(def);
        self.facts.ancestry.push(Ancestry {
            owner: self.nesting.clone(),
            relation: Relation::Include,
            target: name.clone(),
            pos: self.pos(start),
        });

        self.enter(Some(name), Opens::Scope);
        if let Some(body) = block.body() {
            self.visit(&body);
        }
        self.leave();
        true
    }

    /// `self.table_name = "legacy_posts"` — a model pointing at a table that is
    /// not the one its name implies.
    ///
    /// Recorded as the method Rails really does define, with the table in
    /// `target`. The *join* to that table's columns is a tree question: the
    /// schema is a different blob, and only an assembled namespace can put them
    /// together (the same shape as a concern's `ClassMethods`).
    fn handle_table_name(&mut self, call: &ruby_prism::CallNode<'pr>) {
        if method_name(call).as_deref() != Some("table_name=") {
            return;
        }
        if !call.receiver().is_some_and(|r| r.as_self_node().is_some()) {
            return;
        }
        let Some(table) = arg_nodes(call).first().and_then(literal_name) else {
            return;
        };
        let loc = call.location();
        let mut def = self.def(
            "table_name".to_string(),
            Kind::Method,
            loc.start_offset(),
            loc.end_offset(),
        );
        def.singleton = true;
        def.via = Some("table_name".into());
        def.target = Some(table);
        self.push_def(def);
    }

    /// `enum :status, { draft: 0, … }` — what ActiveRecord::Enum generates:
    /// the attribute's reader and writer, the mapping's class method, and per
    /// member a predicate, a bang setter, a scope and its `not_` scope, with
    /// the member names built as Rails builds them (`enums`).
    fn handle_enum(&mut self, call: &ruby_prism::CallNode<'pr>, args: &[Node<'pr>]) -> bool {
        let declared = enums::declared(args);
        if declared.is_empty() {
            return false;
        }
        let loc = call.location();
        let (start, end) = (loc.start_offset(), loc.end_offset());
        let in_singleton = self.in_singleton();
        for decl in declared {
            // At the attribute, like every macro-generated definition: left at
            // the call's own offset they would sit where `enum` does, and a
            // click on `enum` would answer one of them.
            let at = self.pos(decl.at);
            let attribute = &decl.attribute;
            // The reader returns the member's name, whatever the column
            // stores, so it is a String where the schema says Integer. It
            // comes first: a click on the attribute answers the first
            // definition written there.
            let mut made = vec![
                (attribute.clone(), false, at, Some("String")),
                (format!("{attribute}="), false, at, None),
                (macros::pluralize(attribute), true, at, None),
            ];
            for (label, offset) in &decl.members {
                let Some(name) = decl.method_name(label) else {
                    continue;
                };
                let at = self.pos(*offset);
                if decl.instance_methods {
                    made.push((format!("{name}?"), false, at, None));
                    made.push((format!("{name}!"), false, at, None));
                }
                if decl.scopes {
                    made.push((format!("not_{name}"), true, at, None));
                    made.push((name, true, at, None));
                }
            }
            for (name, singleton, at, returns) in made {
                let writer = name.ends_with('=');
                let mut def = self.def(name, Kind::Method, start, end);
                def.pos = at;
                def.via = Some("enum".into());
                def.singleton = singleton || in_singleton;
                def.sig_returns = returns.map(str::to_string);
                if writer {
                    def.params = vec![Param {
                        kind: ParamKind::Req,
                        name: "value".into(),
                    }];
                }
                self.route_to_includer(&mut def);
                self.push_def(def);
            }
        }
        true
    }

    /// `db/schema.rb`'s `create_table "posts" do |t| … end` — the attribute
    /// methods Rails generates for every column.
    ///
    /// This is ruby-lsp-rails' capability without a running app, and the point
    /// is not that `post.body` exists but that it has a **type**: a column's
    /// SQL type names a class, which makes every attribute a typed receiver.
    ///
    /// The table attaches to a model by Rails' `posts` → `Post` convention,
    /// applied here rather than in the tree because it is a pure function of
    /// the table name. A model that overrides `self.table_name` is a known gap
    /// (DEC-022): the override lives in a different blob.
    fn handle_create_table(&mut self, call: &ruby_prism::CallNode<'pr>) {
        if method_name(call).as_deref() != Some("create_table") {
            return;
        }
        let args = arg_nodes(call);
        let Some(table) = args.first().and_then(literal_name) else {
            return;
        };
        let Some(block) = call.block().and_then(|b| b.as_block_node()) else {
            return;
        };
        // The block parameter is what column declarations are called on.
        let builder = block
            .parameters()
            .and_then(|p| p.as_block_parameters_node())
            .and_then(|p| p.parameters())
            .and_then(|p| p.requireds().iter().next())
            .and_then(|p| p.as_required_parameter_node())
            .and_then(|p| String::from_utf8(p.name().as_slice().to_vec()).ok());
        let Some(builder) = builder else { return };
        let Some(body) = block.body().and_then(|b| b.as_statements_node()) else {
            return;
        };

        let owner = macros::table_to_class(&table);
        let mut columns: Vec<(String, Option<&'static str>)> = Vec::new();
        for statement in body.body().iter() {
            let Some(inner) = statement.as_call_node() else {
                continue;
            };
            // Only calls on the block parameter declare columns.
            let on_builder = inner
                .receiver()
                .and_then(|r| r.as_local_variable_read_node())
                .and_then(|l| String::from_utf8(l.name().as_slice().to_vec()).ok())
                .is_some_and(|name| name == builder);
            if !on_builder {
                continue;
            }
            let Some(kind) = method_name(&inner) else {
                continue;
            };
            match kind.as_str() {
                // `t.timestamps` is two datetime columns spelled as one call.
                "timestamps" => {
                    columns.push(("created_at".into(), macros::column_class("datetime")));
                    columns.push(("updated_at".into(), macros::column_class("datetime")));
                }
                // `t.references :author` is the `author_id` column. The
                // `author` reader is the model's `belongs_to`, not the table's.
                "references" | "belongs_to" => {
                    for arg in arg_nodes(&inner) {
                        if let Some(name) = literal_name(&arg) {
                            columns.push((format!("{name}_id"), macros::column_class("integer")));
                        }
                    }
                }
                _ if macros::is_column_type(&kind) => {
                    let class = macros::column_class(&kind);
                    for arg in arg_nodes(&inner) {
                        if let Some(name) = literal_name(&arg) {
                            columns.push((name, class));
                        }
                    }
                }
                _ => continue,
            }
        }

        let loc = call.location();
        let (start, end) = (loc.start_offset(), loc.end_offset());
        for (column, class) in columns {
            // Getter, setter, predicate, and the dirty tracking code calls
            // (DEC-111).
            let dirty = macros::dirty(&column)
                .into_iter()
                .map(|made| (made.name, false));
            for (name, writer) in [
                (column.clone(), false),
                (format!("{column}="), true),
                (format!("{column}?"), false),
            ]
            .into_iter()
            .chain(dirty)
            {
                let mut def = self.def(name.clone(), Kind::Method, start, end);
                def.nesting = vec![owner.clone()];
                def.via = Some("schema".into());
                if writer {
                    def.params = vec![Param {
                        kind: ParamKind::Req,
                        name: "value".into(),
                    }];
                } else if name == column {
                    def.sig_returns = class.map(str::to_string);
                }
                self.push_def(def);
            }
        }
    }

    /// A Rails class macro that defines methods — `delegate`, the association
    /// family, `scope`, and the accessor macros.
    ///
    /// Consumed as a *definition* rather than a call, the same way `attr_reader`
    /// already is. Session 6's audit showed why it matters: a method a DSL
    /// defines is absent from the index without being absent from the program,
    /// which made "nothing defines this name" the weakest thing this engine
    /// could say about a reference (DEC-021).
    fn handle_dsl(
        &mut self,
        call: &ruby_prism::CallNode<'pr>,
        macro_name: &str,
        args: &[Node<'pr>],
    ) -> bool {
        // `delegate` without `to:` is not a delegation: refuse rather than
        // guess, because a wrong method name is worse than an unmodelled one.
        let delegate_to = keyword_literal(args, "to");
        if macro_name == "delegate" && delegate_to.is_none() {
            return false;
        }
        // `prefix:` renames every generated method, by a rule rather than a
        // guess: `true` takes the `to:` target, a symbol is used as written.
        // Refusing here left every prefixed delegation unmodelled.
        let prefix = match (macro_name, keyword_value(args, "prefix")) {
            ("delegate", Some(value)) => match literal_name(&value) {
                Some(name) => Some(name),
                None if value.as_true_node().is_some() => delegate_to.clone(),
                // A computed prefix is a name we cannot know.
                None => return false,
            },
            _ => None,
        };
        let class_name = keyword_literal(args, "class_name");
        // `define_model_callbacks :initialize, only: :after` makes only the
        // `after_` half. A computed `only:` narrows to nothing rather than
        // being ignored, because ignoring it would invent the other two.
        let only: Option<Vec<String>> = keyword_value(args, "only").map(|value| {
            every_element_literal(&value)
                .or_else(|| literal_name(&value).map(|n| vec![n]))
                .unwrap_or_default()
        });

        let loc = call.location();
        let (start, end) = (loc.start_offset(), loc.end_offset());
        let visibility = self.visibility();
        let in_singleton = self.in_singleton();
        let mut any = false;

        // A splat of a constant this blob assigned a symbol array is still a
        // list of literal names; anything else computed produces nothing.
        let mut names: Vec<(String, Pos)> = Vec::new();
        for arg in args {
            let pos = self.pos(arg.location().start_offset());
            if let Some(literal) = literal_name(arg) {
                names.push((literal, pos));
                continue;
            }
            if let Some(splat) = arg.as_splat_node()
                && let Some(inner) = splat.expression()
                && let Some(constant) = const_name(&inner)
                && let Some(listed) = self.symbol_arrays.get(&constant)
            {
                names.extend(listed.iter().map(|name| (name.clone(), pos)));
            }
        }
        // `alias_attribute :new, :old` defines only the new name.
        if macro_name == "alias_attribute" {
            names.truncate(1);
        }
        // `has_secure_password` alone is `has_secure_password :password`. Its
        // methods sit just past the macro's name, where no click lands on it.
        if names.is_empty()
            && let Some(default) = macros::default_argument(macro_name)
        {
            let past = call.message_loc().map_or(start, |m| m.end_offset());
            names.push((default.to_string(), self.pos(past)));
        }
        let off = |key: &str| keyword_value(args, key).is_some_and(|v| v.as_false_node().is_some());
        let accessor_options = macro_name == "class_attribute" || macro_name.contains("attr_");

        for (literal, pos) in names {
            let associated = macros::associated_class(macro_name, &literal, class_name.as_deref());

            for made in macros::generated(macro_name, &literal) {
                if off("reset_token") && made.name.contains("reset_token") {
                    continue;
                }
                if accessor_options && macros::dropped_by(&made, off) {
                    continue;
                }
                if let Some(only) = &only
                    && !only
                        .iter()
                        .any(|kind| made.name.starts_with(&format!("{kind}_")))
                {
                    continue;
                }
                let name = match &prefix {
                    Some(prefix) => format!("{prefix}_{}", made.name),
                    None => made.name.clone(),
                };
                let mut def = self.def(name, Kind::Method, start, end);
                def.pos = pos;
                def.via = Some(macro_name.to_string());
                def.visibility = visibility;
                // `class << self` still governs which side these land on.
                def.singleton = made.singleton || in_singleton;
                self.route_to_includer(&mut def);
                if made.writer {
                    def.params = vec![Param {
                        kind: ParamKind::Req,
                        name: "value".into(),
                    }];
                }
                // ActiveSupport generates `def name(...)`: it takes whatever
                // the target does.
                if macro_name == "delegate" {
                    def.params = vec![Param {
                        kind: ParamKind::Rest,
                        name: "...".into(),
                    }];
                    // What it sends the name to, which `--refs` types
                    // (DEC-166). A prefixed one sends another name.
                    if prefix.is_none() {
                        def.target = delegate_to.clone();
                    }
                }
                def.sig_returns = made.returns.map(str::to_string);
                // A singular association's reader has a determinate type, which
                // makes it a receiver source and not merely a method.
                if !made.writer
                    && made.name == literal
                    && let Some(class) = &associated
                {
                    def.sig_returns = Some(class.clone());
                }
                self.push_def(def);
                any = true;
            }
        }
        if any {
            // The arguments are still constants in their own right. A
            // scope's lambda is its body, run on the relation (DEC-116).
            for arg in args {
                let body = macro_name == "scope" && is_lambda(arg);
                self.scope_body += usize::from(body);
                self.visit(arg);
                self.scope_body -= usize::from(body);
            }
        }
        any
    }

    /// `store :settings, accessors: [:theme]` and `store_accessor :settings,
    /// :theme` — accessors for keys of a serialized attribute, with the
    /// dirty tracking ActiveRecord::Store writes for each. The first argument
    /// is the store, not an accessor; `prefix:` and `suffix:` rename by
    /// Rails' rule, `true` taking the store's name.
    fn handle_store(
        &mut self,
        call: &ruby_prism::CallNode<'pr>,
        macro_name: &str,
        args: &[Node<'pr>],
    ) -> bool {
        let Some((store, keys)) = args.split_first() else {
            return false;
        };
        let Some(store) = literal_name(store) else {
            return false;
        };
        let listed = |node: &Node<'pr>| -> Vec<(String, usize)> {
            match node.as_array_node() {
                Some(array) => array
                    .elements()
                    .iter()
                    .filter_map(|e| Some((literal_name(&e)?, e.location().start_offset())))
                    .collect(),
                None => literal_name(node)
                    .map(|n| vec![(n, node.location().start_offset())])
                    .unwrap_or_default(),
            }
        };
        let keys: Vec<(String, usize)> = if macro_name == "store" {
            keyword_value(args, "accessors")
                .map(|v| listed(&v))
                .unwrap_or_default()
        } else {
            keys.iter().flat_map(listed).collect()
        };
        let affix = |key: &str| -> Option<Option<String>> {
            match keyword_value(args, key) {
                None => Some(None),
                Some(v) if v.as_true_node().is_some() => Some(Some(store.clone())),
                Some(v) if v.as_false_node().is_some() || v.as_nil_node().is_some() => Some(None),
                Some(v) => literal_name(&v).map(Some),
            }
        };
        // A computed affix is a name we cannot spell.
        let (Some(prefix), Some(suffix)) = (affix("prefix"), affix("suffix")) else {
            return false;
        };
        let loc = call.location();
        let (start, end) = (loc.start_offset(), loc.end_offset());
        let visibility = self.visibility();
        let mut any = false;
        for (key, at) in keys {
            let key = format!(
                "{}{key}{}",
                prefix.as_ref().map(|p| format!("{p}_")).unwrap_or_default(),
                suffix.as_ref().map(|s| format!("_{s}")).unwrap_or_default()
            );
            for made in macros::store_accessor(&key) {
                let mut def = self.def(made.name, Kind::Method, start, end);
                def.pos = self.pos(at);
                def.via = Some(macro_name.to_string());
                def.visibility = visibility;
                if made.writer {
                    def.params = vec![Param {
                        kind: ParamKind::Req,
                        name: "value".into(),
                    }];
                }
                self.push_def(def);
                any = true;
            }
        }
        any
    }

    /// `delegate_missing_to :account` — ActiveSupport writes a
    /// `method_missing` and a `respond_to_missing?` that hand any name the
    /// class lacks to `account`. Declared with the target, so a lookup that
    /// finds nothing can follow it (DEC-112).
    fn handle_delegate_missing(&mut self, args: &[Node<'pr>]) -> bool {
        if self.in_method_body() {
            return false;
        }
        let Some(first) = args.first() else {
            return false;
        };
        let Some(target) = literal_name(first) else {
            return false;
        };
        let at = first.location();
        for name in ["method_missing", "respond_to_missing?"] {
            let mut def = self.def(
                name.to_string(),
                Kind::Method,
                at.start_offset(),
                at.end_offset(),
            );
            def.via = Some("delegate_missing_to".into());
            def.target = Some(target.clone());
            def.visibility = self.visibility();
            def.singleton = self.in_singleton();
            def.params = vec![Param {
                kind: ParamKind::Rest,
                name: "args".into(),
            }];
            self.push_def(def);
        }
        true
    }

    /// Forwardable's `def_delegator :@engine, :stop, :halt` and
    /// `def_delegators :@engine, :start, :rev` — ActiveSupport's `delegate`
    /// under another name. The first argument is the accessor, never a method
    /// defined here; `def_delegator`'s third argument renames the method.
    /// The `single` forms define singleton methods.
    fn handle_forwardable(&mut self, macro_name: &str, args: &[Node<'pr>], many: bool) -> bool {
        let Some((_, methods)) = args.split_first() else {
            return false;
        };
        let named: Vec<&Node<'pr>> = if many {
            methods.iter().collect()
        } else {
            match methods {
                [method] | [_, method] => vec![method],
                _ => return false,
            }
        };
        let mut any = false;
        for arg in named {
            let Some(name) = literal_name(arg) else {
                continue;
            };
            let at = arg.location();
            let mut def = self.def(name, Kind::Method, at.start_offset(), at.end_offset());
            def.via = Some(
                if many {
                    "def_delegators"
                } else {
                    "def_delegator"
                }
                .into(),
            );
            def.visibility = self.visibility();
            def.singleton = macro_name.contains("single") || self.in_singleton();
            // Whatever the target takes.
            def.params = vec![Param {
                kind: ParamKind::Rest,
                name: "args".into(),
            }];
            self.push_def(def);
            any = true;
        }
        any
    }

    fn handle_alias_method(
        &mut self,
        call: &ruby_prism::CallNode<'pr>,
        args: &[Node<'pr>],
    ) -> bool {
        if args.len() != 2 {
            return false;
        }
        let (Some(new), Some(old)) = (literal_name(&args[0]), literal_name(&args[1])) else {
            return false;
        };
        let loc = call.location();
        let mut def = self.def(new, Kind::Method, loc.start_offset(), loc.end_offset());
        def.singleton = self.in_singleton();
        def.via = Some("alias_method".into());
        self.bind_alias(&mut def, &old);
        def.target = Some(old);
        self.push_def(def);
        true
    }

    /// The body an alias copies, when it is a method written earlier in this
    /// scope: its position, and its parameters, which the alias takes too.
    fn bind_alias(&self, alias: &mut Def, target: &str) {
        let Some(body) = self.facts.defs.iter().rev().find(|d| {
            d.kind == Kind::Method
                && d.name == target
                && d.nesting == alias.nesting
                && d.singleton == alias.singleton
                && matches!(d.via.as_deref(), None | Some("define_method"))
        }) else {
            return;
        };
        alias.target_pos = Some(body.pos);
        alias.params = body.params.clone();
    }

    fn handle_visibility(
        &mut self,
        call: &ruby_prism::CallNode<'pr>,
        macro_name: &str,
        args: &[Node<'pr>],
    ) -> bool {
        let visibility = match macro_name {
            "private" => Visibility::Private,
            "protected" => Visibility::Protected,
            // `module_function` makes the instance copy private.
            "module_function" => Visibility::Private,
            _ => Visibility::Public,
        };

        if args.is_empty() {
            // Bare modifier: flip the state for the rest of this body. It does
            // not leak out, because the frame is popped with the scope.
            let frame = self.frame();
            frame.visibility = visibility;
            if macro_name == "module_function" {
                frame.module_function = true;
            }
            return true;
        }

        // `private def foo` / `private attr_reader :x` — the definition is the
        // argument, so make it visible to the nested visit and put it back.
        if args
            .iter()
            .any(|a| a.as_def_node().is_some() || a.as_call_node().is_some())
        {
            let saved = self.visibility();
            let saved_mf = self.frames.last().is_some_and(|f| f.module_function);
            {
                let frame = self.frame();
                frame.visibility = visibility;
                frame.module_function = macro_name == "module_function";
            }
            for arg in args {
                self.visit(arg);
            }
            let frame = self.frame();
            frame.visibility = saved;
            frame.module_function = saved_mf;
            return true;
        }

        // `private :foo` names a method that may live in an ancestor, so it is
        // its own fact — an assertion about visibility, not a definition. The
        // `via` column is what tells the two apart.
        let loc = call.location();
        let (start, end) = (loc.start_offset(), loc.end_offset());
        let singleton = self.in_singleton();
        for arg in args {
            let Some(target) = literal_name(arg) else {
                continue;
            };
            let mut def = self.def(target, Kind::Method, start, end);
            def.pos = self.pos(arg.location().start_offset());
            def.via = Some(macro_name.to_string());
            def.visibility = visibility;
            def.singleton = singleton;
            self.push_def(def);
            if macro_name == "module_function" {
                let mut copy = self.facts.defs.last().expect("just pushed").clone();
                copy.singleton = true;
                copy.visibility = Visibility::Public;
                self.push_def(copy);
            }
        }
        true
    }

    fn record_assign(&mut self, target: String, value: &Node<'pr>, offset: usize) {
        let pos = self.pos(offset);
        self.facts.assigns.push(Assign {
            target,
            value: value_shape(value),
            nesting: self.nesting.clone(),
            pos,
        });
    }

    /// A call site, with the receiver shape that the resolution ladder climbs.
    fn record_call(&mut self, call: &ruby_prism::CallNode<'pr>) {
        let Some(name) = method_name(call) else {
            return;
        };
        // A call with no message location is synthesized (`a[0] += 1` and
        // friends); there is no name in the source to navigate from.
        let Some(message) = call.message_loc() else {
            return;
        };
        self.note_shared_name(call);
        let (recv, recv_text) = match call.receiver() {
            None => (RecvShape::Implicit, None),
            Some(r) => receiver_shape(&r),
        };
        let recv_pos = (recv == RecvShape::Local)
            .then(|| {
                call.receiver()
                    .map(|r| self.pos(r.location().start_offset()))
            })
            .flatten();
        let recv_value = match (recv, call.receiver()) {
            (RecvShape::Other, Some(r)) => self.recv_value(&r),
            // The group methods RSpec exposes on `main` (DEC-115).
            (RecvShape::Implicit, None)
                if self.nesting.is_empty()
                    && matches!(
                        self.spec_block(call),
                        Some(SpecBlock::Group(_) | SpecBlock::Shared(_))
                    ) =>
            {
                Some(RecvValue::Main)
            }
            _ => None,
        };
        let argc = argc_of(&arg_nodes(call));
        let pos = self.pos(message.start_offset());
        // Not `in_singleton()`: that answers "is a `def` here a singleton
        // method", which is a different question. A bare call in a class body
        // dispatches on the class even though a `def` there does not.
        let singleton = self.self_is_class();
        let block_owner = self.open_blocks.last().copied().flatten();
        let block = call.block().is_some();
        let stands_for = (recv == RecvShape::Implicit && rspec::in_group(&self.nesting))
            .then(|| rspec::predicate(&name))
            .flatten()
            .map(|predicate| {
                let subject = self
                    .matcher_subjects
                    .remove(&message.start_offset())
                    .unwrap_or(Sent::UNTYPED);
                self.sent(predicate, subject, pos, argc, block)
            });
        self.facts.calls.push(Call {
            name,
            recv,
            recv_text,
            nesting: self.nesting.clone(),
            singleton,
            recv_pos,
            recv_value,
            block_owner,
            in_example: self.frames.last().is_some_and(|f| f.example),
            in_scope: self.scope_body > 0 && !self.in_method_body(),
            argc,
            block,
            pos,
            stands_for,
        });
        self.record_symbol_arguments(call);
        self.record_block_pass(call);
        self.record_body_call(call);
    }

    /// A class or module body's call on itself, outside any method, with
    /// the literal names it is handed: what a macro written in another file
    /// runs on (DEC-162).
    fn record_body_call(&mut self, call: &ruby_prism::CallNode<'pr>) {
        if !on_self(call) || !self.self_is_the_scope() || !self.evals.is_empty() {
            return;
        }
        let Some(name) = method_name(call) else {
            return;
        };
        // A Rails macro trekr declares by name already says what it makes
        // (DEC-111); its string of code would only hedge those names.
        if !macros::generated(&name, "x").is_empty() {
            return;
        }
        let args = arg_nodes(call)
            .iter()
            .filter(|arg| arg.as_keyword_hash_node().is_none())
            .map(literal_name)
            .collect();
        let line = self.pos(call.location().start_offset()).line;
        self.facts.body_calls.push(BodyCall {
            name,
            nesting: self.nesting.clone(),
            args,
            line,
        });
    }

    /// `parts.reject(&:empty?)` calls `empty?` on each element the block is
    /// handed (DEC-094). Recorded as the symbol it is written as, standing for
    /// a call on the elements, whose class is known only for a literal list
    /// of one class.
    fn record_block_pass(&mut self, call: &ruby_prism::CallNode<'pr>) {
        let Some(symbol) = call
            .block()
            .and_then(|b| b.as_block_argument_node())
            .and_then(|b| b.expression())
            .and_then(|e| e.as_symbol_node())
        else {
            return;
        };
        let (Some(name), Some(at)) = (
            String::from_utf8(symbol.unescaped().to_vec()).ok(),
            symbol.value_loc(),
        ) else {
            return;
        };
        let element = call.receiver().and_then(|r| {
            let array = r.as_array_node()?;
            let mut classes = array.elements().iter().map(|e| literal_class(&e));
            let first = classes.next()??;
            classes.all(|class| class == Some(first)).then_some(first)
        });
        let each = Sent {
            recv_value: element.map(RecvValue::Literal),
            ..Sent::UNTYPED
        };
        let pos = self.pos(at.start_offset());
        let stands_for = Some(self.sent(name.clone(), each, pos, Some(0), false));
        self.facts.calls.push(Call {
            name,
            recv: RecvShape::Symbol,
            recv_text: None,
            nesting: self.nesting.clone(),
            singleton: false,
            recv_pos: None,
            recv_value: None,
            block_owner: None,
            in_example: false,
            in_scope: false,
            argc: None,
            block: false,
            pos,
            stands_for,
        });
    }

    /// The call a name stands for, sent where it really goes.
    fn sent(&self, name: String, to: Sent, pos: Pos, argc: Option<u32>, block: bool) -> Box<Call> {
        Box::new(Call {
            name,
            recv: to.recv,
            recv_text: to.recv_text,
            nesting: self.nesting.clone(),
            singleton: to.singleton,
            recv_pos: to.recv_pos,
            recv_value: to.recv_value,
            block_owner: self.open_blocks.last().copied().flatten(),
            in_example: self.frames.last().is_some_and(|f| f.example),
            in_scope: self.scope_body > 0 && !self.in_method_body(),
            argc,
            block,
            pos,
            stands_for: None,
        })
    }

    /// What a symbol handed to this call names a method of, when that is a
    /// rule rather than a guess (DEC-093): the receiver of a reflective call
    /// (`send(:x)`, `obj.respond_to?(:x)`), and `self`'s instances for a
    /// class-level call in a class or module body (`before_action :x`,
    /// `alias_method :new, :old`, `private :x`).
    fn symbol_receiver(&self, call: &ruby_prism::CallNode<'pr>, index: usize) -> Option<Sent> {
        let name = method_name(call)?;
        if REFLECTIVE.contains(&name.as_str()) {
            if index > 0 {
                return None;
            }
            return Some(match call.receiver() {
                None => Sent::to_self(self.self_is_class()),
                Some(receiver) => self.subject_of(&receiver),
            });
        }
        let class_level = call.receiver().is_none()
            && !self.in_method_body()
            && !self.nesting.is_empty()
            && !rspec::in_group(&self.nesting)
            && (self.frames.last().is_some_and(|f| f.blocks == 0) || self.in_includer_body());
        if !class_level {
            return None;
        }
        match name.as_str() {
            "private_class_method" | "public_class_method" => Some(Sent::to_self(true)),
            // These name a callback chain, which is not the method of that name.
            "define_callbacks" | "define_model_callbacks" | "set_callback" | "skip_callback" => {
                None
            }
            _ => Some(Sent::to_self(self.in_singleton())),
        }
    }

    /// A receiver worth typing that is not a name: the call before this one in
    /// a chain, found again by its position, or a literal.
    fn recv_value(&self, node: &Node<'pr>) -> Option<RecvValue> {
        if let Some(class) = literal_class(node) {
            return Some(RecvValue::Literal(class));
        }
        // `(a + b).abs` — one expression in parentheses is that expression.
        if let Some(parens) = node.as_parentheses_node() {
            let statements = parens.body()?.as_statements_node()?;
            let mut body = statements.body().iter();
            let only = body.next()?;
            return body
                .next()
                .is_none()
                .then(|| self.recv_value(&only))
                .flatten();
        }
        let call = node.as_call_node()?;
        method_name(&call)?;
        Some(RecvValue::Call(
            self.pos(call.message_loc()?.start_offset()),
        ))
    }

    /// `super`, as a call of the enclosing method's name. Outside a method
    /// there is no name for it to look up, and Ruby raises.
    fn record_super(&mut self, offset: usize, argc: Option<u32>, block: bool) {
        let Some(name) = self.frames.last().and_then(|f| f.method.clone()) else {
            return;
        };
        let pos = self.pos(offset);
        self.facts.calls.push(Call {
            name,
            recv: RecvShape::Super,
            recv_text: None,
            nesting: self.nesting.clone(),
            singleton: self.self_is_class(),
            recv_pos: None,
            recv_value: None,
            block_owner: None,
            in_example: false,
            in_scope: false,
            stands_for: None,
            argc,
            block,
            pos,
        });
    }

    /// `Point = Struct.new(:x, :y) do … end`, `Data.define`, `Class.new(Base)`
    /// and `Module.new`, assigned to a constant: a class or module body.
    ///
    /// The block is `class_eval`'d, so its methods are the new class's and a
    /// call in it dispatches on it. One liberty is taken: constants written
    /// in the block are scoped as if it were a `class` body, where Ruby keeps
    /// the enclosing scope's (ARCHITECTURE, known gaps).
    fn handle_made(
        &mut self,
        name: String,
        node: &ruby_prism::ConstantWriteNode<'pr>,
        call: &ruby_prism::CallNode<'pr>,
        made: Made,
    ) {
        let loc = node.name_loc();
        let kind = match made {
            Made::Module => Kind::Module,
            _ => Kind::Class,
        };
        let def = self.def(
            name.clone(),
            kind,
            loc.start_offset(),
            node.location().end_offset(),
        );
        self.push_def(def);

        let args = arg_nodes(call);
        let parent = match made {
            Made::Struct | Made::Data => call.receiver().and_then(|r| const_name(&r)),
            // `Class.new` alone inherits Object, which the tree already gives
            // every class with no written superclass.
            Made::Class => args.first().map(|arg| {
                const_name(arg).unwrap_or_else(|| {
                    let at = arg.location();
                    self.text(at.start_offset(), at.end_offset())
                })
            }),
            Made::Module => None,
        };
        if let Some(target) = parent {
            let mut owner = self.nesting.clone();
            owner.insert(0, name.clone());
            self.facts.ancestry.push(Ancestry {
                owner,
                relation: Relation::Superclass,
                target,
                pos: self.pos(call.location().start_offset()),
            });
        }

        // Members: every literal argument, less the class name `Struct.new`
        // takes first when handed a string, and the options hash.
        let mut members: Vec<(String, usize)> = args
            .iter()
            .filter_map(|arg| Some((literal_name(arg)?, arg.location().start_offset())))
            .collect();
        if made == Made::Struct
            && args.first().is_some_and(|a| a.as_string_node().is_some())
            && members
                .first()
                .is_some_and(|(n, _)| n.starts_with(|c: char| c.is_ascii_uppercase()))
        {
            members.remove(0);
        }
        if !matches!(made, Made::Struct | Made::Data) {
            members.clear();
        }
        let (start, end) = (call.location().start_offset(), call.location().end_offset());
        let via = match made {
            Made::Struct => "Struct.new",
            _ => "Data.define",
        };
        self.nesting.insert(0, name.clone());
        for (member, at) in members {
            let mut reader = self.def(member.clone(), Kind::Method, start, end);
            reader.pos = self.pos(at);
            reader.via = Some(via.into());
            self.push_def(reader);
            if made == Made::Struct {
                let mut writer = self.def(format!("{member}="), Kind::Method, start, end);
                writer.pos = self.pos(at);
                writer.via = Some(via.into());
                writer.params = vec![Param {
                    kind: ParamKind::Req,
                    name: member,
                }];
                self.push_def(writer);
            }
        }
        self.nesting.remove(0);

        // Still an ordinary call, whose receiver and arguments are references.
        self.record_call(call);
        if let Some(receiver) = call.receiver() {
            self.visit(&receiver);
        }
        if let Some(arguments) = call.arguments() {
            self.visit_arguments_node(&arguments);
        }
        if let Some(block) = call.block().and_then(|b| b.as_block_node()) {
            self.enter(Some(name), Opens::Scope);
            if let Some(body) = block.body() {
                self.visit(&body);
            }
            self.leave();
        }
    }

    /// `after_create :ensure_thing` invokes `ensure_thing`, and nothing in the
    /// file writes it as a call (DEC-037).
    ///
    /// Recorded for a symbol in **argument position of any call**, not for a
    /// curated list of macros. An app's own DSL is unknowable — discourse's
    /// `step`/`policy`/`model` is a thousand sites and no Rails list would
    /// contain it — and the errors here are asymmetric: a spurious reference
    /// costs a missed dead-code candidate, a missed one costs a false "nothing
    /// uses this". Err toward recording, and tier it as `possible` so the
    /// weakness is disclosed rather than hidden.
    fn record_symbol_arguments(&mut self, call: &ruby_prism::CallNode<'pr>) {
        for (index, arg) in arg_nodes(call).into_iter().enumerate() {
            // Only a bare symbol. A hash's *keys* are options, not methods, and
            // its values are visited on their own as ordinary arguments.
            let Some(symbol) = arg.as_symbol_node() else {
                continue;
            };
            let Some(name) = String::from_utf8(symbol.unescaped().to_vec()).ok() else {
                continue;
            };
            if !name.starts_with(|c: char| c.is_ascii_lowercase() || c == '_') {
                continue;
            }
            let Some(loc) = symbol.value_loc() else {
                continue;
            };
            let pos = self.pos(loc.start_offset());
            let stands_for = self
                .symbol_receiver(call, index)
                .map(|to| self.sent(name.clone(), to, pos, None, false));
            self.facts.calls.push(Call {
                name,
                recv: RecvShape::Symbol,
                recv_text: None,
                nesting: self.nesting.clone(),
                singleton: false,
                recv_pos: None,
                recv_value: None,
                block_owner: None,
                in_example: false,
                in_scope: false,
                // Unknowable: whatever invokes it decides the arity.
                argc: None,
                block: false,
                pos,
                stands_for,
            });
        }
    }
}

/// Does this file write Minitest's spec DSL? Its `describe` is RSpec's
/// syntax, so the tell is what else it writes: Minitest's expectations
/// (`_(x).must_equal`, `wont_be`), a require of its spec, or its constant
/// (DEC-095). Only as code: a string or comment holding the words is not
/// the call (DEC-123). The bytes are searched first, since most files
/// hold none of them.
fn minitest_spec(src: &[u8], root: &Node<'_>) -> bool {
    const TELLS: [&[u8]; 4] = [b".must_", b".wont_", b"minitest/spec", b"Minitest::Spec"];
    let written = TELLS
        .iter()
        .any(|tell| src.windows(tell.len()).any(|w| w == *tell));
    if !written {
        return false;
    }
    let mut finder = MinitestTell { found: false };
    finder.visit(root);
    finder.found
}

/// Finds a Minitest tell in code (DEC-095, DEC-123).
struct MinitestTell {
    found: bool,
}

impl<'pr> Visit<'pr> for MinitestTell {
    fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
        let name = node.name();
        let name = name.as_slice();
        let expectation =
            node.receiver().is_some() && (name.starts_with(b"must_") || name.starts_with(b"wont_"));
        let requires_spec = name == b"require"
            && node
                .arguments()
                .and_then(|args| args.arguments().iter().next())
                .and_then(|arg| arg.as_string_node())
                .is_some_and(|path| path.unescaped() == b"minitest/spec");
        if expectation || requires_spec {
            self.found = true;
            return;
        }
        ruby_prism::visit_call_node(self, node);
    }

    fn visit_constant_path_node(&mut self, node: &ruby_prism::ConstantPathNode<'pr>) {
        let spec = node.name().is_some_and(|name| name.as_slice() == b"Spec")
            && node
                .parent()
                .and_then(|parent| parent.as_constant_read_node())
                .is_some_and(|parent| parent.name().as_slice() == b"Minitest");
        if spec {
            self.found = true;
            return;
        }
        ruby_prism::visit_constant_path_node(self, node);
    }
}

/// Positional argument count, or `None` when a splat hides the real count —
/// saying so rather than reporting a number that is wrong.
fn argc_of(args: &[Node<'_>]) -> Option<u32> {
    let mut argc = 0u32;
    for arg in args {
        if arg.as_splat_node().is_some()
            || arg.as_forwarding_arguments_node().is_some()
            || arg.as_assoc_splat_node().is_some()
        {
            return None;
        }
        argc += 1;
    }
    Some(argc)
}

/// The class a literal produces. Worth typing now that core is indexed: an
/// accumulator written `out = []` is an Array, and `Array#<<` is findable.
fn literal_class(node: &Node<'_>) -> Option<&'static str> {
    Some(match node {
        _ if node.as_array_node().is_some() => "Array",
        _ if node.as_hash_node().is_some() => "Hash",
        _ if node.as_string_node().is_some() => "String",
        _ if node.as_interpolated_string_node().is_some() => "String",
        _ if node.as_symbol_node().is_some() => "Symbol",
        _ if node.as_integer_node().is_some() => "Integer",
        _ if node.as_float_node().is_some() => "Float",
        _ if node.as_regular_expression_node().is_some() => "Regexp",
        _ if node.as_range_node().is_some() => "Range",
        _ => return None,
    })
}

/// The symbols in a literal array, seeing through `.freeze` — which is how a
/// constant array is idiomatically written, and so how Rails writes the one
/// that matters.
/// `[:a, :b, :c]` with **every** element a literal.
///
/// Stricter than `literal_symbol_array`, which drops what it cannot read: here
/// a single unreadable element means the list is not known, and half a list
/// would generate half a set of definitions while looking like a whole one.
fn every_element_literal(node: &Node<'_>) -> Option<Vec<String>> {
    if let Some(call) = node.as_call_node() {
        if !crate::core::IDENTITY.contains(&method_name(&call)?.as_str()) {
            return None;
        }
        return every_element_literal(&call.receiver()?);
    }
    let array = node.as_array_node()?;
    let elements: Vec<Node<'_>> = array.elements().iter().collect();
    let names: Vec<String> = elements.iter().filter_map(literal_name).collect();
    (!names.is_empty() && names.len() == elements.len()).then_some(names)
}

/// A string of code as pieces, when every interpolation in it is a local, or
/// a local through one of `RENDERS`, and its pieces are contiguous bytes of
/// the file. Read raw: the code keeps the file's lines.
fn code_pieces(node: &Node<'_>) -> Option<Vec<Piece>> {
    let text = |location: ruby_prism::Location<'_>| Piece::Text {
        start: location.start_offset(),
        end: location.end_offset(),
    };
    if let Some(string) = node.as_string_node() {
        return Some(vec![text(string.content_loc())]);
    }
    let mut pieces = Vec::new();
    let mut end: Option<usize> = None;
    for part in node.as_interpolated_string_node()?.parts().iter() {
        let at = part.location();
        if end.is_some_and(|end| end != at.start_offset()) {
            return None;
        }
        end = Some(at.end_offset());
        if part.as_string_node().is_some() {
            pieces.push(text(at));
            continue;
        }
        let embedded = part.as_embedded_statements_node()?;
        let statements: Vec<Node<'_>> = embedded.statements()?.body().iter().collect();
        let [statement] = statements.as_slice() else {
            return None;
        };
        let (read, render) = match statement.as_call_node() {
            Some(call) => {
                let method = method_name(&call)?;
                if !RENDERS.contains(&method.as_str()) || call.arguments().is_some() {
                    return None;
                }
                (
                    call.receiver()?.as_local_variable_read_node()?,
                    Some(method),
                )
            }
            None => (statement.as_local_variable_read_node()?, None),
        };
        pieces.push(Piece::Local {
            name: read.name().as_slice().to_vec(),
            render,
            at: at.start_offset(),
        });
    }
    Some(pieces)
}

/// `self.class`.
fn is_self_class(node: &Node<'_>) -> bool {
    node.as_call_node().is_some_and(|call| {
        method_name(&call).as_deref() == Some("class")
            && call.receiver().is_some_and(|r| r.as_self_node().is_some())
    })
}

/// The string an evaluator is handed, when the source spells it: a string,
/// or one through a method that leaves code as it is (`<<~RUBY.strip`).
fn spelled_code(arg: Node<'_>) -> Option<Node<'_>> {
    if arg.as_string_node().is_some() || arg.as_interpolated_string_node().is_some() {
        return Some(arg);
    }
    let call = arg.as_call_node()?;
    let keeps = matches!(
        method_name(&call)?.as_str(),
        "strip" | "lstrip" | "rstrip" | "chomp" | "squish" | "freeze" | "dup" | "to_s"
    );
    if !keeps || call.arguments().is_some() || call.block().is_some() {
        return None;
    }
    let inner = call.receiver()?;
    (inner.as_string_node().is_some() || inner.as_interpolated_string_node().is_some())
        .then_some(inner)
}

/// A string of code's text, with `*` for each interpolation.
fn starred_text(code: &Node<'_>, src: &[u8]) -> String {
    let raw = |at: ruby_prism::Location<'_>| {
        String::from_utf8_lossy(&src[at.start_offset()..at.end_offset().min(src.len())])
            .into_owned()
    };
    let mut text = String::new();
    if let Some(string) = code.as_string_node() {
        text = raw(string.content_loc());
    } else if let Some(string) = code.as_interpolated_string_node() {
        for part in string.parts().iter() {
            match part.as_string_node() {
                Some(_) => text.push_str(&raw(part.location())),
                None => text.push('*'),
            }
        }
    }
    text
}

/// The names a string of code calls that an interpolation spells part of —
/// `assign_nested_attributes_for_*_association` — as shapes: not `def`s,
/// symbols or variables (DEC-163).
fn string_calls(code: &Node<'_>, src: &[u8]) -> Vec<String> {
    let text = starred_text(code, src);
    let bytes = text.as_bytes();
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'*';
    let mut shapes: Vec<String> = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if !word(bytes[at]) {
            at += 1;
            continue;
        }
        let start = at;
        while at < bytes.len() && word(bytes[at]) {
            at += 1;
        }
        if at < bytes.len() && matches!(bytes[at], b'?' | b'!') {
            at += 1;
        }
        let token = &text[start..at];
        let before = text[..start].trim_end();
        let named = token.contains('*')
            && spells_enough(token)
            && !token.starts_with(|c: char| c.is_ascii_digit() || c.is_ascii_uppercase())
            && !before.ends_with("def")
            && !before.ends_with("def self.")
            && !before.ends_with(':')
            && !before.ends_with('@')
            && !before.ends_with('$');
        if named && !shapes.iter().any(|s| s == token) {
            shapes.push(token.to_string());
        }
    }
    shapes
}

/// What a string of code defines, by its text with `*` for each
/// interpolation: the side and shape of each `def`. One unshaped entry for
/// either side when the text may make methods some other way, or spells no
/// `def` at all.
///
/// `handed` are a macro's positional parameters: an interpolation of the
/// `k`th is `{k}`, the name its caller hands it (DEC-162).
fn string_defs(
    code: &Node<'_>,
    src: &[u8],
    handed: &[(String, String)],
) -> Vec<(Option<bool>, Option<String>)> {
    let raw = |at: ruby_prism::Location<'_>| {
        String::from_utf8_lossy(&src[at.start_offset()..at.end_offset().min(src.len())])
            .into_owned()
    };
    let mut text = String::new();
    if let Some(string) = code.as_string_node() {
        text = raw(string.content_loc());
    } else if let Some(string) = code.as_interpolated_string_node() {
        for part in string.parts().iter() {
            match part.as_string_node() {
                Some(_) => text.push_str(&raw(part.location())),
                None => text.push_str(&handed_part(&part, handed)),
            }
        }
    }
    const ELSEWISE: [&str; 7] = [
        "define_method",
        "attr_",
        "alias",
        "delegate",
        "eval",
        "class <<",
        "method_missing",
    ];
    let anything = vec![(None, None)];
    if ELSEWISE.iter().any(|word| text.contains(word)) {
        return anything;
    }
    let mut defs: Vec<(Option<bool>, Option<String>)> = Vec::new();
    let bytes = text.as_bytes();
    let mut at = 0;
    while let Some(found) = text[at..].find("def") {
        let start = at + found;
        at = start + 3;
        let bounded = start == 0 || matches!(bytes[start - 1], b' ' | b'\t' | b'\n' | b';');
        if !bounded || !bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
            continue;
        }
        let rest = text[at..].trim_start();
        let (singleton, rest) = match rest.strip_prefix("self.") {
            Some(rest) => (true, rest),
            None => (false, rest),
        };
        let name: String = rest
            .chars()
            .take_while(|c| {
                c.is_alphanumeric() || matches!(c, '_' | '*' | '?' | '!' | '=' | '{' | '}')
            })
            .collect();
        let shape = name.chars().any(|c| c != '*').then_some(name);
        let def = (Some(singleton), shape);
        if !defs.contains(&def) {
            defs.push(def);
        }
    }
    if defs.is_empty() { anything } else { defs }
}

/// What each local of a string of code is while it is read: a name for some,
/// and the stand-in for the rest.
struct Values<'v> {
    bound: &'v [(Vec<u8>, String)],
}

impl Values<'_> {
    fn of(&self, local: &[u8]) -> &str {
        self.bound
            .iter()
            .find(|(name, _)| name == local)
            .map_or(UNSTATED, |(_, value)| value.as_str())
    }
}

/// A string of code a method evaluates on `self`, kept to be read where a
/// class body in the same file calls the method with literal names (DEC-163).
#[derive(Clone)]
struct StringMacro {
    by: String,
    pieces: Vec<Piece>,
    /// Each local the string interpolates, and which positional argument
    /// hands it.
    handed: Vec<(Vec<u8>, usize)>,
    /// What depends on the values, from the read where it is written: a
    /// class method's string has been read there, and only this is read
    /// again. `None` for a macro's, read whole at each caller.
    named: Option<Mentions>,
    /// The scope the method is written in.
    scope: Vec<String>,
}

/// How many methods one string of code may make, over all the values it is
/// read with, before it is marked instead (DEC-164).
const EXPANDED_PER_STRING: usize = 2_000;

/// How many methods strings of code may make in one file (DEC-164).
const EXPANDED_PER_FILE: usize = 20_000;

/// Where a read of a string of code found what depends on its values: the
/// `def`s and the calls that mention one, read again per value (DEC-132,
/// DEC-163), and the shapes of those calls' names.
#[derive(Clone, Default)]
struct Mentions {
    defs: HashSet<Pos>,
    calls: HashSet<Pos>,
    call_shapes: Vec<String>,
}

/// The locals a string of code interpolates, each once.
fn locals_of(pieces: &[Piece]) -> Vec<Vec<u8>> {
    let mut locals: Vec<Vec<u8>> = Vec::new();
    for piece in pieces {
        if let Piece::Local { name, .. } = piece
            && !locals.contains(name)
        {
            locals.push(name.clone());
        }
    }
    locals
}

/// The code a string of pieces evaluates, with each local's value.
fn render(pieces: &[Piece], values: &Values, file: &[u8]) -> Eval {
    let mut eval = Eval {
        src: Vec::new(),
        pieces: Vec::new(),
    };
    for piece in pieces {
        let start = eval.src.len();
        match piece {
            Piece::Text { start: from, end } => {
                eval.pieces.push((start, *from, true));
                eval.src.extend_from_slice(&file[*from..*end]);
            }
            Piece::Local { name, render, at } => {
                eval.pieces.push((start, *at, false));
                let value = values.of(name);
                let shown = match render.as_deref() {
                    Some("upcase") => value.to_uppercase(),
                    Some("downcase") => value.to_lowercase(),
                    Some("capitalize") => {
                        let mut chars = value.chars();
                        chars.next().map_or(String::new(), |first| {
                            first
                                .to_uppercase()
                                .chain(chars.map(|c| c.to_ascii_lowercase()))
                                .collect()
                        })
                    }
                    _ => value.to_string(),
                };
                eval.src.extend_from_slice(shown.as_bytes());
            }
        }
    }
    eval
}

/// What a `def new` makes, by every value it returns — each `return` and
/// the last expression (DEC-133, DEC-165): `Other.new(…)` makes an `Other`,
/// `super` whatever the next `new` up the chain makes. Several are joined by
/// `|`; a path that says neither is not counted, as DEC-133 counts none.
fn made_by_new(body: &Node<'_>) -> Option<String> {
    let statements = body.as_statements_node()?;
    let mut returned: Vec<Node<'_>> = Vec::new();
    for statement in statements.body().iter() {
        returns_in(&statement, &mut returned);
    }
    if let Some(last) = statements.body().iter().last() {
        returned.push(last);
    }
    let mut made: Vec<String> = Vec::new();
    for value in returned {
        let Some(kind) = made_kind(&value) else {
            continue;
        };
        if !made.contains(&kind) {
            made.push(kind);
        }
    }
    (!made.is_empty()).then(|| made.join("|"))
}

/// `super` or `Other.new(…)`, as `made_by_new` names it.
fn made_kind(value: &Node<'_>) -> Option<String> {
    if let Some(ret) = value.as_return_node() {
        return made_kind(&ret.arguments()?.arguments().iter().next()?);
    }
    if value.as_super_node().is_some() || value.as_forwarding_super_node().is_some() {
        return Some("super".to_string());
    }
    let call = value.as_call_node()?;
    if method_name(&call).as_deref() != Some("new") {
        return None;
    }
    const_name(&call.receiver()?)
}

/// The `return`s a body reaches without entering a block, a lambda or
/// another `def`, which return from something else.
fn returns_in<'pr>(node: &Node<'pr>, out: &mut Vec<Node<'pr>>) {
    struct Returns<'a, 'pr> {
        out: &'a mut Vec<Node<'pr>>,
    }
    impl<'pr> Visit<'pr> for Returns<'_, 'pr> {
        fn visit_return_node(&mut self, node: &ruby_prism::ReturnNode<'pr>) {
            self.out.push(node.as_node());
        }
        fn visit_block_node(&mut self, _: &ruby_prism::BlockNode<'pr>) {}
        fn visit_lambda_node(&mut self, _: &ruby_prism::LambdaNode<'pr>) {}
        fn visit_def_node(&mut self, _: &ruby_prism::DefNode<'pr>) {}
    }
    Returns { out }.visit(node);
}

/// Does a name or text a string of code produced spell the stand-in?
fn mentions_unstated(text: &str) -> bool {
    text.to_ascii_lowercase().contains(UNSTATED)
}

/// Does a shape spell enough of a name to pick out a few methods? `*` or
/// `*!` would caveat every method in a file.
fn spells_enough(shape: &str) -> bool {
    shape
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_')
        .count()
        >= 3
}

/// A name with the stand-in, in any case, as `*`: the shape of a call a
/// value names.
fn unstated_shape(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    let mut shape = String::new();
    let mut rest = 0;
    while let Some(at) = lower[rest..].find(UNSTATED) {
        shape.push_str(&name[rest..rest + at]);
        shape.push('*');
        rest += at + UNSTATED.len();
    }
    shape.push_str(&name[rest..]);
    shape
}

/// The name of a block's one required parameter, when that is all it takes.
fn sole_block_param(call: &ruby_prism::CallNode<'_>) -> Option<String> {
    let block = call.block()?.as_block_node()?;
    let params = block
        .parameters()?
        .as_block_parameters_node()?
        .parameters()?;
    let required: Vec<_> = params.requireds().iter().collect();
    let optionals = params.optionals().iter().count();
    if required.len() != 1 || params.rest().is_some() || optionals != 0 {
        return None;
    }
    required
        .first()?
        .as_required_parameter_node()
        .and_then(|p| String::from_utf8(p.name().as_slice().to_vec()).ok())
}

/// `[Hash, Array]`, or the same `.freeze`d: every element a constant as
/// written, or no list at all — half a list would look like a whole one.
fn literal_constant_array(node: &Node<'_>) -> Option<Vec<String>> {
    if let Some(call) = node.as_call_node() {
        if !crate::core::IDENTITY.contains(&method_name(&call)?.as_str()) {
            return None;
        }
        return literal_constant_array(&call.receiver()?);
    }
    let elements: Vec<Node<'_>> = node.as_array_node()?.elements().iter().collect();
    let constants: Vec<String> = elements.iter().filter_map(const_name).collect();
    (!constants.is_empty() && constants.len() == elements.len()).then_some(constants)
}

fn literal_symbol_array(node: &Node<'_>) -> Option<Vec<String>> {
    if let Some(array) = node.as_array_node() {
        let symbols: Vec<String> = array
            .elements()
            .iter()
            .filter_map(|element| literal_name(&element))
            .collect();
        return (!symbols.is_empty()).then_some(symbols);
    }
    let call = node.as_call_node()?;
    if !crate::core::IDENTITY.contains(&method_name(&call)?.as_str()) {
        return None;
    }
    literal_symbol_array(&call.receiver()?)
}

fn value_shape(node: &Node<'_>) -> ValueShape {
    if let Some(class) = literal_class(node) {
        return ValueShape::Literal(class);
    }
    if let Some(name) = const_name(node) {
        // `x = Foo` — the variable holds the class, not an instance of it.
        return ValueShape::Const(name);
    }
    if let Some(local) = node.as_local_variable_read_node() {
        return String::from_utf8(local.name().as_slice().to_vec())
            .map_or(ValueShape::Other, ValueShape::Same);
    }
    let Some(call) = node.as_call_node() else {
        return ValueShape::Other;
    };
    let Some(name) = method_name(&call) else {
        return ValueShape::Other;
    };
    match call.receiver() {
        None => ValueShape::SelfCall(name),
        Some(receiver) => {
            if crate::core::IDENTITY.contains(&name.as_str()) {
                // Whatever the receiver was, this still is.
                return value_shape(&receiver);
            }
            if let Some(recv) = const_name(&receiver) {
                return if name == "new" {
                    ValueShape::New(recv)
                } else {
                    ValueShape::ConstCall { recv, name }
                };
            }
            // `y.build`, and `y&.build` — safe navigation parses as an
            // ordinary call and types the same way.
            match receiver.as_local_variable_read_node() {
                Some(local) => match String::from_utf8(local.name().as_slice().to_vec()) {
                    Ok(recv) => ValueShape::LocalCall { recv, name },
                    Err(_) => ValueShape::Other,
                },
                None => ValueShape::Other,
            }
        }
    }
}

fn receiver_shape(node: &Node<'_>) -> (RecvShape, Option<String>) {
    if node.as_self_node().is_some() {
        return (RecvShape::SelfRecv, None);
    }
    if let Some(name) = const_name(node) {
        return (RecvShape::Const, Some(name));
    }
    if let Some(local) = node.as_local_variable_read_node() {
        let name = String::from_utf8(local.name().as_slice().to_vec()).ok();
        return (RecvShape::Local, name);
    }
    if let Some(ivar) = node.as_instance_variable_read_node() {
        let name = String::from_utf8(ivar.name().as_slice().to_vec()).ok();
        return (RecvShape::Ivar, name);
    }
    if let Some(cvar) = node.as_class_variable_read_node() {
        let name = String::from_utf8(cvar.name().as_slice().to_vec()).ok();
        return (RecvShape::Ivar, name);
    }
    (RecvShape::Other, None)
}

/// One blob's facts plus what it cost to produce them.
///
/// The cost fields exist for `--index --profile`; they are dropped on the way
/// into the store, which knows nothing about how long anything took.
pub(crate) struct Parsed {
    pub(crate) facts: Facts,
    pub(crate) bytes: u64,
    pub(crate) elapsed: std::time::Duration,
    pub(crate) path: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../tests/fixtures/widget.rb");

    fn facts() -> Facts {
        let facts = extract(FIXTURE.as_bytes());
        assert_eq!(facts.parse_errors, 0, "the fixture must be valid Ruby");
        facts
    }

    /// Every method definition, as `owner name` with the markers that matter.
    fn method(facts: &Facts, name: &str) -> Def {
        facts
            .defs
            .iter()
            .find(|d| d.kind == Kind::Method && d.name == name)
            .unwrap_or_else(|| panic!("no method {name} in {:?}", facts.defs))
            .clone()
    }

    #[test]
    fn records_classes_modules_and_their_lexical_nesting() {
        let facts = facts();
        let scopes: Vec<_> = facts
            .defs
            .iter()
            .filter(|d| matches!(d.kind, Kind::Class | Kind::Module))
            .map(|d| (d.name.as_str(), d.kind))
            .collect();
        assert_eq!(
            scopes,
            [
                ("Registry", Kind::Module),
                ("Trackable", Kind::Module),
                ("Widget", Kind::Class),
                ("Util", Kind::Module),
                // Reopening a class is a second definition, not a duplicate.
                ("Widget", Kind::Class),
            ]
        );
        assert_eq!(method(&facts, "title").nesting, ["Widget"]);
    }

    #[test]
    fn records_ancestry_edges_for_every_relation() {
        let facts = facts();
        let edges: Vec<_> = facts
            .ancestry
            .iter()
            .map(|a| (a.relation, a.target.as_str()))
            .collect();
        assert_eq!(
            edges,
            [
                (Relation::Superclass, "Base::Component"),
                (Relation::Include, "Trackable"),
                (Relation::Prepend, "Auditing"),
                (Relation::Extend, "Registry"),
            ]
        );
    }

    #[test]
    fn expands_attr_macros_into_the_methods_they_define() {
        let facts = facts();
        for (name, via) in [
            ("name", "attr_reader"),
            ("size", "attr_accessor"),
            ("size=", "attr_accessor"),
            ("label=", "attr_writer"),
        ] {
            assert_eq!(method(&facts, name).via.as_deref(), Some(via));
        }
        assert_eq!(
            method(&facts, "size=").params.len(),
            1,
            "a writer takes the value it writes"
        );
    }

    #[test]
    fn tracks_visibility_as_a_stack_that_does_not_leak() {
        let facts = facts();
        assert_eq!(method(&facts, "title").visibility, Visibility::Public);
        assert_eq!(method(&facts, "helper").visibility, Visibility::Private);
        // `class << self` opens a fresh body, so `private` above it is gone.
        assert_eq!(method(&facts, "build").visibility, Visibility::Public);
        // `public :another` asserts visibility without defining anything.
        let assertion = facts
            .defs
            .iter()
            .find(|d| d.name == "another" && d.via.is_some())
            .expect("public :another is its own fact");
        assert_eq!(assertion.visibility, Visibility::Public);
    }

    #[test]
    fn marks_singleton_methods_however_they_are_written() {
        let facts = facts();
        assert!(method(&facts, "lookup").singleton, "def self.lookup");
        assert!(method(&facts, "build").singleton, "inside class << self");
        assert!(!method(&facts, "title").singleton);
    }

    #[test]
    fn module_function_defines_both_a_private_instance_and_a_public_singleton() {
        let facts = facts();
        let both: Vec<_> = facts
            .defs
            .iter()
            .filter(|d| d.name == "normalize")
            .map(|d| (d.singleton, d.visibility))
            .collect();
        assert_eq!(
            both,
            [(true, Visibility::Public), (false, Visibility::Private)],
            "one def, two methods — so no later layer needs to know the macro"
        );
    }

    #[test]
    fn reads_parameters_in_rubys_own_vocabulary() {
        let facts = facts();
        let resize = method(&facts, "resize");
        let params: Vec<_> = resize
            .params
            .iter()
            .map(|p| (p.kind, p.name.as_str()))
            .collect();
        assert_eq!(
            params,
            [
                (ParamKind::Req, "width"),
                (ParamKind::Opt, "height"),
                (ParamKind::Rest, "rest"),
                (ParamKind::Keyreq, "depth"),
                (ParamKind::Key, "unit"),
                (ParamKind::Keyrest, "opts"),
                (ParamKind::Block, "blk"),
            ]
        );
    }

    #[test]
    fn reads_an_inline_sorbet_return_type() {
        let facts = facts();
        assert_eq!(
            method(&facts, "title").sig_returns.as_deref(),
            Some("String")
        );
        assert_eq!(method(&facts, "resize").sig_returns, None);
    }

    #[test]
    fn records_aliases_with_what_they_point_at() {
        let facts = facts();
        assert_eq!(method(&facts, "label").target.as_deref(), Some("name"));
        assert_eq!(method(&facts, "caption").target.as_deref(), Some("title"));
    }

    /// `super` looks up the name of the method it is written in — through a
    /// block, too — and outside any method there is no name to look up.
    #[test]
    fn a_super_is_a_call_of_its_enclosing_methods_name() {
        let facts = extract(
            b"class W < B\n  def save(x)\n    items.each { super(x) }\n  end\n  def self.build\n    super\n  end\nend\n",
        );
        let supers: Vec<(&str, bool, Option<u32>)> = facts
            .calls
            .iter()
            .filter(|c| c.recv == RecvShape::Super)
            .map(|c| (c.name.as_str(), c.singleton, c.argc))
            .collect();
        assert_eq!(supers, [("save", false, Some(1)), ("build", true, None)]);
        // Outside a method, on another object, or in a block run against
        // something the source does not name, the owner is not the scope.
        for unplaced in [
            "class W\n  super\nend\n",
            "class W\n  def @w.save\n    super\n  end\nend\n",
            "class W\n  Class.new(B) do\n    def save\n      super\n    end\n  end\nend\n",
            "class W\n  def self.make(n)\n    define_method(n) { super() }\n  end\nend\n",
        ] {
            let facts = extract(unplaced.as_bytes());
            assert!(
                facts.calls.iter().all(|c| c.recv != RecvShape::Super),
                "{unplaced}"
            );
        }
    }

    #[test]
    fn records_constants_and_follows_a_constant_alias() {
        let facts = facts();
        let consts: Vec<_> = facts
            .defs
            .iter()
            .filter(|d| d.kind == Kind::Constant)
            .map(|d| (d.name.as_str(), d.target.as_deref()))
            .collect();
        assert_eq!(
            consts,
            [("DEFAULT", None), ("ALIAS", Some("DEFAULT"))],
            "a constant assigned another constant is an alias, not a new namespace"
        );
    }

    #[test]
    fn classifies_every_receiver_shape() {
        let facts = facts();
        let shape = |name: &str| {
            facts
                .calls
                .iter()
                .find(|c| c.name == name)
                .unwrap_or_else(|| panic!("no call to {name}"))
                .clone()
        };
        assert_eq!(shape("helper").recv, RecvShape::Implicit);
        assert_eq!(shape("size=").recv, RecvShape::SelfRecv);
        assert_eq!(shape("lookup").recv, RecvShape::Const);
        assert_eq!(shape("lookup").recv_text.as_deref(), Some("Registry"));
        assert_eq!(shape("compute").recv, RecvShape::Local);
        assert_eq!(shape("upcase").recv, RecvShape::Ivar);
        assert_eq!(shape("upcase").recv_text.as_deref(), Some("@name"));
    }

    #[test]
    fn counts_positional_arguments_and_admits_when_a_splat_hides_them() {
        let facts = facts();
        let call = |name: &str| facts.calls.iter().find(|c| c.name == name).unwrap().clone();
        assert_eq!(call("compute").argc, Some(2));
        assert_eq!(call("new").argc, None, "a splat makes the count unknowable");
    }

    #[test]
    fn records_a_reference_for_every_segment_of_a_constant_path() {
        let facts = facts();
        let named: Vec<_> = facts
            .const_refs
            .iter()
            .map(|r| r.name.as_str())
            .filter(|n| n.starts_with("Base") || n.starts_with("Registry::"))
            .collect();
        assert_eq!(
            named,
            ["Base", "Base::Component", "Registry::DEFAULT"],
            "go-to-definition has to work on either half of A::B"
        );
    }

    #[test]
    fn carries_the_nesting_a_reference_will_be_resolved_in() {
        let facts = facts();
        let reference = facts
            .const_refs
            .iter()
            .find(|r| r.name == "Registry::DEFAULT")
            .expect("Registry::DEFAULT is referenced");
        assert_eq!(reference.nesting, ["Widget"]);
    }

    #[test]
    fn a_compact_module_path_opens_one_lexical_scope_not_two() {
        // Ruby's `Module.nesting` here is `[A::B]`: constants inside cannot
        // see `A`'s, and only the stack records that.
        let facts = extract(b"module A::B\n  C = 1\n  D\nend\n");
        let d = facts.const_refs.iter().find(|r| r.name == "D").unwrap();
        assert_eq!(d.nesting, ["A::B"]);
    }

    #[test]
    fn a_top_level_def_is_private_and_a_class_body_def_is_public() {
        let facts = extract(b"def loose\nend\nclass K\n  def tight\n  end\nend\n");
        assert_eq!(method(&facts, "loose").visibility, Visibility::Private);
        assert_eq!(method(&facts, "tight").visibility, Visibility::Public);
    }

    #[test]
    fn a_visibility_modifier_never_reaches_a_singleton_def() {
        let facts = extract(b"class K\n  private\n  def self.made\n  end\nend\n");
        assert_eq!(method(&facts, "made").visibility, Visibility::Public);
    }

    #[test]
    fn an_inline_modifier_applies_to_its_argument_only() {
        let facts = extract(b"class K\n  private def a\n  end\n  def b\n  end\nend\n");
        assert_eq!(method(&facts, "a").visibility, Visibility::Private);
        assert_eq!(method(&facts, "b").visibility, Visibility::Public);
    }

    #[test]
    fn a_dynamic_superclass_still_names_the_class_it_is_built_from() {
        let facts = extract(b"class K < Struct.new(:a)\nend\n");
        assert_eq!(facts.ancestry[0].target, "Struct");
    }

    #[test]
    fn unparseable_ruby_reports_the_errors_instead_of_pretending() {
        let facts = extract(b"class K\n  def broken(\nend\n");
        assert!(facts.parse_errors > 0, "a truncated def is a syntax error");
    }
}

#[cfg(test)]
mod rails_dsl_tests {
    use super::*;

    /// `concerning` is a module definition and an include written as one
    /// expression, so both halves have to come out.
    #[test]
    fn concerning_defines_a_nested_module_and_includes_it() {
        let facts = extract(
            b"class Widget\n\
              \x20 concerning :Tracking do\n\
              \x20   def track\n\
              \x20   end\n\
              \x20 end\n\
              end\n",
        );
        let module = facts
            .defs
            .iter()
            .find(|d| d.kind == Kind::Module)
            .expect("the concern is a module");
        assert_eq!(module.name, "Tracking");
        assert_eq!(module.nesting, ["Widget"]);
        assert_eq!(module.via.as_deref(), Some("concerning"));

        let method = facts
            .defs
            .iter()
            .find(|d| d.kind == Kind::Method)
            .expect("the block's methods belong to the concern");
        // Nesting is innermost-first.
        assert_eq!(method.nesting, ["Tracking", "Widget"]);

        let edge = facts.ancestry.first().expect("and the class includes it");
        assert_eq!(edge.relation, Relation::Include);
        assert_eq!(edge.target, "Tracking");
        assert_eq!(edge.owner, ["Widget"]);
    }

    /// `prefix:` renames what a delegation defines, by a rule Rails follows
    /// exactly. Refusing to model it left every prefixed delegation invisible.
    #[test]
    fn a_prefixed_delegation_defines_the_prefixed_name() {
        let names = |src: &[u8]| {
            extract(src)
                .defs
                .into_iter()
                .filter(|d| d.via.as_deref() == Some("delegate"))
                .map(|d| d.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(b"class W\n  delegate :region, to: :supplier, prefix: true\nend\n"),
            ["supplier_region"],
            "`prefix: true` takes the delegation target"
        );
        assert_eq!(
            names(b"class W\n  delegate :region, :code, to: :supplier, prefix: :home\nend\n"),
            ["home_region", "home_code"],
            "a symbol prefix is used as written, for every name"
        );
        assert!(
            names(b"class W\n  delegate :region, to: :supplier, prefix: PREFIX\nend\n").is_empty(),
            "a computed prefix is still a refusal — the name cannot be known"
        );
    }

    /// Actionpack's shape, and the reason `before_action` in a controller had
    /// no candidate at all: nothing that reads only `def` can see these.
    #[test]
    fn a_computed_name_over_a_literal_array_is_read_as_definitions() {
        let facts = extract(
            b"module M\n  [:before, :after, :around].each do |callback|\n    \
              define_method \"#{callback}_action\" do |*names, &blk|\n    end\n\n    \
              define_method \"skip_#{callback}_action\" do |*names|\n    end\n  end\nend\n",
        );
        let mut names: Vec<&str> = facts
            .defs
            .iter()
            .filter(|d| d.via.as_deref() == Some("define_method"))
            .map(|d| d.name.as_str())
            .collect();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                "after_action",
                "around_action",
                "before_action",
                "skip_after_action",
                "skip_around_action",
                "skip_before_action",
            ]
        );
        let one = facts
            .defs
            .iter()
            .find(|d| d.name == "before_action")
            .unwrap();
        assert_eq!(one.nesting, ["M"]);
        // The block is the method body, so its parameters are the method's.
        assert_eq!(one.params.first().map(|p| p.kind), Some(ParamKind::Rest));
    }

    /// A name half-guessed is worse than a name not offered: the lookup finds
    /// it and stops.
    #[test]
    fn a_name_that_is_not_fully_knowable_generates_nothing() {
        for source in [
            // the list is a constant, whose value is another blob's fact
            &b"module M\n  NAMES.each do |n|\n    define_method(\"#{n}_x\") {}\n  end\nend\n"[..],
            // an element we cannot read makes the whole list unknown
            &b"module M\n  [:a, other].each do |n|\n    define_method(\"#{n}_x\") {}\n  end\nend\n"[..],
            // two interpolations
            &b"module M\n  [:a].each do |n|\n    define_method(\"#{n}_#{n}\") {}\n  end\nend\n"[..],
            // not a bare read of the bound parameter
            &b"module M\n  [:a].each do |n|\n    define_method(\"#{n.to_s}\") {}\n  end\nend\n"[..],
            // no enclosing loop binds it
            &b"module M\n  define_method(\"#{whatever}_x\") {}\nend\n"[..],
            // deferred: runs against whatever `self` is when the method runs
            &b"module M\n  def setup\n    [:a].each { |n| define_method(\"#{n}_x\") {} }\n  end\nend\n"[..],
        ] {
            let facts = extract(source);
            assert!(
                facts
                    .defs
                    .iter()
                    .all(|d| d.via.as_deref() != Some("define_method")),
                "generated a name from {}",
                String::from_utf8_lossy(source)
            );
        }
    }

    /// The block form of `module ClassMethods`, which Concern creates either
    /// way — so a mixin inside it is a class-side ancestor of every includer.
    #[test]
    fn class_methods_opens_the_concerns_class_methods_module() {
        let facts = extract(
            b"module M\n  extend ActiveSupport::Concern\n  class_methods do\n    \
              include Helpers\n    def build\n    end\n  end\nend\n",
        );
        let module = facts
            .defs
            .iter()
            .find(|d| d.name == "ClassMethods")
            .expect("the block declares the module");
        assert_eq!(module.kind, Kind::Module);
        assert_eq!(module.nesting, ["M"]);
        let built = facts
            .defs
            .iter()
            .find(|d| d.name == "build")
            .expect("and the method inside it");
        assert_eq!(built.nesting, ["ClassMethods", "M"]);
        let edge = facts.ancestry.iter().find(|a| a.target == "Helpers");
        assert_eq!(
            edge.expect("the mixin is kept").owner,
            ["ClassMethods", "M"]
        );
    }

    /// `enum :segment, …` defines `Model.segments` — the mapping — as well as
    /// the members' predicates, and `suffix:` renames the members by Rails'
    /// rule while leaving the plural alone.
    #[test]
    fn an_enum_defines_the_attributes_plural_class_method() {
        let made = |src: &[u8]| -> Vec<String> {
            extract(src)
                .defs
                .into_iter()
                .filter(|d| d.via.as_deref() == Some("enum"))
                .map(|d| d.name)
                .collect()
        };
        let plain = made(b"class W\n  enum :segment, { primary: 0, secondary: 1 }\nend\n");
        assert!(
            plain.iter().any(|n| n == "segments"),
            "the mapping accessor: {plain:?}"
        );
        assert!(
            plain.iter().any(|n| n == "primary?"),
            "and the members: {plain:?}"
        );

        let renamed = made(b"class W\n  enum :segment, { primary: 0 }, suffix: true\nend\n");
        assert!(renamed.iter().any(|n| n == "segments"), "{renamed:?}");
        assert!(
            renamed.iter().any(|n| n == "primary_segment?"),
            "{renamed:?}"
        );
        assert!(!renamed.iter().any(|n| n == "primary?"), "{renamed:?}");
    }

    /// Somebody else's `class_methods` is not Concern's. Arguments are the
    /// cheap tell, and inventing a module on one would be worse than the gap.
    #[test]
    fn a_class_methods_that_takes_arguments_is_left_alone() {
        let facts = extract(b"class W\n  class_methods :a do\n  end\nend\n");
        assert!(facts.defs.iter().all(|d| d.name != "ClassMethods"));
    }

    /// An invented edge is worse than a missing one: this shape is Rails'
    /// `has_secure_password`, and recording it lexically put a module's
    /// instance methods into every ActiveRecord model's class-level chain.
    #[test]
    fn a_mixin_inside_a_method_is_not_this_scopes_ancestor() {
        let facts = extract(b"module M\n  def install\n    include Extra\n  end\nend\n");
        assert!(facts.ancestry.is_empty());
        // Still a call — `include` really is `Module#include`.
        assert!(facts.calls.iter().any(|c| c.name == "include"));
    }

    /// The rule is about *when* the line runs, not about what `self` is, so a
    /// class body keeps its edge while `def self.x` loses one.
    #[test]
    fn a_mixin_in_a_class_body_is_still_an_ancestor() {
        let body = extract(b"class W\n  include Extra\nend\n");
        assert_eq!(body.ancestry.len(), 1);
        let deferred = extract(b"class W\n  def self.widen\n    include Extra\n  end\nend\n");
        assert!(deferred.ancestry.is_empty());
    }

    /// Somebody else's `concerning` is not Rails'. A non-constant argument is
    /// the cheap tell, and guessing wrong would invent a module.
    #[test]
    fn a_concerning_that_names_no_constant_is_left_alone() {
        let facts = extract(b"class Widget\n  concerning :tracking do\n  end\nend\n");
        assert!(facts.defs.iter().all(|d| d.name != "tracking"));
        assert!(facts.ancestry.is_empty());
    }
}

#[cfg(test)]
mod macro_call_tests {
    use super::*;

    /// A macro generates methods *and* is a method call. Consuming it used to
    /// swallow the call site, so `--def` on `belongs_to` answered nothing —
    /// and a Rails class body is mostly macros.
    #[test]
    fn a_consumed_macro_is_still_recorded_as_a_call() {
        let facts = extract(
            b"class Widget < ApplicationRecord\n\
              \x20 belongs_to :supplier\n\
              \x20 attr_reader :name\n\
              \x20 delegate :region, to: :supplier\n\
              \x20 private\n\
              end\n",
        );
        let called: Vec<&str> = facts.calls.iter().map(|c| c.name.as_str()).collect();
        for macro_name in ["belongs_to", "attr_reader", "delegate", "private"] {
            assert!(
                called.contains(&macro_name),
                "{macro_name} is a call too: {called:?}"
            );
        }
        // And still generates what it implies — the point of consuming it.
        let defined: Vec<&str> = facts.defs.iter().map(|d| d.name.as_str()).collect();
        assert!(defined.contains(&"supplier") && defined.contains(&"name"));
    }

    /// The handlers that already recorded their own call must not now record
    /// it twice — a doubled call site would double every `--refs` count.
    #[test]
    fn a_mixin_or_concern_is_recorded_exactly_once() {
        let facts =
            extract(b"class Widget\n  include Trackable\n  concerning :Audit do\n  end\nend\n");
        for name in ["include", "concerning"] {
            assert_eq!(
                facts.calls.iter().filter(|c| c.name == name).count(),
                1,
                "{name} recorded once"
            );
        }
    }

    fn marks(facts: &Facts) -> Vec<&str> {
        facts
            .ancestry
            .iter()
            .filter(|edge| edge.relation == Relation::Dynamic)
            .map(|edge| edge.target.as_str())
            .collect()
    }

    /// A loop that would write out more methods than a string may make is
    /// marked instead, saying how many (DEC-164).
    #[test]
    fn a_string_that_would_make_too_many_methods_is_marked() {
        let names: Vec<String> = (0..50).map(|i| format!(":n{i}")).collect();
        let defs: String = (0..50).map(|i| format!("def #{{n}}_{i}; end\n")).collect();
        let source = format!(
            "class C\n  [{}].each do |n|\n    class_eval <<~RUBY\n{defs}    RUBY\n  end\nend\n",
            names.join(", ")
        );
        let facts = extract(source.as_bytes());
        assert!(facts.defs.iter().all(|d| d.kind != Kind::Method));
        assert!(
            marks(&facts)
                .iter()
                .any(|m| m.starts_with("class_eval of 2500 methods, too many to read"))
        );
    }

    /// A local built from a loop's variable names each value too, and a
    /// marker says the side and the shape it can make (DEC-160).
    #[test]
    fn a_marker_says_what_it_can_make() {
        let derived = extract(
            b"class C\n  [:a].each do |m|\n    n = \"x_#{m}\"\n    define_method(\"#{n}=\") {}\n  end\nend\n",
        );
        assert!(derived.defs.iter().any(|d| d.name == "x_a="));
        assert!(marks(&derived).is_empty());
        let shaped = extract(
            b"class C\n  def self.a(k)\n    define_singleton_method(\"#{k}_x\") {}\n  end\nend\n",
        );
        assert_eq!(marks(&shaped), ["define_singleton_method|singleton|*_x"]);
        let handed = extract(
            b"module M\n  def flags(*names)\n    names.each { |n| class_eval \"def #{n}?; end\" }\n  end\nend\n",
        );
        assert_eq!(marks(&handed), ["class_eval|instance|{0*}?|flags"]);
        let body = extract(b"class C\n  flags :a, other, if: 1\n  def x; flags :b; end\nend\n");
        let calls: Vec<_> = body
            .body_calls
            .iter()
            .filter(|c| c.name == "flags")
            .map(|c| (c.name.as_str(), c.args.clone()))
            .collect();
        assert_eq!(calls, [("flags", vec![Some("a".to_string()), None])]);
        let sent = extract(b"class C\n  [:a].each { |m| Other.send(:define_method, m) {} }\nend\n");
        assert_eq!(marks(&sent), ["define_method|instance|a"]);
    }

    /// A `def` read from a string lands where its name is written in the
    /// file, not in the rendered string, whose values are longer or shorter
    /// than the `#{…}` they replace.
    #[test]
    fn a_def_in_a_class_eval_string_is_placed_in_the_file() {
        let facts = extract(
            b"class C\n  %w[get].each do |m|\n    class_eval <<~RUBY\n      def x_#{m}_#{m.upcase}; helper; end\n    RUBY\n  end\nend\n",
        );
        let def = facts.defs.iter().find(|d| d.name == "x_get_GET");
        assert_eq!(def.map(|d| (d.pos.line, d.pos.col)), Some((4, 11)));
        let helper = facts.calls.iter().find(|c| c.name == "helper");
        // After two substitutions on the line, still the file's column.
        assert_eq!(helper.map(|c| (c.pos.line, c.pos.col)), Some((4, 31)));
        assert!(marks(&facts).is_empty());
    }

    /// Code the string does not let us render is not read, and says so; code
    /// that renders but names what no loop states keeps its calls.
    #[test]
    fn a_class_eval_string_that_cannot_be_read_marks_its_scope() {
        let computed = extract(b"class C\n  class_eval \"def #{name.to_s * 2}; end\"\nend\n");
        assert_eq!(marks(&computed), ["class_eval string|instance|"]);
        let broken = extract(b"class C\n  class_eval \"def x(\"\nend\n");
        assert_eq!(marks(&broken), ["class_eval string|instance|x"]);
        let nested = extract(
            b"class C\n  class_eval <<~RUBY\n    class_eval \"def inner; end\"\n  RUBY\nend\n",
        );
        assert_eq!(marks(&nested), ["class_eval string|instance|inner"]);
        let unstated = extract(
            b"class C\n  LIST.each do |m|\n    module_eval \"def #{m}; go(:#{m}); end\"\n  end\nend\n",
        );
        assert_eq!(marks(&unstated), ["module_eval|instance|"]);
        assert!(unstated.defs.iter().all(|d| d.kind != Kind::Method));
        let calls: Vec<&str> = unstated.calls.iter().map(|c| c.name.as_str()).collect();
        assert!(calls.contains(&"go"), "{calls:?}");
        assert!(calls.iter().all(|c| !c.contains(UNSTATED)));
    }
}
