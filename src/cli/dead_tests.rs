//! A `test-only` method's tests (DEC-730): each site a test writes, with the
//! example and group around it, and the spec code that goes when the method
//! does — only where that code exercises nothing else of the checkout, by
//! what trekr resolves each of its calls to, and what it leaves unread.

use super::dead::{DeadKind, DeadRow, Detail, Grade, MethodDetail};
use super::dead_consts::in_tests;
use super::members as cli_members;
use crate::core::{Def, Facts, Pos, RecvShape, SentName};
use crate::extract::line_index::LineIndex;
use crate::query::members::CheckoutFiles;
use crate::resolve::members::{self, Asked, Context, Files, Named};
use crate::tree::Tree;
use serde::Serialize;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

/// A `test-only` method row's tests, and what goes with it.
#[derive(Serialize)]
pub(super) struct Tests {
    pub(super) test_sites: Vec<TestSite>,
    pub(super) delete_with: Vec<Unit>,
    pub(super) frees: Vec<Freed>,
}

/// One call, symbol or `super` of the method written in a test.
#[derive(Clone, Serialize)]
pub(super) struct TestSite {
    pub(super) path: String,
    pub(super) line: u32,
    pub(super) col: u32,
    /// The example it is written in: an `it`, `specify`, `test` or `test_`
    /// method. Absent in a hook, a `let`, a helper.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) example: Option<Block>,
    /// The innermost example group around it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) group: Option<Block>,
}

/// An example or a group, as written: its description and its lines.
#[derive(Clone, Serialize)]
pub(super) struct Block {
    pub(super) name: String,
    pub(super) line: u32,
    pub(super) end_line: u32,
}

/// Spec code to delete with the method: it holds a site, and nothing in it
/// reaches the checkout but the method itself.
#[derive(Clone, Serialize)]
pub(super) struct Unit {
    pub(super) kind: UnitKind,
    pub(super) path: String,
    pub(super) name: String,
    pub(super) line: u32,
    pub(super) end_line: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum UnitKind {
    Example,
    Group,
    /// The group is all the file holds, besides its `require`s.
    File,
}

/// A spec member every read of which is in the units: unread once they go.
#[derive(Clone, Serialize)]
pub(super) struct Freed {
    pub(super) kind: DeadKind,
    pub(super) name: String,
    pub(super) path: String,
    pub(super) line: u32,
}

/// Fills each `test-only` method row's `test_sites` blocks, `delete_with`
/// and `frees`. `test_sites` arrive with their positions only.
pub(super) fn plan(
    tree: &Tree,
    files: &CheckoutFiles<'_>,
    context: &Context<'_>,
    root: &str,
    rows: &mut [DeadRow],
) {
    let relative = |path: &str| -> String {
        path.strip_prefix(root)
            .map(|rest| rest.trim_start_matches('/').to_string())
            .unwrap_or_else(|| path.to_string())
    };
    let shapes: RefCell<HashMap<String, Option<Arc<Shape>>>> = RefCell::new(HashMap::new());
    for row in rows.iter_mut() {
        let said = &mut row.reason;
        let row_path = relative(&row.path);
        let row_line = row.line;
        // A row its own grading says may be called has no spec code to spare.
        let clear = row.confidence == Grade::Clear;
        let Detail::Method(MethodDetail {
            tests: Some(tests), ..
        }) = &mut row.detail
        else {
            continue;
        };
        let Tests {
            test_sites: sites,
            delete_with,
            frees,
        } = &mut **tests;
        let target = Target {
            owner: row.owner.clone(),
            name: row.name.clone(),
            path: row_path,
            line: row_line,
        };
        let mut weigh = Weigh {
            tree,
            files,
            root,
            target: &target,
            shapes: &shapes,
            verdicts: HashMap::new(),
        };
        // A test of a method that reaches live code tests that code too.
        let spare = clear && weigh.closed().is_ok();
        let mut units: Vec<Unit> = Vec::new();
        let written: Vec<(String, Pos)> = sites
            .iter()
            .map(|s| {
                let at = Pos {
                    line: s.line,
                    col: s.col,
                };
                (relative(&s.path), at)
            })
            .collect();
        for site in sites.iter_mut() {
            let path = relative(&site.path);
            let Some(shape) = weigh.shape(&path) else {
                continue;
            };
            let at = Pos {
                line: site.line,
                col: site.col,
            };
            let around: Vec<&Span> = shape.around(at);
            site.example = around
                .iter()
                .rev()
                .find(|s| s.kind == SpanKind::Example)
                .map(|s| s.block());
            site.group = around
                .iter()
                .rev()
                .find(|s| matches!(s.kind, SpanKind::Group | SpanKind::Shared))
                .map(|s| s.block());
            // A shared group's body runs in whichever group includes it.
            if !spare || around.iter().any(|s| s.kind == SpanKind::Shared) {
                continue;
            }
            // The outermost that holds nothing else: a group before the
            // example in it.
            let candidates = around
                .iter()
                .filter(|s| s.kind == SpanKind::Group)
                .chain(around.iter().rev().find(|s| s.kind == SpanKind::Example))
                // A unit is deleted by its lines: none another statement shares.
                .filter(|s| s.alone);
            let here: Vec<Pos> = written
                .iter()
                .filter(|(p, _)| *p == path)
                .map(|(_, at)| *at)
                .collect();
            for span in candidates {
                if weigh.unit(&path, &shape, span, &here) {
                    let kind = match span.kind {
                        SpanKind::Group if shape.whole_file(span) => UnitKind::File,
                        SpanKind::Group => UnitKind::Group,
                        _ => UnitKind::Example,
                    };
                    let unit = Unit {
                        kind,
                        path: site.path.clone(),
                        name: span.name.clone(),
                        line: span.start.line,
                        end_line: span.end.line,
                    };
                    if !units
                        .iter()
                        .any(|u| u.path == unit.path && u.line == unit.line)
                    {
                        units.push(unit);
                    }
                    break;
                }
            }
        }
        // A unit inside another goes with it.
        let held: Vec<Unit> = units
            .iter()
            .filter(|u| {
                !units.iter().any(|o| {
                    o.path == u.path
                        && (o.line, o.end_line) != (u.line, u.end_line)
                        && o.line <= u.line
                        && u.end_line <= o.end_line
                })
            })
            .cloned()
            .collect();
        let unread = freed(context, files, &relative, &held);
        said.push_str(&told(sites, &held, &unread));
        *frees = unread;
        *delete_with = held;
    }
}

/// What a row's reason adds: how many sites go with the method, in what.
fn told(sites: &[TestSite], units: &[Unit], unread: &[Freed]) -> String {
    if units.is_empty() {
        return String::new();
    }
    let covered = sites
        .iter()
        .filter(|s| {
            units
                .iter()
                .any(|u| u.path == s.path && (u.line..=u.end_line).contains(&s.line))
        })
        .count();
    let counted = |n: usize, one: &str, many: &str| match n {
        0 => None,
        1 => Some(format!("1 {one}")),
        n => Some(format!("{n} {many}")),
    };
    let of = |kind: UnitKind| units.iter().filter(|u| u.kind == kind).count();
    let what: Vec<String> = [
        counted(of(UnitKind::File), "spec file", "spec files"),
        counted(of(UnitKind::Group), "group", "groups"),
        counted(of(UnitKind::Example), "example", "examples"),
    ]
    .into_iter()
    .flatten()
    .collect();
    let all = match covered == sites.len() {
        true => "all".to_string(),
        false => format!("{covered} of them"),
    };
    let left = match unread.len() {
        0 => String::new(),
        1 => ", and leaves 1 spec member unread".to_string(),
        n => format!(", and leaves {n} spec members unread"),
    };
    format!(
        "; {all} in spec code that exercises nothing else, which goes with it ({}){left}",
        what.join(", ")
    )
}

/// The members of the units' files read only from inside the units: unread
/// once they go. One step, as DEC-680's cascade: a member only those read
/// is found by running `--dead` again.
fn freed(
    context: &Context<'_>,
    files: &CheckoutFiles<'_>,
    relative: &dyn Fn(&str) -> String,
    units: &[Unit],
) -> Vec<Freed> {
    let mut paths: Vec<String> = units.iter().map(|u| relative(&u.path)).collect();
    paths.sort();
    paths.dedup();
    let inside = |path: &str, line: u32| {
        units
            .iter()
            .any(|u| relative(&u.path) == path && (u.line..=u.end_line).contains(&line))
    };
    let mut out = Vec::new();
    for path in paths {
        let Some(facts) = files.facts(&path) else {
            continue;
        };
        for def in &facts.defs {
            if !members::is_member(def)
                || cli_members::eager(def)
                || members::names_a_named_subject(def, &facts)
                || inside(&path, def.pos.line)
            {
                continue;
            }
            let asked = Asked { path: &path, def };
            let reads = members::reads(context, &asked, false);
            // A name sent at runtime may read it unseen.
            if !reads.caveats.is_empty() || reads.found.is_empty() {
                continue;
            }
            if reads.found.iter().all(|r| inside(&r.path, r.line)) {
                out.push(Freed {
                    kind: cli_members::kind(def),
                    name: def.name.clone(),
                    path: units
                        .iter()
                        .find(|u| relative(&u.path) == path)
                        .map_or(path.clone(), |u| u.path.clone()),
                    line: def.pos.line,
                });
            }
        }
    }
    out
}

/// Where a unit runs. A `let`, hook or helper outside it runs on the
/// unit's examples, so a member it reads is the one the unit sees, not the
/// one its own text sees: a nested group may redefine it.
struct From {
    path: String,
    start: Pos,
    end: Pos,
    /// The nesting of the group the unit is written in; `None` for a
    /// Minitest method, which reads no member.
    nesting: Option<Vec<String>>,
}

impl From {
    fn holds(&self, path: &str, at: Pos) -> bool {
        self.path == path && self.start <= at && at <= self.end
    }
}

/// The method a row is.
struct Target {
    owner: String,
    name: String,
    path: String,
    line: u32,
}

/// Whether spec code reaches the checkout only through the target.
struct Weigh<'a, 'f> {
    tree: &'a Tree,
    files: &'a CheckoutFiles<'f>,
    root: &'a str,
    target: &'a Target,
    shapes: &'a RefCell<HashMap<String, Option<Arc<Shape>>>>,
    verdicts: HashMap<(String, Pos), bool>,
}

/// What Ruby and RSpec call on an object without the code naming the
/// call: `eq` compares with `==`, a Hash key hashes, interpolation and
/// failure messages render, `sort` compares, a splat converts. A class
/// defining one in the checkout runs live code a spec never writes.
const PROTOCOL: [&str; 22] = [
    "==",
    "eql?",
    "equal?",
    "===",
    "hash",
    "<=>",
    "=~",
    "each",
    "to_s",
    "to_str",
    "to_a",
    "to_ary",
    "to_h",
    "to_hash",
    "to_proc",
    "to_i",
    "to_int",
    "coerce",
    "inspect",
    "method_missing",
    "respond_to_missing?",
    "respond_to?",
];

/// How many bodies one unit's weighing may follow before it gives up and
/// says no: a helper chain that long is no unit to delete on trust.
const MAX_BODIES: usize = 64;

impl Weigh<'_, '_> {
    /// Whether the span is spec code to delete with the target: it, the
    /// hooks that run before its examples, and every body they reach
    /// through test code touch nothing else of the checkout; and, for a
    /// group, every example in it reaches the target.
    fn unit(&mut self, path: &str, shape: &Shape, span: &Span, here: &[Pos]) -> bool {
        let key = (path.to_string(), span.start);
        if let Some(known) = self.verdicts.get(&key) {
            return *known;
        }
        let verdict = self.judge(path, shape, span, here).is_ok();
        self.verdicts.insert(key, verdict);
        verdict
    }

