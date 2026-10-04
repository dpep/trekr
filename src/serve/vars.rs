//! Variables in one file: which assignments a local's read can see, and each
//! instance or class variable with the class it belongs to.
//!
//! Pure — bytes in, occurrences out — so the scope rules are unit-tested
//! without a session. Locals are answered here entirely: Prism already decided
//! which identifiers are locals and how many block scopes up each one lives
//! (`depth`), so what is left is flow: an assignment on each branch of an `if`
//! both reach the read after it; a loop body sees its own later writes; `def`
//! sees nothing outside. Instance variables span files, so this only records
//! each one with its written nesting and whether `self` is the class or an
//! instance there; which files make up the class is the session's question.

use ruby_prism::{Node, Visit};
use std::collections::HashMap;
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sigil {
    Local,
    /// `@x`
    Instance,
    /// `@@x`
    Class,
}

/// How a write gave the variable its value — what a hover says about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Binding {
    Assign,
    /// `x += 1`, `x ||= 1`: a read and a write at once.
    OpAssign,
    Param,
    BlockParam,
    /// `in {x:}`, `=> x`, a regexp's named capture.
    Pattern,
    Rescue,
    For,
    /// `attr_writer`/`attr_accessor`: the setter it defines writes the ivar.
    Attr,
    /// `instance_variable_set(:@x, …)`
    Set,
}

impl Binding {
    pub(crate) fn describe(self) -> &'static str {
        match self {
            Binding::Assign | Binding::OpAssign => "assigned",
            Binding::Param => "parameter",
            Binding::BlockParam => "block parameter",
            Binding::Pattern => "bound by a pattern",
            Binding::Rescue => "bound by `rescue`",
            Binding::For => "bound by `for`",
            Binding::Attr => "set by an attribute writer",
            Binding::Set => "set by `instance_variable_set`",
        }
    }
}

/// One mention of a variable.
#[derive(Clone, Debug)]
pub(crate) struct Occurrence {
    pub(crate) sigil: Sigil,
    /// As written, sigil included: `total`, `@thing`.
    pub(crate) name: String,
    /// Bytes of the name.
    pub(crate) span: Range<usize>,
    pub(crate) read: bool,
    /// How it was written, when this is a write.
    pub(crate) write: Option<Binding>,
    /// Two occurrences with the same `var` are the same variable.
    pub(crate) var: u32,
    /// A local's read: the writes whose value it may see, in source order.
    pub(crate) reaches: Vec<u32>,
    /// An ivar or cvar: index into [`Vars::owners`].
    pub(crate) owner: Option<u32>,
    /// The method it is written in, if any.
    pub(crate) method: Option<String>,
    /// False for an `attr_accessor` symbol: the cursor there is on a method,
    /// not a variable.
    pub(crate) target: bool,
}

impl Occurrence {
    pub(crate) fn is_write(&self) -> bool {
        self.write.is_some()
    }
}

/// Where an ivar lives: the written nesting (innermost first, as facts carry
/// it) and whether `self` there is the class rather than an instance.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Owner {
    pub(crate) nesting: Vec<String>,
    pub(crate) singleton: bool,
}

#[derive(Debug, Default)]
pub(crate) struct Vars {
    pub(crate) occurrences: Vec<Occurrence>,
    pub(crate) owners: Vec<Owner>,
}

impl Vars {
    /// The occurrence under a byte offset; the end is inclusive, so a cursor
    /// just after the name still finds it.
    pub(crate) fn at(&self, offset: usize) -> Option<&Occurrence> {
        self.occurrences
            .iter()
            .find(|o| o.target && o.span.start <= offset && offset <= o.span.end)
    }

    /// Every mention of the same variable in this file, in source order.
    pub(crate) fn same<'a>(&'a self, of: &Occurrence) -> Vec<&'a Occurrence> {
        let mut all: Vec<&Occurrence> = self
            .occurrences
            .iter()
            .filter(|o| o.var == of.var)
            .collect();
        all.sort_by_key(|o| o.span.start);
        all
    }

    /// Where a local got the value read at `of`: the writes that reach it, or
    /// the write itself when `of` only writes. Empty for a member variable,
    /// which is answered across files.
    pub(crate) fn local_definitions<'a>(&'a self, of: &'a Occurrence) -> Vec<&'a Occurrence> {
        if of.sigil != Sigil::Local {
            return Vec::new();
        }
        if !of.read {
            return vec![of];
        }
        let mut found: Vec<&Occurrence> = of
            .reaches
            .iter()
            .map(|&i| &self.occurrences[i as usize])
            .collect();
        found.sort_by_key(|o| o.span.start);
        found
    }

    pub(crate) fn owner(&self, of: &Occurrence) -> Option<&Owner> {
        of.owner.map(|i| &self.owners[i as usize])
    }

    /// Take in the locals of a string of code this file evaluates, each
    /// placed in the file by `place`; one `place` cannot put in the file — a
    /// name a value was substituted into — is left out, and so is an ivar,
    /// whose owner the string's own parse cannot say (DEC-167).
    pub(crate) fn absorb(
        &mut self,
        other: Vars,
        place: impl Fn(Range<usize>) -> Option<Range<usize>>,
    ) {
        let var_base = self
            .occurrences
            .iter()
            .map(|o| o.var + 1)
            .max()
            .unwrap_or(0);
        let mut kept: Vec<Option<u32>> = Vec::with_capacity(other.occurrences.len());
        let mut next = self.occurrences.len() as u32;
        let keep = |o: &Occurrence| o.sigil == Sigil::Local && place(o.span.clone()).is_some();
        for occurrence in &other.occurrences {
            match keep(occurrence).then_some(()) {
                Some(_) => {
                    kept.push(Some(next));
                    next += 1;
                }
                None => kept.push(None),
            }
        }
        for (occurrence, at) in other.occurrences.into_iter().zip(&kept) {
            if at.is_none() {
                continue;
            }
            let Some(span) = place(occurrence.span.clone()) else {
                continue;
            };
            self.occurrences.push(Occurrence {
                span,
                var: occurrence.var + var_base,
                reaches: occurrence
                    .reaches
                    .iter()
                    .filter_map(|&i| kept[i as usize])
                    .collect(),
                ..occurrence
            });
        }
    }
}

