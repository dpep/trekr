//! A stub's `sig`s: one per call shape, only where every overload that
//! covers the shape agrees on one class (DEC-077). Ported from the generator
//! it replaces (`core_sigs.rb`), rule for rule.

use super::env::{Method, Returns};
use super::params::{Param, ParamKind};
use std::collections::HashSet;

/// How many positional arguments a call passes: a count, or "past the
/// longest finite form", where every count behaves like the last.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Argc {
    Count(usize),
    Rest,
}

/// A call shape: whether a block is given, and how many arguments.
type Shape = (bool, Argc);

/// Each shape → every covering overload's class, in the order the overloads
/// cover them. `None` is an overload that returns no class.
struct Table {
    cells: Vec<(Shape, Vec<Option<String>>)>,
}

impl Table {
    fn push(&mut self, key: Shape, returns: Option<String>) {
        match self.cells.iter_mut().find(|(k, _)| *k == key) {
            Some((_, list)) => list.push(returns),
            None => self.cells.push((key, vec![returns])),
        }
    }

    /// Each cell's one answer: the class every overload agrees on, or
    /// `None` where they disagree or say none.
    fn agreed(self) -> Vec<(Shape, Option<String>)> {
        self.cells
            .into_iter()
            .map(|(key, returns)| {
                let first = returns[0].clone();
                let one = returns.iter().all(|r| *r == first);
                (key, if one { first } else { None })
            })
            .collect()
    }
}

fn return_table(method: &Method, known: &HashSet<String>) -> Vec<(Shape, Option<String>)> {
    let overloads: Vec<_> = method
        .overloads
        .iter()
        .filter_map(|o| Some((o.function.as_ref()?, o)))
        .collect();
    let ceiling = overloads
        .iter()
        .map(|(f, _)| f.required.len() + f.optional.len())
        .max()
        .unwrap_or(0)
        + 1;
    let mut table = Table { cells: Vec::new() };
    for (f, overload) in overloads {
        let low = f.required.len() + f.trailing.len();
        let high = if f.rest.is_some() {
            ceiling
        } else {
            low + f.optional.len()
        };
        let blocks: &[bool] = match overload.block {
            None => &[false],
            Some(true) => &[true],
            Some(false) => &[false, true],
        };
        let returns = match &overload.returns {
            Returns::Class(name) if known.contains(name) => Some(name.clone()),
            _ => None,
        };
        for &block in blocks {
            for argc in low..=high {
                table.push((block, Argc::Count(argc)), returns.clone());
            }
        }
        if f.rest.is_some() {
            for &block in blocks {
                table.push((block, Argc::Rest), returns.clone());
            }
        }
    }
    table.agreed()
}

/// One block state's sigs as `(count, class)` pairs, a count of `None`
/// meaning every count.
#[derive(Clone, Debug, PartialEq)]
enum State {
    /// No overload covers the state at all.
    Absent,
    /// It cannot be said.
    Unsayable,
    /// RBS types it only at some counts: these.
    Partial(Vec<(usize, String)>),
    Said(Vec<(Option<usize>, String)>),
}

fn shape_sigs(cells: &[(Shape, Option<String>)], block: bool, positional: usize) -> State {
    let cells: Vec<(Argc, &Option<String>)> = cells
        .iter()
        .filter(|((given, _), _)| *given == block)
        .map(|((_, argc), returns)| (*argc, returns))
        .collect();
    if cells.is_empty() {
        return State::Absent;
    }
    // A shape RBS cannot type leaves only the counts that can be named: a
    // sig naming none, or the rest, would read as covering it.
    if cells.iter().any(|(_, r)| r.is_none()) {
        return State::Partial(
            cells
                .iter()
                .filter_map(|(argc, returns)| match (argc, returns) {
                    (Argc::Count(n), Some(class)) if (1..=positional).contains(n) => {
                        Some((*n, class.clone()))
                    }
                    _ => None,
                })
                .collect(),
        );
    }
    let mut values: Vec<&String> = cells.iter().filter_map(|(_, r)| r.as_ref()).collect();
    values.dedup();
    let first = values[0];
    if values.iter().all(|v| *v == first) {
        return State::Said(vec![(None, first.clone())]);
    }
    // Per count, which needs the counts to be finite and nameable, and a
    // zero count to have nothing to say: a `sig` naming no positional
    // parameter means "any count".
    if cells.iter().any(|(argc, _)| *argc == Argc::Rest)
        || cells.iter().any(|(argc, _)| *argc == Argc::Count(0))
    {
        return State::Unsayable;
    }
    let mut said = Vec::new();
    for (argc, returns) in cells {
        let Argc::Count(n) = argc else {
            return State::Unsayable;
        };
        if n > positional {
            return State::Unsayable;
        }
        said.push((
            Some(n),
            returns.clone().expect("every cell says a class here"),
        ));
    }
    State::Said(said)
}