    fn judge(&self, path: &str, shape: &Shape, span: &Span, here: &[Pos]) -> Result<(), String> {
        let facts = self
            .files
            .facts(path)
            .ok_or_else(|| format!("unread {path}"))?;
        // A shared context included around it adds hooks and `let`s unseen.
        if let Some(call) = facts.calls.iter().find(|c| {
            INCLUDES_INTO.contains(&c.name.as_str())
                && !span.holds(c.pos)
                && shape.innermost_group(c.pos).is_some_and(|g| g.covers(span))
        }) {
            return Err(format!("{} at line {}", call.name, call.pos.line));
        }
        // The call that opens it carries the nesting of the group it is in.
        let nesting = match span.kind {
            SpanKind::Example if span.name.starts_with("test_") && span.call.is_empty() => None,
            _ => Some(
                facts
                    .calls
                    .iter()
                    .find(|c| {
                        c.name == span.call && c.pos.line == span.start.line && span.holds(c.pos)
                    })
                    .map(|c| c.nesting.clone())
                    .ok_or_else(|| format!("no call opens line {}", span.start.line))?,
            ),
        };
        let from = From {
            path: path.to_string(),
            start: span.start,
            end: span.end,
            nesting,
        };
        if let Some(hook) = shape
            .hooks_before(span)
            .find(|h| h.kind == SpanKind::Unseen)
        {
            return Err(format!(
                "setup names a method its class does not define, at line {}",
                hook.start.line
            ));
        }
        // What an example asserts may be what a hook did before it.
        let mut spans = vec![(path.to_string(), span.start, span.end)];
        spans.extend(
            shape
                .hooks_before(span)
                .map(|h| (path.to_string(), h.start, h.end)),
        );
        // A Minitest test runs the setup its class inherits too.
        if span.kind == SpanKind::Example && (span.call.is_empty() || span.call == "test") {
            spans.extend(self.inherited_setup(path, shape, span)?);
        }
        self.weigh(spans, &from)?;
        if span.kind == SpanKind::Group {
            for example in shape.examples_in(span) {
                let calls_it = here.iter().any(|at| example.holds(*at))
                    || self.weigh(vec![(path.to_string(), example.start, example.end)], &from)?;
                if !calls_it {
                    return Err(format!(
                        "the example at line {} does not reach it",
                        example.start.line
                    ));
                }
            }
        }
        Ok(())
    }