pub(crate) fn analyze(src: &[u8]) -> Vars {
    let parsed = ruby_prism::parse(src);
    let root = parsed.node();
    // Twice: a loop's first pass learns which writes its body holds, so the
    // second can let a read at the top of the body see a write at its bottom
    // from the iteration before.
    let mut first = Walker::new(src, Prior::default());
    first.visit(&root);
    let prior = Prior {
        writes: first
            .occurrences
            .iter()
            .map(|o| {
                (o.sigil == Sigil::Local && o.is_write()).then(|| (o.var, first.var_scope[&o.var]))
            })
            .collect(),
        loops: first.loops,
    };
    let mut second = Walker::new(src, prior);
    second.visit(&root);
    Vars {
        occurrences: second.occurrences,
        owners: second.owners,
    }
}

/// The writes that may have set each local right now: var → occurrence
/// indices, sorted.
type State = HashMap<u32, Vec<u32>>;

fn merge(into: &mut State, from: State) {
    for (var, writes) in from {
        reach(into, var, writes);
    }
}

/// These writes may also have set `var`.
fn reach(state: &mut State, var: u32, writes: impl IntoIterator<Item = u32>) {
    let slot = state.entry(var).or_default();
    slot.extend(writes);
    slot.sort_unstable();
    slot.dedup();
}

/// What the first pass learned. Both passes number occurrences, vars and
/// scopes in the same order, so its indices hold in the second.
#[derive(Default)]
struct Prior {
    /// Loop bodies' occurrence ranges, in visiting order.
    loops: Vec<Range<usize>>,
    /// Each occurrence's var and its scope, when it writes a local.
    writes: Vec<Option<(u32, u32)>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Scope {
    Local(u32),
    Member(u32),
}

struct Walker<'s> {
    src: &'s [u8],
    occurrences: Vec<Occurrence>,
    owners: Vec<Owner>,
    owner_ids: HashMap<Owner, u32>,
    vars: HashMap<(Scope, String), u32>,
    /// A local var's scope: a block's own locals end with it.
    var_scope: HashMap<u32, u32>,
    scopes: Vec<u32>,
    next_scope: u32,
    state: State,
    prior: Prior,
    loops: Vec<Range<usize>>,
    loop_seq: usize,
    /// Each block body's occurrences, in the order the blocks close.
    blocks: Vec<Range<usize>>,
    nesting: Vec<String>,
    /// Inside `class << self`.
    eigen: bool,
    /// The enclosing method and whether it is a singleton one.
    method: Option<(String, bool)>,
    /// What a target or parameter written now is.
    binding: Binding,
}

