//! A `test-only` method's tests (DEC-730): each site a test writes, with the
//! example and group around it.

use super::dead::{DeadRow, Detail, MethodDetail};
use crate::core::Pos;
use crate::extract::line_index::LineIndex;
use crate::query::members::CheckoutFiles;
use crate::resolve::members::Files;
use serde::Serialize;
use std::collections::HashMap;

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

/// Fills each `test-only` method row's `test_sites` with the example and
/// group around each. They arrive with their positions only.
pub(super) fn place(files: &CheckoutFiles<'_>, root: &str, rows: &mut [DeadRow]) {
    let relative = |path: &str| -> String {
        path.strip_prefix(root)
            .map(|rest| rest.trim_start_matches('/').to_string())
            .unwrap_or_else(|| path.to_string())
    };
    let mut shapes: HashMap<String, Option<Shape>> = HashMap::new();
    for row in rows.iter_mut() {
        let Detail::Method(MethodDetail {
            test_sites: Some(sites),
            ..
        }) = &mut row.detail
        else {
            continue;
        };
        for site in sites.iter_mut() {
            let path = relative(&site.path);
            let shape = shapes.entry(path.clone()).or_insert_with(|| {
                let facts = files.facts(&path)?;
                let source = facts.source.clone()?;
                Some(Shape::read(&source))
            });
            let Some(shape) = shape else {
                continue;
            };
            let at = Pos {
                line: site.line,
                col: site.col,
            };
            let around = shape.around(at);
            let innermost = |group: bool| {
                around
                    .iter()
                    .rev()
                    .find(|s| s.group == group)
                    .map(|s| s.block())
            };
            site.example = innermost(false);
            site.group = innermost(true);
        }
    }
}

/// A test file's examples and groups, where each starts and ends.
struct Shape {
    spans: Vec<Span>,
}

struct Span {
    /// A group, or a shared group's body; else an example.
    group: bool,
    name: String,
    start: Pos,
    end: Pos,
}

impl Span {
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
        }
        impl Walk<'_> {
            fn span(&mut self, group: bool, name: String, loc: &ruby_prism::Location<'_>) {
                let (from, to) = (loc.start_offset(), loc.end_offset());
                self.spans.push(Span {
                    group,
                    name,
                    start: self.lines.pos(from),
                    end: self.lines.pos(to.saturating_sub(1)),
                });
            }

            /// No receiver, or `RSpec`: what opens a group or an example.
            fn on_rspec(&self, call: &ruby_prism::CallNode<'_>) -> bool {
                call.receiver().is_none_or(|r| {
                    let loc = r.location();
                    let text = &self.source[loc.start_offset()..loc.end_offset()];
                    text == b"RSpec" || text == b"::RSpec"
                })
            }
        }
        impl<'pr> Visit<'pr> for Walk<'_> {
            fn visit_call_node(&mut self, call: &ruby_prism::CallNode<'pr>) {
                let name = String::from_utf8_lossy(call.name().as_slice()).into_owned();
                let group = if !self.on_rspec(call)
                    || call.block().and_then(|b| b.as_block_node()).is_none()
                {
                    None
                } else if GROUPS.contains(&name.as_str()) || SHARED.contains(&name.as_str()) {
                    Some(true)
                } else if call.receiver().is_none() && EXAMPLES.contains(&name.as_str()) {
                    Some(false)
                } else {
                    None
                };
                if let Some(group) = group {
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
                    self.span(group, described, &call.location());
                }
                ruby_prism::visit_call_node(self, call);
            }

            fn visit_def_node(&mut self, def: &ruby_prism::DefNode<'pr>) {
                let name = String::from_utf8_lossy(def.name().as_slice()).into_owned();
                if def.receiver().is_none() && name.starts_with("test_") {
                    self.span(false, name, &def.location());
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
        };
        walk.visit(&parsed.node());
        Shape { spans: walk.spans }
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
}