    /// The hooks a Minitest test's class runs from its ancestors and its
    /// other bodies: each checkout body's `setup` and statements, read as its
    /// own body's are. Refused when an ancestor is unindexed (its hooks are
    /// unseen) unless it is a framework's test base, whose checkout
    /// reopenings are weighed whether the chain reaches them or not.
    fn inherited_setup(
        &self,
        path: &str,
        shape: &Shape,
        span: &Span,
    ) -> Result<Vec<(String, Pos, Pos)>, String> {
        let Some(scope) = shape
            .around(span.start)
            .into_iter()
            .rev()
            .find(|s| s.kind == SpanKind::Scope)
        else {
            return Ok(Vec::new());
        };
        let facts = self
            .files
            .facts(path)
            .ok_or_else(|| format!("unread {path}"))?;
        let class = facts
            .defs
            .iter()
            .find(|d| {
                matches!(d.kind, crate::core::Kind::Class | crate::core::Kind::Module)
                    && d.pos.line == scope.start.line
            })
            .ok_or_else(|| format!("no class opens line {}", scope.start.line))?;
        let mut written = vec![class.name.clone()];
        written.extend(class.nesting.iter().cloned());
        let fqn = self
            .tree
            .scope_fqn(&written)
            .ok_or_else(|| format!("{} unplaced", class.name))?;
        let base = |name: &str| TEST_BASES.contains(&name.trim_start_matches("::"));
        let ancestry = self.tree.ancestors(&fqn);
        if let Some(hidden) = ancestry.unresolved.iter().find(|name| !base(name)) {
            return Err(format!("{fqn} inherits {hidden}, unindexed"));
        }
        let names = ancestry
            .chain
            .iter()
            .map(String::as_str)
            .chain(TEST_BASES)
            .collect::<Vec<_>>();
        let mut out = Vec::new();
        for name in names {
            for site in self.tree.sites(name) {
                if !self.tree.in_checkout(&site.path) {
                    continue;
                }
                let relative = self.relative(&site.path);
                if relative == path && site.line == scope.start.line {
                    continue;
                }
                // A live module mixed into the test class: its setup is live code.
                if !in_tests(&relative) {
                    let facts = self
                        .files
                        .facts(&relative)
                        .ok_or_else(|| format!("unread {relative}"))?;
                    if let Some(def) = facts.defs.iter().find(|d| {
                        d.kind == crate::core::Kind::Method
                            && !d.singleton
                            && SETUP.contains(&d.name.as_str())
                            && self.tree.scope_fqn(&d.nesting).as_deref() == Some(name)
                    }) {
                        return Err(format!("{name}#{} ({relative}) runs first", def.name));
                    }
                    continue;
                }
                let other = self
                    .shape(&relative)
                    .ok_or_else(|| format!("unread {relative}"))?;
                let hooks = other
                    .scope_hooks(site.line)
                    .ok_or_else(|| format!("no body read at {relative}:{}", site.line))?;
                for hook in hooks {
                    if hook.kind == SpanKind::Unseen {
                        return Err(format!(
                            "setup names a method its class does not define, at {relative}:{}",
                            hook.start.line
                        ));
                    }
                    out.push((relative.clone(), hook.start, hook.end));
                }
            }
        }
        Ok(out)
    }

    /// Every call in the spans, and in each body they reach through test
    /// code — a `let` read, a helper called — lands outside the checkout,
    /// on the target, or on the target's class being built. `Ok(true)` when
    /// one reaches the target.
    fn weigh(&self, mut todo: Vec<(String, Pos, Pos)>, from: &From) -> Result<bool, String> {
        let mut touched = false;
        let mut seen: Vec<(String, Pos)> = Vec::new();
        while let Some((path, start, end)) = todo.pop() {
            if seen.iter().any(|(p, s)| *p == path && *s == start) {
                continue;
            }
            if seen.len() >= MAX_BODIES {
                return Err("too many bodies".into());
            }
            seen.push((path.clone(), start));
            let Some(facts) = self.files.facts(&path) else {
                return Err(format!("unread {path}"));
            };
            self.reads_live(&facts, &path, start, end)?;
            let within = |at: Pos| start <= at && at <= end;
            // A name sent: only a literal one can be weighed.
            for send in facts.sends.iter().filter(|s| within(s.call)) {
                let name = match &send.name {
                    SentName::Symbol(name) | SentName::String(name) => name,
                    SentName::Shape(_) | SentName::Computed => {
                        return Err(format!("computed send at {path}:{}", send.call.line));
                    }
                };
                // Any method of the name may answer it, not only the target.
                if self.live_named(name) {
                    return Err(format!("sends {name} at {path}:{}", send.call.line));
                }
                touched |= *name == self.target.name;
            }
            // A name RSpec or a block-pass calls without the code writing it.
            for (at, name) in self.implied(&path).iter().filter(|(at, _)| within(*at)) {
                if self.live_named(name) {
                    return Err(format!("calls {name} implicitly at {path}:{}", at.line));
                }
                touched |= *name == self.target.name;
            }
            for call in facts.calls.iter().filter(|c| within(c.pos)) {
                match self.reaches(&facts, &path, call, from) {
                    Reach::Nothing => {}
                    Reach::Target => touched = true,
                    Reach::Other(why) => {
                        return Err(format!("{why} at {path}:{}", call.pos.line));
                    }
                    Reach::Body(more) => todo.extend(more),
                }
            }
        }
        Ok(touched)
    }

    /// Whether the target, and what builds its class (`initialize`, a
    /// `self.new`), reach nothing else of the checkout: deleting a test of
    /// it then loses a test of nothing live.
    fn closed(&self) -> Result<(), String> {
        let target = self.target;
        let mut seen = Vec::new();
        let def = self.def_at(&target.path, target.line, &target.name)?;
        // A macro's method runs what the macro made, unread here.
        if let Some(via) = def.via.as_deref().filter(|via| !via.starts_with("attr_")) {
            return Err(format!("made by {via}"));
        }
        self.live_body(&target.path, &def, &mut seen)?;
        for name in PROTOCOL.iter().filter(|name| **name != target.name) {
            for singleton in [false, true] {
                let Some(found) = self.tree.lookup(&target.owner, singleton, name) else {
                    continue;
                };
                let relative = self.relative(&found.site.path);
                if self.tree.in_checkout(&found.site.path) && !in_tests(&relative) {
                    return Err(format!("its class defines {name} ({relative})"));
                }
            }
        }
        for (singleton, name) in [(false, "initialize"), (true, "new")] {
            let Some(found) = self.tree.lookup(&target.owner, singleton, name) else {
                continue;
            };
            if !self.tree.in_checkout(&found.site.path) {
                continue;
            }
            let relative = self.relative(&found.site.path);
            let def = self.def_at(&relative, found.site.line, name)?;
            self.live_body(&relative, &def, &mut seen)?;
        }
        Ok(())
    }