impl<'s> Walker<'s> {
    fn new(src: &'s [u8], prior: Prior) -> Walker<'s> {
        Walker {
            src,
            occurrences: Vec::new(),
            owners: Vec::new(),
            owner_ids: HashMap::new(),
            vars: HashMap::new(),
            var_scope: HashMap::new(),
            scopes: Vec::new(),
            next_scope: 0,
            state: State::new(),
            prior,
            loops: Vec::new(),
            loop_seq: 0,
            blocks: Vec::new(),
            nesting: Vec::new(),
            eigen: false,
            method: None,
            binding: Binding::Assign,
        }
    }

    fn text(&self, range: Range<usize>) -> String {
        String::from_utf8_lossy(&self.src[range]).into_owned()
    }

    fn push_scope(&mut self) -> u32 {
        let id = self.next_scope;
        self.next_scope += 1;
        self.scopes.push(id);
        id
    }

    fn local_var(&mut self, name: &str, depth: u32) -> u32 {
        let at = self.scopes.len().saturating_sub(1 + depth as usize);
        let scope = self.scopes.get(at).copied().unwrap_or(0);
        let next = self.vars.len() as u32;
        let var = *self
            .vars
            .entry((Scope::Local(scope), name.to_string()))
            .or_insert(next);
        self.var_scope.insert(var, scope);
        var
    }

    fn owner_id(&mut self, singleton: bool) -> u32 {
        let owner = Owner {
            nesting: self.nesting.clone(),
            singleton,
        };
        let next = self.owners.len() as u32;
        *self.owner_ids.entry(owner.clone()).or_insert_with(|| {
            self.owners.push(owner);
            next
        })
    }

    /// Whether `self` is the class where an ivar is written now: in a class
    /// body, a `def self.x`, or anything inside `class << self`.
    fn self_is_class(&self) -> bool {
        self.method.as_ref().is_none_or(|(_, singleton)| *singleton)
    }

    fn push(&mut self, occurrence: Occurrence) -> u32 {
        self.occurrences.push(occurrence);
        (self.occurrences.len() - 1) as u32
    }

    fn local(&mut self, name: &[u8], depth: u32, start: usize, read: bool, write: Option<Binding>) {
        let name = String::from_utf8_lossy(name).into_owned();
        let var = self.local_var(&name, depth);
        let reaches = match read {
            true => self.state.get(&var).cloned().unwrap_or_default(),
            false => Vec::new(),
        };
        let span = start..start + name.len();
        let index = self.push(Occurrence {
            sigil: Sigil::Local,
            name,
            span,
            read,
            write,
            var,
            reaches,
            owner: None,
            method: self.method.as_ref().map(|(m, _)| m.clone()),
            target: true,
        });
        if write.is_some() {
            self.state.insert(var, vec![index]);
        }
    }

    /// `visit = lambda { … visit.call … }`: a block in the value runs only
    /// once the assignment is done, so its reads of the name see that write.
    /// A read outside a block (`x = x + 1`) does not.
    fn reaches_its_own_blocks(&mut self, from: usize, blocks_from: usize) {
        let write = self.occurrences.len() - 1;
        let var = self.occurrences[write].var;
        for block in &self.blocks[blocks_from..] {
            for read in &mut self.occurrences[block.start.max(from)..block.end] {
                if read.read && read.var == var && !read.reaches.contains(&(write as u32)) {
                    read.reaches.push(write as u32);
                }
            }
        }
    }

    /// `x ||= v` may leave the old value; `x += v` never does.
    fn op_assign(&mut self, name: &[u8], depth: u32, start: usize, keeps_old: bool) {
        let var = self.local_var(&String::from_utf8_lossy(name), depth);
        let before = self.state.get(&var).cloned().unwrap_or_default();
        self.local(name, depth, start, true, Some(Binding::OpAssign));
        if keeps_old {
            reach(&mut self.state, var, before);
        }
    }

    /// A mention of `@x` or `@@x`, as a read the caller amends.
    fn member(
        &mut self,
        sigil: Sigil,
        name: &[u8],
        start: usize,
        singleton: bool,
    ) -> &mut Occurrence {
        let name = String::from_utf8_lossy(name).into_owned();
        // A class variable is shared by the class and its instances alike.
        let owner = self.owner_id(sigil == Sigil::Instance && singleton);
        let next = self.vars.len() as u32;
        let var = *self
            .vars
            .entry((Scope::Member(owner), name.clone()))
            .or_insert(next);
        let span = start..start + name.len();
        let index = self.push(Occurrence {
            sigil,
            name,
            span,
            read: true,
            write: None,
            var,
            reaches: Vec::new(),
            owner: Some(owner),
            method: self.method.as_ref().map(|(m, _)| m.clone()),
            target: true,
        });
        &mut self.occurrences[index as usize]
    }

    fn ivar(&mut self, name: &[u8], start: usize, read: bool, write: Option<Binding>) {
        let singleton = self.self_is_class();
        let sigil = match name.starts_with(b"@@") {
            true => Sigil::Class,
            false => Sigil::Instance,
        };
        let occurrence = self.member(sigil, name, start, singleton);
        occurrence.read = read;
        occurrence.write = write;
    }

    fn with_binding(&mut self, binding: Binding, f: impl FnOnce(&mut Self)) {
        let saved = std::mem::replace(&mut self.binding, binding);
        f(self);
        self.binding = saved;
    }

    /// Runs a branch from `start` and returns where it leaves the locals.
    fn branch(&mut self, start: &State, f: impl FnOnce(&mut Self)) -> State {
        self.state = start.clone();
        f(self);
        std::mem::take(&mut self.state)
    }

    /// A body that may run any number of times: on entry it sees its own
    /// writes from the iteration before, and after it the locals are as they
    /// were or as it left them. `own` is the scope a block opens, whose
    /// locals start fresh each call.
    fn repeated(&mut self, own: Option<u32>, f: impl FnOnce(&mut Self)) {
        let seq = self.loop_seq;
        self.loop_seq += 1;
        self.loops.push(0..0);
        let before = self.state.clone();
        if let Some(range) = self.prior.loops.get(seq).cloned() {
            for index in range {
                let Some(Some((var, scope))) = self.prior.writes.get(index).copied() else {
                    continue;
                };
                // Only what outlives an iteration: a local of an enclosing
                // scope, not the block's own or a nested block's.
                if Some(scope) == own || !self.scopes.contains(&scope) {
                    continue;
                }
                reach(&mut self.state, var, [index as u32]);
            }
        }
        let start = self.occurrences.len();
        f(self);
        self.loops[seq] = start..self.occurrences.len();
        merge(&mut self.state, before);
    }

    /// The local writes made so far since occurrence `from`, merged in: what
    /// a `rescue` may see, since the body can stop after any of them.
    fn written_since(&mut self, from: usize) {
        for index in from..self.occurrences.len() {
            let o = &self.occurrences[index];
            if o.sigil == Sigil::Local && o.is_write() {
                reach(&mut self.state, o.var, [index as u32]);
            }
        }
    }

    /// A body of its own: a method, class or module forgets the caller's
    /// locals, and gives back none of its own.
    fn fresh(&mut self, f: impl FnOnce(&mut Self)) {
        let saved = std::mem::take(&mut self.state);
        self.push_scope();
        f(self);
        self.scopes.pop();
        self.state = saved;
    }

    fn visit_opt(&mut self, node: Option<Node<'_>>) {
        if let Some(node) = node {
            self.visit(&node);
        }
    }

    fn params(&mut self, node: Option<Node<'_>>, binding: Binding) {
        self.with_binding(binding, |w| w.visit_opt(node));
    }
}

fn union(states: Vec<State>) -> State {
    let mut out = State::new();
    for state in states {
        merge(&mut out, state);
    }
    out
}

/// A symbol or string argument's text and where it starts.
fn literal(node: &Node<'_>) -> Option<(Vec<u8>, usize)> {
    if let Some(symbol) = node.as_symbol_node() {
        let at = symbol
            .value_loc()
            .map_or(node.location().start_offset(), |l| l.start_offset());
        return Some((symbol.unescaped().to_vec(), at));
    }
    let string = node.as_string_node()?;
    Some((
        string.unescaped().to_vec(),
        string.content_loc().start_offset(),
    ))
}

impl<'pr> Visit<'pr> for Walker<'_> {
    fn visit_program_node(&mut self, node: &ruby_prism::ProgramNode<'pr>) {
        self.push_scope();
        ruby_prism::visit_program_node(self, node);
        self.scopes.pop();
    }

    fn visit_class_node(&mut self, node: &ruby_prism::ClassNode<'pr>) {
        self.visit_opt(node.superclass());
        let path = node.constant_path().location();
        let name = self.text(path.start_offset()..path.end_offset());
        self.enter_namespace(Some(name), false, node.body());
    }

    fn visit_module_node(&mut self, node: &ruby_prism::ModuleNode<'pr>) {
        let path = node.constant_path().location();
        let name = self.text(path.start_offset()..path.end_offset());
        self.enter_namespace(Some(name), false, node.body());
    }

    fn visit_singleton_class_node(&mut self, node: &ruby_prism::SingletonClassNode<'pr>) {
        self.visit(&node.expression());
        self.enter_namespace(None, true, node.body());
    }

    fn visit_def_node(&mut self, node: &ruby_prism::DefNode<'pr>) {
        let name = String::from_utf8_lossy(node.name().as_slice()).into_owned();
        let singleton = node.receiver().is_some() || self.eigen;
        // `def zone.x` reads `zone` in the scope around the def.
        self.visit_opt(node.receiver());
        let saved = self.method.replace((name, singleton));
        self.fresh(|w| {
            w.params(node.parameters().map(|p| p.as_node()), Binding::Param);
            w.visit_opt(node.body());
        });
        self.method = saved;
    }

    fn visit_block_node(&mut self, node: &ruby_prism::BlockNode<'pr>) {
        self.block(node.parameters(), node.body());
    }

    fn visit_lambda_node(&mut self, node: &ruby_prism::LambdaNode<'pr>) {
        self.block(node.parameters(), node.body());
    }

    fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
        let name = node.name().as_slice();
        let implicit = node.receiver().is_none_or(|r| r.as_self_node().is_some());
        let args: Vec<Node<'pr>> = node
            .arguments()
            .map(|a| a.arguments().iter().collect())
            .unwrap_or_default();
        if implicit && self.method.is_none() && matches!(name, b"attr_writer" | b"attr_accessor") {
            for arg in &args {
                if let Some((attr, at)) = literal(arg) {
                    let ivar = [b"@".as_slice(), &attr].concat();
                    // The setter it defines runs on an instance — or on the
                    // class, inside `class << self`.
                    let singleton = self.eigen;
                    let occurrence = self.member(Sigil::Instance, &ivar, at, singleton);
                    occurrence.read = false;
                    occurrence.write = Some(Binding::Attr);
                    // The symbol names a method; the cursor there is not on
                    // the ivar. Its span is the attribute's name, sans `@`.
                    occurrence.target = false;
                    occurrence.span = at..at + attr.len();
                }
            }
        }
        if implicit
            && name == b"instance_variable_set"
            && let Some((ivar, at)) = args.first().and_then(literal)
            && ivar.starts_with(b"@")
            && !ivar.starts_with(b"@@")
        {
            // A symbol's span starts after the colon; the ivar's `@` is there.
            self.ivar(&ivar, at, false, Some(Binding::Set));
        }
        // `define_method`'s block is a method body: `self` is an instance.
        if name == b"define_method"
            && self.method.is_none()
            && let Some(block) = node.block().and_then(|b| b.as_block_node())
        {
            self.visit_opt(node.receiver());
            for arg in &args {
                self.visit(arg);
            }
            let method = args
                .first()
                .and_then(literal)
                .map(|(n, _)| String::from_utf8_lossy(&n).into_owned())
                .unwrap_or_else(|| "define_method".into());
            let saved = self.method.replace((method, self.eigen));
            self.visit_block_node(&block);
            self.method = saved;
            return;
        }
        ruby_prism::visit_call_node(self, node);
    }

    // --- locals ---

    fn visit_local_variable_read_node(&mut self, node: &ruby_prism::LocalVariableReadNode<'pr>) {
        let at = node.location().start_offset();
        self.local(node.name().as_slice(), node.depth(), at, true, None);
    }

    fn visit_local_variable_write_node(&mut self, node: &ruby_prism::LocalVariableWriteNode<'pr>) {
        let from = self.occurrences.len();
        let blocks_from = self.blocks.len();
        self.visit(&node.value());
        let at = node.name_loc().start_offset();
        self.local(
            node.name().as_slice(),
            node.depth(),
            at,
            false,
            Some(Binding::Assign),
        );
        self.reaches_its_own_blocks(from, blocks_from);
    }

    fn visit_local_variable_target_node(
        &mut self,
        node: &ruby_prism::LocalVariableTargetNode<'pr>,
    ) {
        let at = node.location().start_offset();
        let binding = self.binding;
        self.local(
            node.name().as_slice(),
            node.depth(),
            at,
            false,
            Some(binding),
        );
    }

    fn visit_local_variable_or_write_node(
        &mut self,
        node: &ruby_prism::LocalVariableOrWriteNode<'pr>,
    ) {
        self.visit(&node.value());
        let at = node.name_loc().start_offset();
        self.op_assign(node.name().as_slice(), node.depth(), at, true);
    }

    fn visit_local_variable_and_write_node(
        &mut self,
        node: &ruby_prism::LocalVariableAndWriteNode<'pr>,
    ) {
        self.visit(&node.value());
        let at = node.name_loc().start_offset();
        self.op_assign(node.name().as_slice(), node.depth(), at, true);
    }

    fn visit_local_variable_operator_write_node(
        &mut self,
        node: &ruby_prism::LocalVariableOperatorWriteNode<'pr>,
    ) {
        self.visit(&node.value());
        let at = node.name_loc().start_offset();
        self.op_assign(node.name().as_slice(), node.depth(), at, false);
    }

    fn visit_multi_write_node(&mut self, node: &ruby_prism::MultiWriteNode<'pr>) {
        // The value is evaluated before anything is assigned.
        self.visit(&node.value());
        self.with_binding(Binding::Assign, |w| {
            for target in node.lefts().iter() {
                w.visit(&target);
            }
            w.visit_opt(node.rest());
            for target in node.rights().iter() {
                w.visit(&target);
            }
        });
    }

    fn visit_match_write_node(&mut self, node: &ruby_prism::MatchWriteNode<'pr>) {
        self.visit_call_node(&node.call());
        self.with_binding(Binding::Pattern, |w| {
            for target in node.targets().iter() {
                w.visit(&target);
            }
        });
    }

    fn visit_match_required_node(&mut self, node: &ruby_prism::MatchRequiredNode<'pr>) {
        self.visit(&node.value());
        self.with_binding(Binding::Pattern, |w| w.visit(&node.pattern()));
    }

    fn visit_match_predicate_node(&mut self, node: &ruby_prism::MatchPredicateNode<'pr>) {
        self.visit(&node.value());
        self.with_binding(Binding::Pattern, |w| w.visit(&node.pattern()));
    }

    // --- parameters: the binding says whether a method's or a block's ---

    fn visit_required_parameter_node(&mut self, node: &ruby_prism::RequiredParameterNode<'pr>) {
        let at = node.location().start_offset();
        let binding = self.binding;
        self.local(node.name().as_slice(), 0, at, false, Some(binding));
    }

    fn visit_optional_parameter_node(&mut self, node: &ruby_prism::OptionalParameterNode<'pr>) {
        self.visit(&node.value());
        let at = node.name_loc().start_offset();
        let binding = self.binding;
        self.local(node.name().as_slice(), 0, at, false, Some(binding));
    }

    fn visit_required_keyword_parameter_node(
        &mut self,
        node: &ruby_prism::RequiredKeywordParameterNode<'pr>,
    ) {
        let at = node.name_loc().start_offset();
        let binding = self.binding;
        self.local(node.name().as_slice(), 0, at, false, Some(binding));
    }

    fn visit_optional_keyword_parameter_node(
        &mut self,
        node: &ruby_prism::OptionalKeywordParameterNode<'pr>,
    ) {
        self.visit(&node.value());
        let at = node.name_loc().start_offset();
        let binding = self.binding;
        self.local(node.name().as_slice(), 0, at, false, Some(binding));
    }

    fn visit_rest_parameter_node(&mut self, node: &ruby_prism::RestParameterNode<'pr>) {
        if let (Some(name), Some(loc)) = (node.name(), node.name_loc()) {
            let binding = self.binding;
            self.local(name.as_slice(), 0, loc.start_offset(), false, Some(binding));
        }
    }

    fn visit_keyword_rest_parameter_node(
        &mut self,
        node: &ruby_prism::KeywordRestParameterNode<'pr>,
    ) {
        if let (Some(name), Some(loc)) = (node.name(), node.name_loc()) {
            let binding = self.binding;
            self.local(name.as_slice(), 0, loc.start_offset(), false, Some(binding));
        }
    }

    fn visit_block_parameter_node(&mut self, node: &ruby_prism::BlockParameterNode<'pr>) {
        if let (Some(name), Some(loc)) = (node.name(), node.name_loc()) {
            let binding = self.binding;
            self.local(name.as_slice(), 0, loc.start_offset(), false, Some(binding));
        }
    }

    fn visit_block_local_variable_node(&mut self, node: &ruby_prism::BlockLocalVariableNode<'pr>) {
        let at = node.location().start_offset();
        self.local(
            node.name().as_slice(),
            0,
            at,
            false,
            Some(Binding::BlockParam),
        );
    }

    // --- instance and class variables ---

    fn visit_instance_variable_read_node(
        &mut self,
        node: &ruby_prism::InstanceVariableReadNode<'pr>,
    ) {
        self.ivar(
            node.name().as_slice(),
            node.location().start_offset(),
            true,
            None,
        );
    }

    fn visit_instance_variable_write_node(
        &mut self,
        node: &ruby_prism::InstanceVariableWriteNode<'pr>,
    ) {
        self.visit(&node.value());
        let at = node.name_loc().start_offset();
        self.ivar(node.name().as_slice(), at, false, Some(Binding::Assign));
    }

    fn visit_instance_variable_or_write_node(
        &mut self,
        node: &ruby_prism::InstanceVariableOrWriteNode<'pr>,
    ) {
        self.visit(&node.value());
        let at = node.name_loc().start_offset();
        self.ivar(node.name().as_slice(), at, true, Some(Binding::OpAssign));
    }

    fn visit_instance_variable_and_write_node(
        &mut self,
        node: &ruby_prism::InstanceVariableAndWriteNode<'pr>,
    ) {
        self.visit(&node.value());
        let at = node.name_loc().start_offset();
        self.ivar(node.name().as_slice(), at, true, Some(Binding::OpAssign));
    }

    fn visit_instance_variable_operator_write_node(
        &mut self,
        node: &ruby_prism::InstanceVariableOperatorWriteNode<'pr>,
    ) {
        self.visit(&node.value());
        let at = node.name_loc().start_offset();
        self.ivar(node.name().as_slice(), at, true, Some(Binding::OpAssign));
    }

    fn visit_instance_variable_target_node(
        &mut self,
        node: &ruby_prism::InstanceVariableTargetNode<'pr>,
    ) {
        let at = node.location().start_offset();
        let binding = self.binding;
        self.ivar(node.name().as_slice(), at, false, Some(binding));
    }

    fn visit_class_variable_read_node(&mut self, node: &ruby_prism::ClassVariableReadNode<'pr>) {
        self.ivar(
            node.name().as_slice(),
            node.location().start_offset(),
            true,
            None,
        );
    }

    fn visit_class_variable_write_node(&mut self, node: &ruby_prism::ClassVariableWriteNode<'pr>) {
        self.visit(&node.value());
        let at = node.name_loc().start_offset();
        self.ivar(node.name().as_slice(), at, false, Some(Binding::Assign));
    }

    fn visit_class_variable_or_write_node(
        &mut self,
        node: &ruby_prism::ClassVariableOrWriteNode<'pr>,
    ) {
        self.visit(&node.value());
        let at = node.name_loc().start_offset();
        self.ivar(node.name().as_slice(), at, true, Some(Binding::OpAssign));
    }

    fn visit_class_variable_and_write_node(
        &mut self,
        node: &ruby_prism::ClassVariableAndWriteNode<'pr>,
    ) {
        self.visit(&node.value());
        let at = node.name_loc().start_offset();
        self.ivar(node.name().as_slice(), at, true, Some(Binding::OpAssign));
    }

    fn visit_class_variable_operator_write_node(
        &mut self,
        node: &ruby_prism::ClassVariableOperatorWriteNode<'pr>,
    ) {
        self.visit(&node.value());
        let at = node.name_loc().start_offset();
        self.ivar(node.name().as_slice(), at, true, Some(Binding::OpAssign));
    }

    fn visit_class_variable_target_node(
        &mut self,
        node: &ruby_prism::ClassVariableTargetNode<'pr>,
    ) {
        let at = node.location().start_offset();
        let binding = self.binding;
        self.ivar(node.name().as_slice(), at, false, Some(binding));
    }

    // --- control flow ---

    fn visit_if_node(&mut self, node: &ruby_prism::IfNode<'pr>) {
        self.visit(&node.predicate());
        let start = std::mem::take(&mut self.state);
        let then = self.branch(&start, |w| {
            w.visit_opt(node.statements().map(|s| s.as_node()))
        });
        let otherwise = self.branch(&start, |w| w.visit_opt(node.subsequent()));
        self.state = union(vec![then, otherwise]);
    }

    fn visit_unless_node(&mut self, node: &ruby_prism::UnlessNode<'pr>) {
        self.visit(&node.predicate());
        let start = std::mem::take(&mut self.state);
        let then = self.branch(&start, |w| {
            w.visit_opt(node.statements().map(|s| s.as_node()))
        });
        let otherwise = self.branch(&start, |w| {
            w.visit_opt(node.else_clause().map(|e| e.as_node()))
        });
        self.state = union(vec![then, otherwise]);
    }

    fn visit_and_node(&mut self, node: &ruby_prism::AndNode<'pr>) {
        self.visit(&node.left());
        let start = self.state.clone();
        self.visit(&node.right());
        merge(&mut self.state, start);
    }

    fn visit_or_node(&mut self, node: &ruby_prism::OrNode<'pr>) {
        self.visit(&node.left());
        let start = self.state.clone();
        self.visit(&node.right());
        merge(&mut self.state, start);
    }

    fn visit_case_node(&mut self, node: &ruby_prism::CaseNode<'pr>) {
        self.visit_opt(node.predicate());
        let start = std::mem::take(&mut self.state);
        let mut outs: Vec<State> = node
            .conditions()
            .iter()
            .map(|when| self.branch(&start, |w| w.visit(&when)))
            .collect();
        outs.push(self.branch(&start, |w| {
            w.visit_opt(node.else_clause().map(|e| e.as_node()))
        }));
        self.state = union(outs);
    }

    fn visit_case_match_node(&mut self, node: &ruby_prism::CaseMatchNode<'pr>) {
        self.visit_opt(node.predicate());
        let start = std::mem::take(&mut self.state);
        let mut outs: Vec<State> = node
            .conditions()
            .iter()
            .map(|branch| self.branch(&start, |w| w.visit(&branch)))
            .collect();
        outs.push(self.branch(&start, |w| {
            w.visit_opt(node.else_clause().map(|e| e.as_node()))
        }));
        self.state = union(outs);
    }

    fn visit_in_node(&mut self, node: &ruby_prism::InNode<'pr>) {
        self.with_binding(Binding::Pattern, |w| w.visit(&node.pattern()));
        self.visit_opt(node.statements().map(|s| s.as_node()));
    }

    fn visit_while_node(&mut self, node: &ruby_prism::WhileNode<'pr>) {
        self.repeated(None, |w| {
            w.visit(&node.predicate());
            w.visit_opt(node.statements().map(|s| s.as_node()));
        });
    }

    fn visit_until_node(&mut self, node: &ruby_prism::UntilNode<'pr>) {
        self.repeated(None, |w| {
            w.visit(&node.predicate());
            w.visit_opt(node.statements().map(|s| s.as_node()));
        });
    }

    fn visit_for_node(&mut self, node: &ruby_prism::ForNode<'pr>) {
        self.visit(&node.collection());
        self.repeated(None, |w| {
            w.with_binding(Binding::For, |w| w.visit(&node.index()));
            w.visit_opt(node.statements().map(|s| s.as_node()));
        });
    }

    fn visit_begin_node(&mut self, node: &ruby_prism::BeginNode<'pr>) {
        let before = self.state.clone();
        let from = self.occurrences.len();
        self.visit_opt(node.statements().map(|s| s.as_node()));
        let mut outs = Vec::new();
        if let Some(rescue) = node.rescue_clause() {
            let body = self.state.clone();
            // The body can stop after any of its writes, or before all.
            self.written_since(from);
            merge(&mut self.state, before);
            let entry = std::mem::take(&mut self.state);
            let mut clause = Some(rescue);
            while let Some(r) = clause {
                outs.push(self.branch(&entry, |w| w.rescue(&r)));
                clause = r.subsequent();
            }
            self.state = body;
        }
        self.visit_opt(node.else_clause().map(|e| e.as_node()));
        outs.push(std::mem::take(&mut self.state));
        self.state = union(outs);
        self.visit_opt(node.ensure_clause().map(|e| e.as_node()));
    }

    fn visit_rescue_modifier_node(&mut self, node: &ruby_prism::RescueModifierNode<'pr>) {
        let before = self.state.clone();
        let from = self.occurrences.len();
        self.visit(&node.expression());
        let body = self.state.clone();
        self.written_since(from);
        merge(&mut self.state, before);
        self.visit(&node.rescue_expression());
        merge(&mut self.state, body);
    }
}

