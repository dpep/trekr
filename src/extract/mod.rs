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

mod line_index;
mod macros;

pub(crate) use macros::table_to_class;
mod sig;

use crate::core::*;
use line_index::LineIndex;
use ruby_prism::{Node, Visit};
use std::collections::HashMap;

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
    /// `module_function` seen with no arguments: every later `def` in this body
    /// becomes both a private instance method and a public singleton one.
    module_function: bool,
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
            module_function: false,
        }
    }
}

struct Extractor<'a> {
    src: &'a [u8],
    lines: LineIndex,
    nesting: Vec<String>,
    frames: Vec<Frame>,
    /// Return type from a Sorbet `sig` in the immediately preceding statement.
    pending_sig: Option<String>,
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
    facts: Facts,
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
            module_function: false,
        }],
        pending_sig: None,
        pending_sig_params: Vec::new(),
        symbol_arrays: HashMap::new(),
    };
    ex.visit(&parsed.node());
    ex.facts
}

impl<'a> Extractor<'a> {
    fn pos(&self, offset: usize) -> Pos {
        self.lines.pos(offset)
    }

    fn text(&self, start: usize, end: usize) -> String {
        String::from_utf8_lossy(&self.src[start..end.min(self.src.len())]).into_owned()
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
    }

    /// What `self` is for a call written here.
    fn self_is_class(&self) -> bool {
        self.frames.last().is_some_and(|f| f.self_is_class)
    }

    fn leave(&mut self) {
        if let Some(frame) = self.frames.pop()
            && frame.pushed
        {
            self.nesting.remove(0);
        }
    }