    /// The names a file calls without writing the call: `&:name`, an
    /// RSpec predicate matcher's, `have_attributes`' keys, `its`' attribute.
    fn implied(&self, path: &str) -> Vec<(Pos, String)> {
        self.shape(path)
            .map(|s| s.implied.clone())
            .unwrap_or_default()
    }

    /// A file's examples and groups, read once.
    fn shape(&self, path: &str) -> Option<Arc<Shape>> {
        self.shapes
            .borrow_mut()
            .entry(path.to_string())
            .or_insert_with(|| {
                let facts = self.files.facts(path)?;
                let source = facts.source.clone()?;
                Some(Arc::new(Shape::read(&source)))
            })
            .clone()
    }

    /// The method `name` defined at `line` of a file.
    fn def_at(&self, path: &str, line: u32, name: &str) -> Result<Def, String> {
        let facts = self
            .files
            .facts(path)
            .ok_or_else(|| format!("unread {path}"))?;
        facts
            .defs
            .iter()
            .filter(|d| d.kind == crate::core::Kind::Method && d.pos.line == line && d.name == name)
            .min_by_key(|d| d.end_line)
            .cloned()
            .ok_or_else(|| format!("no body at {path}:{line}"))
    }

    /// Every call, name sent and constant read in a checkout method's body
    /// lands outside the checkout, on the target, or on building its class.
    fn live_body(
        &self,
        path: &str,
        def: &Def,
        seen: &mut Vec<(String, u32)>,
    ) -> Result<(), String> {
        if seen.iter().any(|(p, l)| p == path && *l == def.pos.line) {
            return Ok(());
        }
        seen.push((path.to_string(), def.pos.line));
        let facts = self
            .files
            .facts(path)
            .ok_or_else(|| format!("unread {path}"))?;
        let (_, start, end) = body(path, def);
        let within = |at: Pos| start <= at && at <= end;
        for send in facts.sends.iter().filter(|s| within(s.call)) {
            match &send.name {
                SentName::Symbol(name) | SentName::String(name) => {
                    if self.live_named(name) {
                        return Err(format!("sends {name} at {path}:{}", send.call.line));
                    }
                }
                SentName::Shape(_) | SentName::Computed => {
                    return Err(format!("computed send at {path}:{}", send.call.line));
                }
            }
        }
        for (at, name) in self.implied(path).iter().filter(|(at, _)| within(*at)) {
            if self.live_named(name) {
                return Err(format!("calls {name} implicitly at {path}:{}", at.line));
            }
        }
        for call in facts
            .calls
            .iter()
            .filter(|c| within(c.pos) && c.recv != RecvShape::Symbol)
        {
            let answer = crate::resolve::method_at(self.tree, &facts, call, path);
            let sites = answer
                .sites
                .iter()
                .map(|s| (&s.site, s.method.as_ref().map(|m| m.owner.as_str())))
                .chain(
                    answer
                        .candidates
                        .iter()
                        .map(|c| (&c.site, Some(c.owner.as_str()))),
                );
            for (site, owner) in sites {
                if !self.tree.in_checkout(&site.path) {
                    continue;
                }
                let relative = self.relative(&site.path);
                if relative == self.target.path && site.line == self.target.line {
                    continue;
                }
                // Weighed by `closed` as the class's own building.
                if matches!(call.name.as_str(), "new" | "initialize")
                    && owner == Some(self.target.owner.as_str())
                {
                    continue;
                }
                return Err(format!(
                    "{}#{} ({relative}:{}) at {path}:{}",
                    owner.unwrap_or("?"),
                    call.name,
                    site.line,
                    call.pos.line
                ));
            }
            let settled = answer.status == crate::tree::Status::Resolved
                || matches!(call.recv, RecvShape::Implicit | RecvShape::SelfRecv);
            if !settled && self.live_named(&call.name) {
                return Err(format!("`{}` on a receiver no type settles", call.name));
            }
        }
        self.reads_live(&facts, path, start, end)
    }

    /// Whether the span reads a checkout constant, outside its tests, but
    /// the target's class and the namespaces around it: a lambda or a value
    /// computed there is live code.
    fn reads_live(&self, facts: &Facts, path: &str, start: Pos, end: Pos) -> Result<(), String> {
        for read in facts
            .const_refs
            .iter()
            .filter(|r| start <= r.pos && r.pos <= end)
        {
            let Some(fqn) = self.tree.resolve_at(&read.name, &read.nesting, path).fqn else {
                continue;
            };
            // `Billing::Invoice` reads `Billing` on the way: a namespace runs nothing.
            let owner = &self.target.owner;
            if fqn == *owner || owner.starts_with(&format!("{fqn}::")) {
                continue;
            }
            let live = self.tree.sites(&fqn).iter().any(|site| {
                self.tree.in_checkout(&site.path) && !in_tests(&self.relative(&site.path))
            });
            if live {
                return Err(format!("reads {fqn} at {path}:{}", read.pos.line));
            }
        }
        Ok(())
    }