/// The `sig` lines for a stub `def` with these parameters. Returns are
/// written from the top (`::String`): a stub nests its owners, and a
/// nesting would find `Psych::Set` for `Set`.
pub(crate) fn sigs(method: &Method, known: &HashSet<String>, params: &[Param]) -> Vec<String> {
    let table = return_table(method, known);
    if table.is_empty() {
        return Vec::new();
    }
    let positional: Vec<&str> = params
        .iter()
        .filter(|p| matches!(p.kind, ParamKind::Req | ParamKind::Opt))
        .map(|p| p.name.as_str())
        .collect();
    let block_param = params
        .iter()
        .find(|p| p.kind == ParamKind::Block)
        .map(|p| p.name.as_str());
    let without = shape_sigs(&table, false, positional.len());
    let with = shape_sigs(&table, true, positional.len());

    let render = |argc: Option<usize>, returns: &str, block: Option<bool>| {
        let mut names: Vec<String> = argc
            .map(|n| {
                positional[..n]
                    .iter()
                    .map(|name| format!("{name}: T.untyped"))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(block) = block {
            let kind = if block { "T.proc.void" } else { "NilClass" };
            names.push(format!("{}: {kind}", block_param.unwrap_or_default()));
        }
        if names.is_empty() {
            format!("sig {{ returns(::{returns}) }}")
        } else {
            format!(
                "sig {{ params({}).returns(::{returns}) }}",
                names.join(", ")
            )
        }
    };
    // A block changes nothing — or the method takes none, and Ruby ignores it.
    if with == State::Absent || with == without {
        return match without {
            State::Said(list) => list
                .iter()
                .map(|(argc, returns)| render(*argc, returns, None))
                .collect(),
            _ => Vec::new(),
        };
    }
    // Only `block: NilClass` confines a sig to its block state and count; a
    // lone `T.proc` one is Sorbet's ordinary sig and would cover every call.
    // So a partial state is said only beside a blockless sig that confines
    // the whole set to overloads.
    if without == State::Unsayable {
        return Vec::new();
    }
    let shapes = |state: &State| -> Option<Vec<(Option<usize>, String)>> {
        match state {
            State::Partial(list) => Some(list.iter().map(|(n, c)| (Some(*n), c.clone())).collect()),
            State::Said(list) => Some(list.clone()),
            _ => None,
        }
    };
    let partial = matches!(without, State::Partial(_)) || matches!(with, State::Partial(_));
    let confined = block_param.is_some() && shapes(&without).is_some_and(|list| !list.is_empty());
    if partial && !confined {
        return Vec::new();
    }
    [(&without, false), (&with, true)]
        .into_iter()
        .flat_map(|(state, block)| {
            shapes(state)
                .unwrap_or_default()
                .into_iter()
                .map(move |(argc, returns)| (argc, returns, block))
        })
        .map(|(argc, returns, block)| render(argc, &returns, Some(block)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rbs::env::{Env, Source};
    use crate::rbs::{params, parse};

    fn sigs_of(rbs: &str, name: &str) -> Vec<String> {
        let env = Env::build(&[Source {
            library: None,
            parsed: parse::parse(rbs),
        }]);
        let method = env.classes["W"]
            .methods
            .values()
            .find(|m| m.name == name)
            .unwrap()
            .clone();
        let known: HashSet<String> = ["String", "Array", "Enumerator", "Integer"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        sigs(&method, &known, &params::for_method(&method))
    }

    #[test]
    fn one_class_for_every_call_is_one_sig() {
        assert_eq!(
            sigs_of(
                "class W\n  def go: () -> String\n       | (Integer) -> String\nend\n",
                "go"
            ),
            ["sig { returns(::String) }"]
        );
    }

    #[test]
    fn a_block_that_changes_the_return_is_two_overloads() {
        assert_eq!(
            sigs_of(
                "class W\n  def map: () -> Enumerator[untyped, untyped]\n        | () { (untyped) -> untyped } -> Array[untyped]\nend\n",
                "map"
            ),
            [
                "sig { params(block: NilClass).returns(::Enumerator) }",
                "sig { params(block: T.proc.void).returns(::Array) }"
            ]
        );
    }

    #[test]
    fn a_union_an_optional_or_self_says_nothing() {
        for ret in ["String?", "(String | Integer)", "self", "bool", "Elem"] {
            let rbs = format!("class W\n  def go: () -> {ret}\nend\n");
            assert!(sigs_of(&rbs, "go").is_empty(), "{ret}");
        }
    }

    #[test]
    fn counts_only_some_overloads_type_are_left_unsaid() {
        // `first` is an element without a count and an Array with one.
        assert!(
            sigs_of(
                "class W[Elem]\n  def first: () -> Elem\n         | (Integer n) -> Array[Elem]\nend\n",
                "first"
            )
            .is_empty()
        );
    }

    #[test]
    fn a_class_nobody_knows_is_never_a_return() {
        assert!(sigs_of("class W\n  def go: () -> Pathname\nend\n", "go").is_empty());
    }
}