impl Walker<'_> {
    fn enter_namespace(&mut self, name: Option<String>, eigen: bool, body: Option<Node<'_>>) {
        let pushed = name.is_some();
        if let Some(name) = name {
            self.nesting.insert(0, name);
        }
        let saved_eigen = std::mem::replace(&mut self.eigen, eigen);
        let saved_method = self.method.take();
        self.fresh(|w| w.visit_opt(body));
        self.method = saved_method;
        self.eigen = saved_eigen;
        if pushed {
            self.nesting.remove(0);
        }
    }

    fn block(&mut self, parameters: Option<Node<'_>>, body: Option<Node<'_>>) {
        let own = self.push_scope();
        let start = self.occurrences.len();
        self.repeated(Some(own), |w| {
            w.params(parameters, Binding::BlockParam);
            w.visit_opt(body);
        });
        self.blocks.push(start..self.occurrences.len());
        self.scopes.pop();
        // Its own locals are gone; carrying them on would make every later
        // branch copy them, and a spec file is thousands of blocks.
        let var_scope = &self.var_scope;
        self.state
            .retain(|var, _| var_scope.get(var).copied() != Some(own));
    }

    fn rescue(&mut self, node: &ruby_prism::RescueNode<'_>) {
        for exception in node.exceptions().iter() {
            self.visit(&exception);
        }
        if let Some(reference) = node.reference() {
            self.with_binding(Binding::Rescue, |w| w.visit(&reference));
        }
        self.visit_opt(node.statements().map(|s| s.as_node()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A local in a string of code is the file's where the string's bytes
    /// are: its read finds its write, and every mention is highlighted
    /// together, in file offsets (DEC-167). A name a value was substituted
    /// into is left out.
    #[test]
    fn a_string_of_codes_locals_are_the_files() {
        let src = "class W\n  [:a].each do |n|\n    class_eval <<~RUBY\n      def #{n}_x\n        total = 1\n        total + #{n}_y\n      end\n    RUBY\n  end\nend\n";
        let mut vars = analyze(src.as_bytes());
        for string in crate::extract::extract(src.as_bytes()).strings {
            vars.absorb(analyze(&string.src), |span| string.place(span));
        }
        let read = src.find("total +").unwrap();
        let write = src.find("total = 1").unwrap();
        let at = vars.at(read).expect("the read is a local");
        let found: Vec<usize> = vars
            .local_definitions(at)
            .iter()
            .map(|o| o.span.start)
            .collect();
        assert_eq!(found, [write]);
        assert_eq!(vars.same(at).len(), 2);
        assert!(
            vars.occurrences
                .iter()
                .all(|o| o.name != "trekr_unstated_name_y")
        );
    }

    /// The byte offset of the `nth` (0-based) whole-word `name` on a 1-based line.
    fn offset(src: &str, line: usize, name: &str, nth: usize) -> usize {
        let start: usize = src.lines().take(line - 1).map(|l| l.len() + 1).sum();
        let text = src.lines().nth(line - 1).expect("line exists");
        let word = |i: usize| {
            let before = text[..i].chars().next_back();
            let after = text[i + name.len()..].chars().next();
            let ident = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
            !ident(before) && !ident(after)
        };
        let at = text
            .match_indices(name)
            .map(|(i, _)| i)
            .filter(|&i| word(i))
            .nth(nth)
            .unwrap_or_else(|| panic!("no {name} #{nth} on line {line}"));
        start + at
    }

    fn line_of(src: &str, offset: usize) -> usize {
        src[..offset].matches('\n').count() + 1
    }

    /// The lines a local at `line` gets its value from.
    fn defined_at(src: &str, line: usize, name: &str, nth: usize) -> Vec<usize> {
        let vars = analyze(src.as_bytes());
        let at = vars
            .at(offset(src, line, name, nth))
            .unwrap_or_else(|| panic!("{name} on line {line} is a variable"));
        vars.local_definitions(at)
            .iter()
            .map(|o| line_of(src, o.span.start))
            .collect()
    }

    fn binding_at(src: &str, line: usize, name: &str) -> Option<Binding> {
        let vars = analyze(src.as_bytes());
        vars.at(offset(src, line, name, 0)).and_then(|o| o.write)
    }

    #[test]
    fn a_read_goes_to_the_latest_assignment_before_it() {
        let src = "x = 1\nx = 2\nputs x\n";
        assert_eq!(defined_at(src, 3, "x", 0), [2]);
    }

    #[test]
    fn a_read_on_the_right_of_its_own_assignment_sees_the_one_before() {
        let src = "x = 1\nx = x + 1\n";
        assert_eq!(defined_at(src, 2, "x", 1), [1]);
    }

    #[test]
    fn assignments_on_both_branches_both_reach() {
        let src = "if c\n  x = 1\nelse\n  x = 2\nend\nputs x\n";
        assert_eq!(defined_at(src, 6, "x", 0), [2, 4]);
    }

    #[test]
    fn a_branch_without_else_keeps_the_value_from_before() {
        let src = "x = 0\nx = 1 if c\nputs x\n";
        assert_eq!(defined_at(src, 3, "x", 0), [1, 2]);
    }

    #[test]
    fn case_branches_and_the_fallthrough_all_reach() {
        let src = "x = 0\ncase y\nwhen 1 then x = 1\nwhen 2 then x = 2\nend\nx\n";
        assert_eq!(defined_at(src, 6, "x", 0), [1, 3, 4]);
    }

    #[test]
    fn the_right_of_an_and_may_not_run() {
        let src = "x = 0\nc && (x = 1)\nx\n";
        assert_eq!(defined_at(src, 3, "x", 0), [1, 2]);
    }

    #[test]
    fn def_starts_a_new_scope() {
        let src = "x = 1\ndef run\n  x = 2\n  x\nend\n";
        assert_eq!(defined_at(src, 4, "x", 0), [3]);
    }

    #[test]
    fn a_name_never_assigned_in_the_method_is_not_a_variable() {
        let src = "x = 1\ndef run\n  x\nend\n";
        let vars = analyze(src.as_bytes());
        assert!(vars.at(offset(src, 3, "x", 0)).is_none());
    }

    #[test]
    fn a_block_sees_the_enclosing_locals() {
        let src = "total = 0\nitems.each do |i|\n  total += i\nend\nputs total\n";
        assert_eq!(defined_at(src, 3, "total", 0), [1, 3]);
        assert_eq!(defined_at(src, 5, "total", 0), [1, 3]);
    }

    #[test]
    fn a_block_parameter_shadows_the_outer_local() {
        let src = "x = 1\n[2].each { |x| puts x }\nputs x\n";
        assert_eq!(defined_at(src, 2, "x", 1), [2]);
        assert_eq!(defined_at(src, 3, "x", 0), [1]);
    }

    #[test]
    fn a_blocks_own_local_does_not_outlive_it() {
        let src = "[1].each { y = 1; y }\ny = 2\ny\n";
        assert_eq!(defined_at(src, 1, "y", 1), [1]);
        assert_eq!(defined_at(src, 3, "y", 0), [2]);
    }

    #[test]
    fn a_lambda_sees_the_assignment_that_holds_it() {
        let src = "visit = lambda do |n|\n  visit.call(n)\nend\nf = ->(n) { f.(n) }\n";
        assert_eq!(defined_at(src, 2, "visit", 0), [1]);
        assert_eq!(defined_at(src, 4, "f", 1), [4]);
        // Outside a block, the value is computed before the write.
        let src = "x = 1\nx = x + 1\n";
        assert_eq!(defined_at(src, 2, "x", 1), [1]);
    }

    #[test]
    fn a_loop_sees_its_own_write_from_the_iteration_before() {
        let src = "prev = nil\nitems.each do |i|\n  puts prev\n  prev = i\nend\n";
        assert_eq!(defined_at(src, 3, "prev", 0), [1, 4]);
    }

    #[test]
    fn a_while_loop_carries_its_writes_around() {
        let src = "n = 0\nwhile n < 3\n  n = n + 1\nend\nn\n";
        assert_eq!(defined_at(src, 2, "n", 0), [1, 3]);
        assert_eq!(defined_at(src, 5, "n", 0), [1, 3]);
    }

    #[test]
    fn method_parameters_of_every_shape_are_definitions() {
        let src = "def run(a, b = 1, *c, d:, e: 2, **f, &g)\n  [a, b, c, d, e, f, g]\nend\n";
        for (name, _) in [
            ("a", 0),
            ("b", 0),
            ("c", 0),
            ("d", 0),
            ("e", 0),
            ("f", 0),
            ("g", 0),
        ] {
            assert_eq!(defined_at(src, 2, name, 0), [1], "{name}");
        }
        assert_eq!(binding_at(src, 1, "d"), Some(Binding::Param));
    }

    #[test]
    fn block_parameters_destructure() {
        let src = "pairs.each do |a, (b, c); d|\n  [a, b, c, d]\nend\n";
        for name in ["a", "b", "c", "d"] {
            assert_eq!(defined_at(src, 2, name, 0), [1], "{name}");
        }
        assert_eq!(binding_at(src, 1, "b"), Some(Binding::BlockParam));
    }

    #[test]
    fn a_lambdas_parameter_is_its_own() {
        let src = "f = ->(x) { x * 2 }\n";
        assert_eq!(defined_at(src, 1, "x", 1), [1]);
    }

    #[test]
    fn pattern_bindings_are_definitions() {
        let src = "case h\nin {x:}\n  x\nin [y, *rest]\n  rest\nend\nh => {z:}\nz\n";
        assert_eq!(defined_at(src, 3, "x", 0), [2]);
        assert_eq!(defined_at(src, 5, "rest", 0), [4]);
        assert_eq!(defined_at(src, 8, "z", 0), [7]);
        assert_eq!(binding_at(src, 2, "x"), Some(Binding::Pattern));
    }

    #[test]
    fn rescue_for_and_multiple_assignment_bind() {
        let src = "a, b = pair\nfor i in list\n  i\nend\nbegin\nrescue => e\n  e\nend\n[a, b]\n";
        assert_eq!(defined_at(src, 3, "i", 0), [2]);
        assert_eq!(defined_at(src, 7, "e", 0), [6]);
        assert_eq!(defined_at(src, 9, "b", 0), [1]);
        assert_eq!(binding_at(src, 6, "e"), Some(Binding::Rescue));
        assert_eq!(binding_at(src, 2, "i"), Some(Binding::For));
    }

    #[test]
    fn a_named_capture_binds() {
        let src = "if /(?<year>\\d+)/ =~ s\n  year\nend\n";
        assert_eq!(defined_at(src, 2, "year", 0), [1]);
    }

    #[test]
    fn or_assign_may_keep_the_old_value_and_op_assign_does_not() {
        let src = "x = nil\nx ||= 1\nx\ny = 1\ny += 1\ny\n";
        assert_eq!(defined_at(src, 3, "x", 0), [1, 2]);
        assert_eq!(defined_at(src, 6, "y", 0), [5]);
        // The op-assign itself reads the value before it.
        assert_eq!(defined_at(src, 5, "y", 0), [4]);
    }

    #[test]
    fn rescue_sees_any_write_the_body_made_before_it_failed() {
        let src = "x = 1\nbegin\n  x = 2\n  work\n  x = 3\nrescue\n  x\nend\n";
        assert_eq!(defined_at(src, 7, "x", 0), [1, 3, 5]);
    }

    #[test]
    fn a_defs_receiver_reads_the_local_around_it() {
        let src = "clock = 1\nclock = 2\ndef clock.tick\n  clock = 3\nend\n";
        assert_eq!(defined_at(src, 3, "clock", 0), [2]);
    }

    #[test]
    fn a_plain_write_is_its_own_definition() {
        let src = "x = 1\n";
        assert_eq!(defined_at(src, 1, "x", 0), [1]);
    }

    #[test]
    fn every_mention_of_a_local_is_the_same_variable_and_no_other() {
        let src = "x = 1\n[1].each { |x| x }\nputs x\ndef m\n  x = 2\nend\n";
        let vars = analyze(src.as_bytes());
        let first = vars.at(offset(src, 1, "x", 0)).unwrap();
        let lines: Vec<usize> = vars
            .same(first)
            .iter()
            .map(|o| line_of(src, o.span.start))
            .collect();
        assert_eq!(lines, [1, 3]);
    }

    #[test]
    fn an_ivar_knows_its_class_and_whether_self_is_an_instance() {
        let src = concat!(
            "module Shop\n",        // 1
            "  class Widget\n",     // 2
            "    @count = 0\n",     // 3
            "    def initialize\n", // 4
            "      @name = 'w'\n",  // 5
            "    end\n",            // 6
            "    def self.count\n", // 7
            "      @count\n",       // 8
            "    end\n",            // 9
            "  end\n",              // 10
            "end\n",                // 11
        );
        let vars = analyze(src.as_bytes());
        let name = vars.at(offset(src, 5, "@name", 0)).unwrap();
        let owner = vars.owner(name).unwrap();
        assert_eq!(owner.nesting, ["Widget", "Shop"]);
        assert!(!owner.singleton);
        assert_eq!(name.method.as_deref(), Some("initialize"));
        // Class-level `@count`: the class body and `def self.count` share it.
        let read = vars.at(offset(src, 8, "@count", 0)).unwrap();
        let written = vars.at(offset(src, 3, "@count", 0)).unwrap();
        assert_eq!(read.var, written.var);
        assert!(vars.owner(read).unwrap().singleton);
    }

    #[test]
    fn an_instance_ivar_is_not_the_class_level_one_of_the_same_name() {
        let src = "class W\n  @x = 1\n  def x\n    @x\n  end\nend\n";
        let vars = analyze(src.as_bytes());
        let read = vars.at(offset(src, 4, "@x", 0)).unwrap();
        let class_level = vars.at(offset(src, 2, "@x", 0)).unwrap();
        assert_ne!(read.var, class_level.var);
    }

    #[test]
    fn attribute_writers_and_instance_variable_set_write_the_ivar() {
        let src = concat!(
            "class W\n",                                // 1
            "  attr_accessor :size, \"color\"\n",       // 2
            "  attr_reader :shape\n",                   // 3
            "  def reset\n",                            // 4
            "    instance_variable_set(:@weight, 0)\n", // 5
            "    [@size, @color, @shape, @weight]\n",   // 6
            "  end\n",                                  // 7
            "end\n",                                    // 8
        );
        let vars = analyze(src.as_bytes());
        let writes = |ivar: &str| -> Vec<(usize, Binding)> {
            let read = vars.at(offset(src, 6, ivar, 0)).unwrap();
            vars.same(read)
                .iter()
                .filter_map(|o| Some((line_of(src, o.span.start), o.write?)))
                .collect()
        };
        assert_eq!(writes("@size"), [(2, Binding::Attr)]);
        assert_eq!(writes("@color"), [(2, Binding::Attr)]);
        assert_eq!(writes("@shape"), [], "a reader writes nothing");
        assert_eq!(writes("@weight"), [(5, Binding::Set)]);
        // The accessor's symbol is a method, not a place to ask about the ivar.
        assert!(vars.at(offset(src, 2, "size", 0)).is_none());
    }

    #[test]
    fn a_define_method_body_is_an_instance_context() {
        let src = "class W\n  define_method(:go) do\n    @go = 1\n  end\n  def go?\n    @go\n  end\nend\n";
        let vars = analyze(src.as_bytes());
        let read = vars.at(offset(src, 6, "@go", 0)).unwrap();
        let write = vars.at(offset(src, 3, "@go", 0)).unwrap();
        assert_eq!(read.var, write.var);
        assert_eq!(write.method.as_deref(), Some("go"));
    }

    #[test]
    fn a_class_variable_is_shared_by_the_class_and_its_instances() {
        let src = "class W\n  @@n = 0\n  def bump\n    @@n += 1\n  end\nend\n";
        let vars = analyze(src.as_bytes());
        let body = vars.at(offset(src, 2, "@@n", 0)).unwrap();
        let method = vars.at(offset(src, 4, "@@n", 0)).unwrap();
        assert_eq!(body.var, method.var);
        assert_eq!(method.sigil, Sigil::Class);
    }

    #[test]
    fn a_syntax_error_still_answers_what_parsed() {
        let src = "x = 1\nputs x\ndef broken(\n";
        assert_eq!(defined_at(src, 2, "x", 0), [1]);
    }
}