    /// What one call reaches of the checkout.
    fn reaches(&self, facts: &Facts, path: &str, call: &crate::core::Call, from: &From) -> Reach {
        // A symbol a send hands is weighed as the send; one a macro is
        // handed (`let(:x)`, `Fabricate(:user)`) runs nothing by itself.
        if call.recv == RecvShape::Symbol {
            return Reach::Nothing;
        }
        // A shared group's examples and hooks run here, unweighed.
        if members::SHARED_INCLUDERS.contains(&call.name.as_str()) {
            return Reach::Other(format!("includes a shared group ({})", call.name));
        }
        // `is_expected` and a bare `should` read the `subject`.
        let read =
            match call.recv == RecvShape::Implicit && READS_SUBJECT.contains(&call.name.as_str()) {
                true => crate::core::Call {
                    name: "subject".to_string(),
                    ..call.clone()
                },
                false => call.clone(),
            };
        // Outside the unit, a member read is the one the unit's examples see.
        let unit = from
            .nesting
            .as_ref()
            .filter(|_| {
                !from.holds(path, call.pos)
                    && matches!(read.recv, RecvShape::Implicit | RecvShape::SelfRecv)
            })
            .and_then(|nesting| {
                let facts = self.files.facts(&from.path)?;
                let mut asked = read.clone();
                asked.nesting = nesting.clone();
                members::named_by(self.tree, &facts, &asked)
                    .map(|named| (from.path.as_str(), named))
            });
        let named =
            unit.or_else(|| members::named_by(self.tree, facts, &read).map(|named| (path, named)));
        if let Some((written, named)) = named {
            return match named {
                Named::Here(def) => Reach::Body(vec![body(written, &def)]),
                // A module the group includes may be live code.
                Named::Shared { site, line } => {
                    let relative = self.relative(&site);
                    if relative == self.target.path && line == self.target.line {
                        Reach::Target
                    } else if self.tree.in_checkout(&site) && !in_tests(&relative) {
                        Reach::Other(format!("{} ({relative}:{line})", read.name))
                    } else {
                        self.body_at(&site, line)
                    }
                }
            };
        }
        // An implicit `subject` is `described_class.new`: each builds the
        // class the group describes, whose building `closed` weighs only
        // for the target's own class. Read where the unit's examples run.
        if read.recv == RecvShape::Implicit
            && matches!(read.name.as_str(), "subject" | "described_class")
        {
            let (facts, nesting, path) = match &from.nesting {
                Some(nesting) if !from.holds(path, call.pos) => {
                    let Some(facts) = self.files.facts(&from.path) else {
                        return Reach::Other(format!("unread {}", from.path));
                    };
                    (facts, nesting.clone(), from.path.as_str())
                }
                _ => (
                    self.files.facts(path).unwrap_or_default(),
                    read.nesting.clone(),
                    path,
                ),
            };
            return match self.described(&facts, &nesting, path) {
                None => Reach::Nothing,
                Some(Ok(fqn)) if fqn == self.target.owner => Reach::Nothing,
                Some(Ok(fqn)) => Reach::Other(format!("builds the described {fqn}")),
                Some(Err(name)) => Reach::Other(format!("describes {name}, unresolved")),
            };
        }
        let answer = crate::resolve::method_at(self.tree, facts, call, path);
        let mut more = Vec::new();
        let mut touched = false;
        let sites = answer
            .sites
            .iter()
            .map(|s| (&s.site, s.method.as_ref().map(|m| m.owner.as_str())))
            .chain(
                answer
                    .candidates
                    .iter()
                    .map(|c| (&c.site, Some(c.owner.as_str()))),
            );
        for (site, owner) in sites {
            match self.weigh_site(site, owner, &call.name) {
                Reach::Nothing => {}
                Reach::Target => touched = true,
                Reach::Other(why) => return Reach::Other(why),
                Reach::Body(body) => more.extend(body),
            }
        }
        // An answer a type does not settle may be any method of the name.
        let settled = answer.status == crate::tree::Status::Resolved
            || matches!(call.recv, RecvShape::Implicit | RecvShape::SelfRecv);
        if !settled && self.live_named(&call.name) {
            return Reach::Other(format!("`{}` on a receiver no type settles", call.name));
        }
        match (more.is_empty(), touched) {
            (false, _) => Reach::Body(more),
            (true, true) => Reach::Target,
            (true, false) => Reach::Nothing,
        }
    }

    /// The class `described_class` is where `nesting` is written: the
    /// innermost group around it that describes a constant, or why that
    /// constant names none. Mirrors `resolve::described_by`.
    fn described(
        &self,
        facts: &Facts,
        nesting: &[String],
        path: &str,
    ) -> Option<Result<String, String>> {
        let (level, class) = (0..nesting.len()).find_map(|at| {
            let level = &nesting[at..];
            facts
                .described
                .iter()
                .find(|(group, _)| group == level)
                .map(|(_, class)| (level, class))
        })?;
        Some(
            self.tree
                .resolve_at(class, level, path)
                .fqn
                .ok_or_else(|| class.clone()),
        )
    }

    fn weigh_site(&self, site: &crate::tree::Site, owner: Option<&str>, name: &str) -> Reach {
        if !self.tree.in_checkout(&site.path) {
            return Reach::Nothing;
        }
        let relative = self.relative(&site.path);
        if in_tests(&relative) {
            return self.body_at(&site.path, site.line);
        }
        if relative == self.target.path && site.line == self.target.line {
            return Reach::Target;
        }
        // Building the target's class is how a spec gets one to call it on.
        let builds =
            matches!(name, "new" | "initialize") && owner == Some(self.target.owner.as_str());
        match builds {
            true => Reach::Nothing,
            false => Reach::Other(format!(
                "{}#{name} ({relative}:{})",
                owner.unwrap_or("?"),
                site.line
            )),
        }
    }

    /// The body of the method defined at `line` of a test file, to weigh.
    fn body_at(&self, site_path: &str, line: u32) -> Reach {
        let relative = self.relative(site_path);
        let Some(facts) = self.files.facts(&relative) else {
            return Reach::Other(format!("unread {relative}"));
        };
        match facts
            .defs
            .iter()
            .filter(|d| d.kind == crate::core::Kind::Method && d.pos.line == line)
            .min_by_key(|d| d.end_line)
        {
            Some(def) => Reach::Body(vec![body(&relative, def)]),
            None => Reach::Other(format!("no body at {relative}:{line}")),
        }
    }

    /// Whether the checkout's own code, outside its tests, defines a method
    /// of this name other than the target.
    fn live_named(&self, name: &str) -> bool {
        self.tree.named(name).iter().any(|m| {
            self.tree.in_checkout(&m.site.path) && {
                let relative = self.relative(&m.site.path);
                !in_tests(&relative)
                    && !(relative == self.target.path && m.site.line == self.target.line)
            }
        })
    }

    fn relative(&self, site_path: &str) -> String {
        site_path
            .strip_prefix(self.root)
            .map(|rest| rest.trim_start_matches('/').to_string())
            .unwrap_or_else(|| site_path.to_string())
    }
}

enum Reach {
    /// Nothing of the checkout's.
    Nothing,
    /// The target.
    Target,
    /// Another method of the checkout's, or what may be: said how.
    Other(String),
    /// Test code, whose bodies are weighed in turn.
    Body(Vec<(String, Pos, Pos)>),
}

/// A method's body, as a span to weigh.
fn body(path: &str, def: &Def) -> (String, Pos, Pos) {
    (
        path.to_string(),
        def.pos,
        Pos {
            line: def.end_line,
            col: u32::MAX,
        },
    )
}

/// A test file's examples and groups, where each starts and ends.
struct Shape {
    spans: Vec<Span>,
    /// Names called without a call written, where.
    implied: Vec<(Pos, String)>,
    /// Each span's innermost group, by index into `spans`.
    group_of: Vec<Option<usize>>,
    /// The one group the file holds at its top level, when it holds nothing
    /// else but `require`s.
    sole: Option<Pos>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SpanKind {
    Group,
    /// A shared group's body, or a group `it_behaves_like` opens.
    Shared,
    Example,
    /// What runs before each example of its group and those nested in it:
    /// a `before`, `before_all` or `around` hook, a `let_it_be`, a `let!` or
    /// any call named with a `!`. Not an `after`,
    /// which runs once the example has asserted what it asserts.
    Hook,
    /// A class or module body: a Minitest test class's examples run its
    /// `setup` and the statements of its body as hooks.
    Scope,
    /// A hook whose body is not read: `setup :name` with no `def name` in
    /// its class. A unit it runs before is none.
    Unseen,
}

struct Span {
    kind: SpanKind,
    /// The call that opens it (`it`, `describe`), empty for a `test_` method.
    call: String,
    name: String,
    start: Pos,
    end: Pos,
    /// Nothing but blanks or a comment shares its first and last lines.
    alone: bool,
}

impl Span {
    fn holds(&self, at: Pos) -> bool {
        self.start <= at && at <= self.end
    }