    fn push_def(&mut self, def: Def) {
        self.facts.defs.push(def);
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
            sig_params: Vec::new(),
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
fn has_keyword(args: &[Node<'_>], key: &str) -> bool {
    keyword_value(args, key).is_some()
}

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
            self.pending_sig = previous.and_then(sig::returns);
            self.pending_sig_params = previous.map(sig::params).unwrap_or_default();
            self.visit(stmt);
        }
        self.pending_sig = None;
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

        let mut def = self.def(name, Kind::Method, name_start, loc.end_offset());
        def.singleton = singleton;
        def.params = params_of(node.parameters());
        def.sig_returns = self.pending_sig.take();
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

        // Descend for calls and constants in the body — but not through a
        // receiver we already recorded.
        self.enter(None, Opens::Method { singleton });
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
        if let Some(symbols) = literal_symbol_array(&value) {
            self.symbol_arrays.insert(name.clone(), symbols);
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
        if self.handle_macro(node) {
            return;
        }
        self.record_call(node);

        if let Some(receiver) = node.receiver() {
            self.visit(&receiver);
        }
        if let Some(args) = node.arguments() {
            self.visit_arguments_node(&args);
        }
        if let Some(block) = node.block() {
            self.visit(&block);
        }
    }

    fn visit_local_variable_write_node(&mut self, node: &ruby_prism::LocalVariableWriteNode<'pr>) {
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
        def.target = Some(target);
        self.push_def(def);
    }
}

fn method_name(call: &ruby_prism::CallNode<'_>) -> Option<String> {
    String::from_utf8(call.name().as_slice().to_vec()).ok()
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
            "include" | "prepend" | "extend" => self.handle_mixin(call, &name, &args),
            "alias_method" => self.handle_alias_method(call, &args),
            "enum" => self.handle_enum(call, &args),
            // Any macro the expansion table knows. The probe argument only
            // asks "is this a macro we model" — the real names come below.
            _ if !macros::generated(&name, "probe").is_empty() => {
                self.handle_dsl(call, &name, &args)
            }
            "private" | "protected" | "public" | "module_function" => {
                self.handle_visibility(call, &name, &args)
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
        let sig = self.pending_sig.take();
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

    fn handle_mixin(
        &mut self,
        call: &ruby_prism::CallNode<'pr>,
        macro_name: &str,
        args: &[Node<'pr>],
    ) -> bool {
        let Some(relation) = Relation::parse(macro_name) else {
            return false;
        };
        if args.is_empty() {
            return false;
        }
        let mut any = false;
        // `include A, B` inserts B first — Ruby applies multi-arg mixins
        // right to left, and the ancestor order the tree layer builds is
        // exactly this list's order.
        for arg in args.iter().rev() {
            let target = if arg.as_self_node().is_some() {
                // `extend self` — the idiomatic module-function alternative.
                Some("self".to_string())
            } else {
                const_name(arg)
            };
            let Some(target) = target else {
                continue;
            };
            let pos = self.pos(arg.location().start_offset());
            self.facts.ancestry.push(Ancestry {
                owner: self.nesting.clone(),
                relation,
                target,
                pos,
            });
            any = true;
        }
        if any {
            // The argument constants are real references too.
            for arg in args {
                self.visit(arg);
            }
            self.record_call(call);
        }
        any
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

    /// `enum status: { draft: 0, … }` — a predicate, a bang setter, and a scope
    /// per member.
    ///
    /// Both spellings: Rails 6's `enum status: {…}` puts the attribute in the
    /// options hash, Rails 7's `enum :status, {…}` makes it the first argument.
    /// Members come from a literal hash's keys or a literal array's elements;
    /// anything computed produces nothing.
    fn handle_enum(&mut self, call: &ruby_prism::CallNode<'pr>, args: &[Node<'pr>]) -> bool {
        let mut members: Vec<String> = Vec::new();
        let mut collect = |node: &Node<'pr>| {
            if let Some(hash) = node.as_hash_node() {
                for element in hash.elements().iter() {
                    if let Some(assoc) = element.as_assoc_node()
                        && let Some(name) = literal_name(&assoc.key())
                    {
                        members.push(name);
                    }
                }
            } else if let Some(array) = node.as_array_node() {
                members.extend(array.elements().iter().filter_map(|e| literal_name(&e)));
            }
        };

        for (index, arg) in args.iter().enumerate() {
            // Rails 6 spelling: the attribute name is a key in the trailing
            // hash and its value holds the members.
            if let Some(hash) = arg.as_keyword_hash_node() {
                for element in hash.elements().iter() {
                    let Some(assoc) = element.as_assoc_node() else {
                        continue;
                    };
                    // `prefix:`/`suffix:` rename every generated method, so
                    // refuse rather than produce wrong names.
                    let key = literal_name(&assoc.key()).unwrap_or_default();
                    if key == "prefix" || key == "suffix" {
                        return false;
                    }
                    if !matches!(key.as_str(), "default" | "validate" | "instance_methods") {
                        collect(&assoc.value());
                    }
                }
            } else if index > 0 {
                // Rails 7 spelling: members are a positional argument.
                collect(arg);
            }
        }
        if members.is_empty() {
            return false;
        }

        let loc = call.location();
        let (start, end) = (loc.start_offset(), loc.end_offset());
        let in_singleton = self.in_singleton();
        for member in members {
            for (name, singleton) in [
                (format!("{member}?"), false),
                (format!("{member}!"), false),
                (member.clone(), true),
            ] {
                let mut def = self.def(name, Kind::Method, start, end);
                def.via = Some("enum".into());
                def.singleton = singleton || in_singleton;
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
                // `t.references :author` is a foreign key plus the association.
                "references" | "belongs_to" => {
                    for arg in arg_nodes(&inner) {
                        if let Some(name) = literal_name(&arg) {
                            columns.push((format!("{name}_id"), macros::column_class("integer")));
                            columns.push((name, None));
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
            // Getter, setter, predicate. The dirty-tracking family
            // (`_changed?`, `_was`, `_before_last_save`, …) is deliberately out:
            // it is a dozen names per column for a fraction of the calls.
            for (name, writer) in [
                (column.clone(), false),
                (format!("{column}="), true),
                (format!("{column}?"), false),
            ] {
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
        // `delegate` without `to:` is not a delegation, and a `prefix:` rewrites
        // every generated name. Both refuse rather than guess: a wrong method
        // name is worse than an unmodelled one.
        if macro_name == "delegate"
            && (keyword_literal(args, "to").is_none() || has_keyword(args, "prefix"))
        {
            return false;
        }
        let class_name = keyword_literal(args, "class_name");

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

        for (literal, pos) in names {
            let associated = class_name
                .clone()
                .or_else(|| macros::associated_class(macro_name, &literal));

            for made in macros::generated(macro_name, &literal) {
                let mut def = self.def(made.name.clone(), Kind::Method, start, end);
                def.pos = pos;
                def.via = Some(macro_name.to_string());
                def.visibility = visibility;
                // `class << self` still governs which side these land on.
                def.singleton = made.singleton || in_singleton;
                if made.writer {
                    def.params = vec![Param {
                        kind: ParamKind::Req,
                        name: "value".into(),
                    }];
                }
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
            // The arguments are still constants in their own right.
            for arg in args {
                self.visit(arg);
            }
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
        def.target = Some(old);
        self.push_def(def);
        true
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
        let (recv, recv_text) = match call.receiver() {
            None => (RecvShape::Implicit, None),
            Some(r) => receiver_shape(&r),
        };
        let mut argc = Some(0u32);
        for arg in arg_nodes(call) {
            if arg.as_splat_node().is_some()
                || arg.as_forwarding_arguments_node().is_some()
                || arg.as_assoc_splat_node().is_some()
            {
                // A splat hides the real count; `None` says so rather than
                // reporting a number that is wrong.
                argc = None;
                break;
            }
            argc = argc.map(|n| n + 1);
        }
        let pos = self.pos(message.start_offset());
        // Not `in_singleton()`: that answers "is a `def` here a singleton
        // method", which is a different question. A bare call in a class body
        // dispatches on the class even though a `def` there does not.
        let singleton = self.self_is_class();
        self.facts.calls.push(Call {
            name,
            recv,
            recv_text,
            nesting: self.nesting.clone(),
            singleton,
            argc,
            block: call.block().is_some(),
            pos,
        });
    }
}

/// Methods that hand back their receiver unchanged, so the type survives them.
/// From rwr's D61 measurement; `then` and `presence` are deliberately absent
/// because they do not preserve the type.
const IDENTITY: [&str; 5] = ["freeze", "dup", "clone", "itself", "tap"];

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
    if !IDENTITY.contains(&method_name(&call)?.as_str()) {
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
            if IDENTITY.contains(&name.as_str()) {
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