    fn covers(&self, other: &Span) -> bool {
        self.start <= other.start && other.end <= self.end
    }

    fn block(&self) -> Block {
        Block {
            name: self.name.clone(),
            line: self.start.line,
            end_line: self.end.line,
        }
    }
}

/// Calls whose block is an example group's body.
const GROUPS: [&str; 10] = [
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
];

/// Calls whose block is run where a group includes it.
const SHARED: [&str; 5] = [
    "shared_examples",
    "shared_context",
    "shared_examples_for",
    "it_behaves_like",
    "it_should_behave_like",
];

/// Calls whose block runs before each example in reach; so does any call
/// named with a `!` (`let!`, `subject!`, `fab!`), as `members::eager` reads
/// a member.
const HOOKS: [&str; 6] = [
    "before",
    "prepend_before",
    "append_before",
    "around",
    "before_all",
    "let_it_be",
];

/// Statements of a group's body that run only when read (`let`), after
/// the example (`after`), as examples or groups of their own, or are refused
/// whole (`include_context`): no hook of the examples around them.
const READ_ELSEWHERE: [&str; 9] = [
    "let",
    "subject",
    "after",
    "prepend_after",
    "append_after",
    "include_context",
    "include_examples",
    "pending",
    "skip",
];

/// What Minitest runs before each test: a method of the name, and Active
/// Support's `setup` callback.
const SETUP: [&str; 3] = ["before_setup", "setup", "after_setup"];

/// What Minitest runs once the test has asserted, as an `after` hook.
const TEARDOWN: [&str; 3] = ["before_teardown", "teardown", "after_teardown"];

/// Test base classes whose own hooks are the framework's: an unindexed one
/// hides nothing of the checkout's, but a reopening in the checkout does.
const TEST_BASES: [&str; 14] = [
    "Minitest::Test",
    "MiniTest::Test",
    "Minitest::Spec",
    "MiniTest::Unit::TestCase",
    "Test::Unit::TestCase",
    "ActiveSupport::TestCase",
    "ActionDispatch::IntegrationTest",
    "ActionController::TestCase",
    "ActionView::TestCase",
    "ActionMailer::TestCase",
    "ActionDispatch::SystemTestCase",
    "ActiveJob::TestCase",
    "ActionCable::Channel::TestCase",
    "Rails::Generators::TestCase",
];

/// Calls that include a shared group's hooks and `let`s into the group
/// they are written in (`it_behaves_like` opens a group of its own).
const INCLUDES_INTO: [&str; 2] = ["include_context", "include_examples"];

/// RSpec's own matchers named `be_…`, which ask no predicate.
const BE_MATCHERS: [&str; 12] = [
    "be_a",
    "be_an",
    "be_a_kind_of",
    "be_an_instance_of",
    "be_kind_of",
    "be_instance_of",
    "be_truthy",
    "be_falsey",
    "be_falsy",
    "be_nil",
    "be_within",
    "be_between",
];

/// Calls that read the `subject` without naming it: `its` reads an
/// attribute of it.
const READS_SUBJECT: [&str; 5] = ["is_expected", "are_expected", "should", "should_not", "its"];

/// Calls whose block is an example: RSpec's, and Active Support's `test`.
const EXAMPLES: [&str; 15] = [
    "it",
    "specify",
    "example",
    "scenario",
    "its",
    "focus",
    "fit",
    "fspecify",
    "fexample",
    "fscenario",
    "xit",
    "xspecify",
    "xexample",
    "xscenario",
    "test",
];

impl Shape {
    fn read(source: &[u8]) -> Shape {
        use ruby_prism::Visit;
        struct Walk<'a> {
            source: &'a [u8],
            lines: &'a LineIndex,
            spans: Vec<Span>,
            implied: Vec<(Pos, String)>,
        }
        impl Walk<'_> {
            fn imply(&mut self, at: usize, name: &[u8]) {
                let at = self.lines.pos(at);
                self.implied
                    .push((at, String::from_utf8_lossy(name).into_owned()));
            }

            /// The names an RSpec matcher or `its` calls on its object.
            fn implied_by(&mut self, call: &ruby_prism::CallNode<'_>, name: &str) {
                let at = call.location().start_offset();
                let args: Vec<ruby_prism::Node<'_>> = call
                    .arguments()
                    .map(|a| a.arguments().iter().collect())
                    .unwrap_or_default();
                match name {
                    "have_attributes" => {
                        for arg in &args {
                            let elements = match (arg.as_keyword_hash_node(), arg.as_hash_node()) {
                                (Some(hash), _) => hash.elements(),
                                (_, Some(hash)) => hash.elements(),
                                _ => continue,
                            };
                            for element in elements.iter() {
                                if let Some(key) = element
                                    .as_assoc_node()
                                    .and_then(|assoc| assoc.key().as_symbol_node())
                                {
                                    self.imply(at, key.unescaped());
                                }
                            }
                        }
                    }
                    "its" => match args.first() {
                        Some(first) if first.as_symbol_node().is_some() => {
                            let symbol = first.as_symbol_node().expect("a symbol");
                            self.imply(at, symbol.unescaped());
                        }
                        Some(first) if first.as_string_node().is_some() => {
                            let text = first.as_string_node().expect("a string");
                            let text = text.unescaped().to_vec();
                            for part in text.split(|b| *b == b'.') {
                                self.imply(at, part);
                            }
                        }
                        Some(first) if first.as_array_node().is_some() => self.imply(at, b"[]"),
                        _ => {}
                    },
                    _ => {
                        // RSpec's `be_visible` asks `visible?` (or `visibles?`),
                        // `have_key` asks `has_key?`.
                        if let Some(rest) = name.strip_prefix("be_") {
                            if !BE_MATCHERS.contains(&name) {
                                let rest = rest
                                    .strip_prefix("an_")
                                    .or_else(|| rest.strip_prefix("a_"))
                                    .unwrap_or(rest);
                                self.imply(at, format!("{rest}?").as_bytes());
                                self.imply(at, format!("{rest}s?").as_bytes());
                            }
                        } else if let Some(rest) = name.strip_prefix("have_") {
                            self.imply(at, format!("has_{rest}?").as_bytes());
                        }
                    }
                }
            }
            fn span(
                &mut self,
                kind: SpanKind,
                call: &str,
                name: String,
                loc: &ruby_prism::Location<'_>,
            ) {
                let (from, to) = (loc.start_offset(), loc.end_offset());
                let before = &self.source[..from];
                let before = &before[before
                    .iter()
                    .rposition(|b| *b == b'\n')
                    .map_or(0, |at| at + 1)..];
                let after = &self.source[to..];
                let after = &after[..after
                    .iter()
                    .position(|b| *b == b'\n')
                    .unwrap_or(after.len())];
                let after = after.trim_ascii_start();
                let alone =
                    before.trim_ascii().is_empty() && (after.is_empty() || after[0] == b'#');
                self.spans.push(Span {
                    kind,
                    call: call.to_string(),
                    name,
                    start: self.lines.pos(from),
                    end: self.lines.pos(to.saturating_sub(1)),
                    alone,
                });
            }
        }
        impl Walk<'_> {
            /// What a group's body runs as the file loads, or registers to
            /// run before its examples, that is no RSpec DSL read elsewhere:
            /// a macro from a helper (`with_registry`), an assignment, an
            /// `if` around hooks. Each is weighed as a hook.
            fn group_body(&mut self, call: &ruby_prism::CallNode<'_>) {
                let Some(body) = call
                    .block()
                    .and_then(|b| b.as_block_node())
                    .and_then(|b| b.body())
                else {
                    return;
                };
                let Some(statements) = body.as_statements_node() else {
                    self.span(SpanKind::Hook, "", String::new(), &body.location());
                    return;
                };
                for statement in statements.body().iter() {
                    if statement.as_def_node().is_some() {
                        continue;
                    }
                    let name = statement.as_call_node().map(|c| {
                        let name = String::from_utf8_lossy(c.name().as_slice()).into_owned();
                        let block = c.block().and_then(|b| b.as_block_node()).is_some();
                        (name, block)
                    });
                    if let Some((name, block)) = &name {
                        let hook = HOOKS.contains(&name.as_str()) || name.ends_with('!');
                        let elsewhere =
                            [&READ_ELSEWHERE[..], &GROUPS[..], &SHARED[..], &EXAMPLES[..]]
                                .iter()
                                .any(|names| names.contains(&name.as_str()));
                        if (hook && *block) || elsewhere {
                            continue;
                        }
                    }
                    let name = name.map(|(name, _)| name).unwrap_or_default();
                    self.span(SpanKind::Hook, &name, name.clone(), &statement.location());
                }
            }

            /// A class or module body, as a Minitest test class runs it
            /// around each test: its `setup` methods and `setup` callbacks,
            /// and each statement of its body that is no test, `teardown`
            /// or definition, are hooks.
            fn scope_body(&mut self, body: Option<ruby_prism::Node<'_>>) {
                let Some(body) = body else {
                    return;
                };
                let Some(statements) = body.as_statements_node() else {
                    self.span(SpanKind::Hook, "", String::new(), &body.location());
                    return;
                };
                let defs: Vec<ruby_prism::DefNode<'_>> = statements
                    .body()
                    .iter()
                    .filter_map(|s| s.as_def_node())
                    .filter(|d| d.receiver().is_none())
                    .collect();
                for statement in statements.body().iter() {
                    if let Some(def) = statement.as_def_node() {
                        let name = String::from_utf8_lossy(def.name().as_slice()).into_owned();
                        if def.receiver().is_none() && SETUP.contains(&name.as_str()) {
                            self.span(SpanKind::Hook, &name, name.clone(), &def.location());
                        }
                        continue;
                    }
                    if statement.as_class_node().is_some() || statement.as_module_node().is_some() {
                        continue;
                    }
                    if let Some(call) = statement.as_call_node()
                        && self.on_rspec(&call)
                    {
                        let name = String::from_utf8_lossy(call.name().as_slice()).into_owned();
                        let own_span = [&GROUPS[..], &SHARED[..], &EXAMPLES[..]]
                            .iter()
                            .any(|names| names.contains(&name.as_str()));
                        if own_span || TEARDOWN.contains(&name.as_str()) {
                            continue;
                        }
                        if !SETUP.contains(&name.as_str()) {
                            self.span(SpanKind::Hook, &name, name.clone(), &statement.location());
                            continue;
                        }
                        // The callback's block, a lambda, or the method a
                        // symbol names, not the `setup` call: a test class's
                        // `def setup` is no candidate for it.
                        let args = call.arguments().map(|a| a.arguments().iter().collect());
                        for arg in args.unwrap_or_else(Vec::new) {
                            let Some(symbol) = arg.as_symbol_node() else {
                                self.span(SpanKind::Hook, &name, name.clone(), &arg.location());
                                continue;
                            };
                            match defs
                                .iter()
                                .find(|d| d.name().as_slice() == symbol.unescaped())
                            {
                                Some(def) => {
                                    self.span(SpanKind::Hook, &name, name.clone(), &def.location())
                                }
                                None => self.span(
                                    SpanKind::Unseen,
                                    &name,
                                    name.clone(),
                                    &arg.location(),
                                ),
                            }
                        }
                        if let Some(block) = call.block() {
                            self.span(SpanKind::Hook, &name, name.clone(), &block.location());
                        }
                        continue;
                    }
                    self.span(SpanKind::Hook, "", String::new(), &statement.location());
                }
            }

            /// No receiver, or `RSpec`: what opens a group or an example.
            fn on_rspec(&self, call: &ruby_prism::CallNode<'_>) -> bool {
                call.receiver().is_none_or(|r| {
                    let loc = r.location();
                    let text = &self.source[loc.start_offset()..loc.end_offset()];
                    text == b"RSpec" || text == b"::RSpec"
                })
            }

            fn scope(&mut self, path: &ruby_prism::Node<'_>, loc: &ruby_prism::Location<'_>) {
                let at = path.location();
                let name =
                    String::from_utf8_lossy(&self.source[at.start_offset()..at.end_offset()])
                        .into_owned();
                self.span(SpanKind::Scope, "", name, loc);
            }
        }
        impl<'pr> Visit<'pr> for Walk<'_> {
            fn visit_call_node(&mut self, call: &ruby_prism::CallNode<'pr>) {
                let name = String::from_utf8_lossy(call.name().as_slice()).into_owned();
                if call.receiver().is_none() {
                    self.implied_by(call, &name);
                }
                let on_rspec = self.on_rspec(call);
                let kind = if !on_rspec || call.block().and_then(|b| b.as_block_node()).is_none() {
                    None
                } else if GROUPS.contains(&name.as_str()) {
                    Some(SpanKind::Group)
                } else if SHARED.contains(&name.as_str()) {
                    Some(SpanKind::Shared)
                } else if call.receiver().is_none() && EXAMPLES.contains(&name.as_str()) {
                    Some(SpanKind::Example)
                } else if call.receiver().is_none()
                    && (HOOKS.contains(&name.as_str()) || name.ends_with('!'))
                {
                    Some(SpanKind::Hook)
                } else {
                    None
                };
                if kind == Some(SpanKind::Group) {
                    self.group_body(call);
                }
                if let Some(kind) = kind {
                    let described = call
                        .arguments()
                        .and_then(|args| args.arguments().iter().next())
                        .map(|first| {
                            let loc = first.location();
                            let text = &self.source[loc.start_offset()..loc.end_offset()];
                            let text = String::from_utf8_lossy(text);
                            text.trim_matches(|c| c == '"' || c == '\'').to_string()
                        })
                        .unwrap_or_default();
                    self.span(kind, &name, described, &call.location());
                }
                ruby_prism::visit_call_node(self, call);
            }

            fn visit_block_argument_node(&mut self, node: &ruby_prism::BlockArgumentNode<'pr>) {
                if let Some(symbol) = node.expression().and_then(|e| e.as_symbol_node()) {
                    self.imply(node.location().start_offset(), symbol.unescaped());
                }
                ruby_prism::visit_block_argument_node(self, node);
            }

            fn visit_class_node(&mut self, node: &ruby_prism::ClassNode<'pr>) {
                self.scope(&node.constant_path(), &node.location());
                self.scope_body(node.body());
                ruby_prism::visit_class_node(self, node);
            }

            fn visit_module_node(&mut self, node: &ruby_prism::ModuleNode<'pr>) {
                self.scope(&node.constant_path(), &node.location());
                self.scope_body(node.body());
                ruby_prism::visit_module_node(self, node);
            }

            fn visit_def_node(&mut self, def: &ruby_prism::DefNode<'pr>) {
                let name = String::from_utf8_lossy(def.name().as_slice()).into_owned();
                if def.receiver().is_none() && name.starts_with("test_") {
                    self.span(SpanKind::Example, "", name, &def.location());
                }
                ruby_prism::visit_def_node(self, def);
            }
        }
        let parsed = ruby_prism::parse(source);
        let lines = LineIndex::new(source);
        let mut walk = Walk {
            source,
            lines: &lines,
            spans: Vec::new(),
            implied: Vec::new(),
        };
        walk.visit(&parsed.node());
        let sole = sole_group(&parsed.node(), source, &lines);
        let spans = walk.spans;
        let implied = walk.implied;
        let group_of = (0..spans.len())
            .map(|at| {
                (0..spans.len())
                    .filter(|&g| {
                        // A class a group's body writes is a hook of that
                        // group, not of its own body.
                        (spans[g].start, spans[g].end) != (spans[at].start, spans[at].end)
                            && matches!(
                                spans[g].kind,
                                SpanKind::Group | SpanKind::Shared | SpanKind::Scope
                            )
                            && spans[g].covers(&spans[at])
                    })
                    .min_by_key(|&g| {
                        (
                            spans[g].end.line - spans[g].start.line,
                            std::cmp::Reverse(spans[g].start),
                        )
                    })
            })
            .collect();
        Shape {
            spans,
            implied,
            group_of,
            sole,
        }
    }

    /// The spans holding `at`, outermost first.
    fn around(&self, at: Pos) -> Vec<&Span> {
        let mut found: Vec<&Span> = self
            .spans
            .iter()
            .filter(|s| s.start <= at && at <= s.end)
            .collect();
        found.sort_by_key(|s| (s.start, std::cmp::Reverse(s.end)));
        found
    }

    /// The smallest group holding `at`.
    fn innermost_group(&self, at: Pos) -> Option<&Span> {
        self.spans
            .iter()
            .filter(|s| matches!(s.kind, SpanKind::Group | SpanKind::Shared) && s.holds(at))
            .min_by_key(|s| (s.end.line - s.start.line, std::cmp::Reverse(s.start)))
    }

    /// The hooks outside `span` that run before its examples: those of the
    /// groups around it.
    fn hooks_before<'s>(&'s self, span: &'s Span) -> impl Iterator<Item = &'s Span> {
        self.spans
            .iter()
            .zip(&self.group_of)
            .filter_map(move |(h, group)| {
                let g = &self.spans[(*group)?];
                (matches!(h.kind, SpanKind::Hook | SpanKind::Unseen)
                    && !span.covers(h)
                    && g.covers(span)
                    && g.start != span.start)
                    .then_some(h)
            })
    }

    /// The hooks of the class or module body that opens at `line`, as a
    /// Minitest test class runs them; `None` for no body there.
    fn scope_hooks(&self, line: u32) -> Option<Vec<&Span>> {
        let at = self
            .spans
            .iter()
            .position(|s| s.kind == SpanKind::Scope && s.start.line == line)?;
        Some(
            self.spans
                .iter()
                .zip(&self.group_of)
                .filter(|(h, group)| {
                    matches!(h.kind, SpanKind::Hook | SpanKind::Unseen) && **group == Some(at)
                })
                .map(|(h, _)| h)
                .collect(),
        )
    }

    /// The examples a group holds, however deep.
    fn examples_in<'s>(&'s self, group: &'s Span) -> impl Iterator<Item = &'s Span> {
        self.spans
            .iter()
            .filter(move |s| s.kind == SpanKind::Example && group.covers(s))
    }

    fn whole_file(&self, span: &Span) -> bool {
        self.sole == Some(span.start)
    }
}

/// Where the file's one top-level group starts, when its other top-level
/// statements are only `require`s.
fn sole_group(node: &ruby_prism::Node<'_>, source: &[u8], lines: &LineIndex) -> Option<Pos> {
    let program = node.as_program_node()?;
    let mut group = None;
    for statement in program.statements().body().iter() {
        let call = statement.as_call_node()?;
        let name = call.name();
        let name = name.as_slice();
        if matches!(name, b"require" | b"require_relative") && call.receiver().is_none() {
            continue;
        }
        let on_rspec = call.receiver().is_none_or(|r| {
            let loc = r.location();
            matches!(
                &source[loc.start_offset()..loc.end_offset()],
                b"RSpec" | b"::RSpec"
            )
        });
        let is_group = on_rspec
            && GROUPS.iter().any(|g| g.as_bytes() == name)
            && call.block().and_then(|b| b.as_block_node()).is_some();
        if !is_group || group.is_some() {
            return None;
        }
        group = Some(lines.pos(call.location().start_offset()));
    }
    group
}
