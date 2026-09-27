//! Sorbet signatures, read as the ordinary Ruby they are.
//!
//! Lifted in shape from rwr's `src/sigs.rs`. A `sig` names a usable class for
//! 64% of signatures against 3.9% from syntax alone (PLAN §2) — the highest
//! yield per line of code anywhere in the ladder, and free on repos with no
//! Sorbet at all.

use ruby_prism::Node;

/// The classes a `sig { params(...) }` gives the method's parameters.
///
/// The returns half of a signature has always been read; the params half is
/// worth at least as much and was not. Measured on graph_weaver: half of all
/// untyped local receivers are method *parameters*, which have no assignment to
/// chase and are invisible to every rung that looks for one.
pub(super) fn params(node: &Node<'_>) -> Vec<(String, String)> {
    let Some(chain) = sig_chain(node) else {
        return Vec::new();
    };
    let mut current = chain;
    loop {
        let Some(call) = current.as_call_node() else {
            return Vec::new();
        };
        if call.name().as_slice() == b"params" {
            return keyword_types(&call);
        }
        let Some(receiver) = call.receiver() else {
            return Vec::new();
        };
        current = receiver;
    }
}

/// `params(source: String, options: T::Hash[...])` — the pairs that name a
/// class. A parameter typed `T.untyped` contributes nothing and is dropped
/// rather than recorded as unknown.
fn keyword_types(call: &ruby_prism::CallNode<'_>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Some(arguments) = call.arguments() else {
        return out;
    };
    for argument in arguments.arguments().iter() {
        let Some(hash) = argument.as_keyword_hash_node() else {
            continue;
        };
        for element in hash.elements().iter() {
            let Some(assoc) = element.as_assoc_node() else {
                continue;
            };
            let Some(key) = assoc.key().as_symbol_node() else {
                continue;
            };
            let Ok(name) = String::from_utf8(key.unescaped().to_vec()) else {
                continue;
            };
            if let Some(class) = type_name(&assoc.value()) {
                out.push((name, class));
            }
        }
    }
    out
}

/// The block body of a `sig { ... }`, if this node is one.
fn sig_chain<'pr>(node: &Node<'pr>) -> Option<Node<'pr>> {
    let call = node.as_call_node()?;
    if call.name().as_slice() != b"sig" {
        return None;
    }
    let body = call.block()?.as_block_node()?.body()?;
    body.as_statements_node()?.body().iter().next()
}

/// The class a `sig { ... }` says its method returns, if it names one.
///
/// Handles `sig { returns(X) }`, `sig { params(..).returns(X) }`, and
/// `sig(:final) { void }` — the whole family is one chain of calls, so walking
/// receivers inward covers it without enumerating the forms.
pub(super) fn returns(node: &Node<'_>) -> Option<String> {
    let mut current = sig_chain(node)?;
    loop {
        let call = current.as_call_node()?;
        if call.name().as_slice() == b"returns"
            && let Some(arg) = call.arguments().and_then(|a| a.arguments().iter().next())
        {
            return type_name(&arg);
        }
        current = call.receiver()?;
    }
}

/// The class a Sorbet type expression denotes, or `None` when it denotes no
/// single class (`T.untyped`, `T.any(..)`, `void`).
fn type_name(node: &Node<'_>) -> Option<String> {
    if let Some(read) = node.as_constant_read_node() {
        return String::from_utf8(read.name().as_slice().to_vec()).ok();
    }
    if let Some(path) = node.as_constant_path_node() {
        // `A::B` denotes B, the same way a constant path resolves elsewhere.
        return String::from_utf8(path.name()?.as_slice().to_vec()).ok();
    }
    let call = node.as_call_node()?;
    let first = || call.arguments().and_then(|a| a.arguments().iter().next());
    match call.name().as_slice() {
        // `T::Array[String]` parses as `[]` called on the constant path.
        b"[]" => type_name(&call.receiver()?),
        b"nilable" => type_name(&first()?),
        _ => None,
    }
}

/// One `sig`, reduced to what tells overloads apart: the parameters it names
/// and what it returns.
pub(super) struct Shape {
    /// Each named parameter, and whether its type says a block is given
    /// (`T.proc…`), is not (`NilClass`), or neither.
    named: Vec<(String, Option<bool>)>,
    returns: Option<String>,
}

/// This statement as a `sig`, if it is one.
pub(super) fn shape(node: &Node<'_>) -> Option<Shape> {
    let chain = sig_chain(node)?;
    let mut named = Vec::new();
    let mut current = chain;
    while let Some(call) = current.as_call_node() {
        if call.name().as_slice() == b"params" {
            named = named_params(&call);
        }
        match call.receiver() {
            Some(receiver) => current = receiver,
            None => break,
        }
    }
    Some(Shape {
        named,
        returns: returns(node),
    })
}

fn named_params(call: &ruby_prism::CallNode<'_>) -> Vec<(String, Option<bool>)> {
    let Some(arguments) = call.arguments() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for argument in arguments.arguments().iter() {
        let Some(hash) = argument.as_keyword_hash_node() else {
            continue;
        };
        for element in hash.elements().iter() {
            let Some(assoc) = element.as_assoc_node() else {
                continue;
            };
            let Some(key) = assoc.key().as_symbol_node() else {
                continue;
            };
            let Ok(name) = String::from_utf8(key.unescaped().to_vec()) else {
                continue;
            };
            out.push((name, block_given(&assoc.value())));
        }
    }
    out
}

/// `T.proc.void` says a block is passed, `NilClass` that none is.
fn block_given(node: &Node<'_>) -> Option<bool> {
    if type_name(node).as_deref() == Some("NilClass") {
        return Some(false);
    }
    let mut current = node.as_call_node()?;
    loop {
        if current.name().as_slice() == b"proc" {
            let receiver = current.receiver()?;
            return (const_text(&receiver).as_deref() == Some("T")).then_some(true);
        }
        current = current.receiver()?.as_call_node()?;
    }
}

fn const_text(node: &Node<'_>) -> Option<String> {
    let read = node.as_constant_read_node()?;
    String::from_utf8(read.name().as_slice().to_vec()).ok()
}

/// A method's `sig`s as one return type for every call, or as overloads
/// when the call's shape decides (DEC-077).
///
/// One `sig` is Sorbet's ordinary case and holds for every call, unless it
/// names `NilClass` for the block. Several that agree are the same answer;
/// several that differ are overloads, told apart by how many positional
/// parameters each names (none: any count) and what it says of the block.
pub(super) fn resolve(
    sigs: &[Shape],
    params: &[crate::core::Param],
) -> (Option<String>, Vec<crate::core::Overload>) {
    use crate::core::ParamKind;
    let constrained = |s: &Shape| s.named.iter().any(|(_, block)| *block == Some(false));
    match sigs {
        [] => return (None, Vec::new()),
        [only] if !constrained(only) => return (only.returns.clone(), Vec::new()),
        [first, rest @ ..]
            if first.returns.is_some()
                && !constrained(first)
                && rest
                    .iter()
                    .all(|s| s.returns == first.returns && !constrained(s)) =>
        {
            return (first.returns.clone(), Vec::new());
        }
        _ => {}
    }
    let kind_of = |name: &str| params.iter().find(|p| p.name == name).map(|p| p.kind);
    let overloads = sigs
        .iter()
        .map(|s| {
            let positional = s
                .named
                .iter()
                .filter(|(name, _)| matches!(kind_of(name), Some(ParamKind::Req | ParamKind::Opt)))
                .count() as u32;
            let block = s
                .named
                .iter()
                .find(|(name, _)| kind_of(name) == Some(ParamKind::Block))
                .and_then(|(_, given)| *given);
            crate::core::Overload {
                argc: (positional > 0).then_some(positional),
                block,
                returns: s.returns.clone(),
            }
        })
        .collect();
    (None, overloads)
}

#[cfg(test)]
mod tests {
    use crate::core::Overload;

    fn overloads(source: &str) -> (Option<String>, Vec<Overload>) {
        let facts = crate::extract::extract(source.as_bytes());
        let def = facts.defs.iter().find(|d| d.name == "go").expect("a def");
        (def.sig_returns.clone(), def.sig_overloads.clone())
    }

    #[test]
    fn one_sig_holds_for_every_call() {
        let (returns, shapes) = overloads(
            "class W\n  sig { params(x: Integer).returns(String) }\n  def go(x); end\nend\n",
        );
        assert_eq!(returns.as_deref(), Some("String"));
        assert!(shapes.is_empty());
    }

    #[test]
    fn sigs_that_differ_are_told_apart_by_the_call() {
        let source = "class W\n  \
            sig { params(a: T.untyped, block: NilClass).returns(Enumerator) }\n  \
            sig { params(a: T.untyped, b: T.untyped, block: NilClass).returns(String) }\n  \
            sig { params(block: T.proc.void).returns(String) }\n  \
            def go(a, b = nil, &block); end\nend\n";
        let (returns, shapes) = overloads(source);
        assert_eq!(returns, None, "no one class holds for every call");
        let at = |argc, block| crate::core::returns_for(None, &shapes, argc, block);
        assert_eq!(at(Some(1), false), Some("Enumerator"));
        assert_eq!(at(Some(2), false), Some("String"));
        assert_eq!(at(Some(1), true), Some("String"));
        assert_eq!(at(None, false), None, "a splat could be either");
    }

    #[test]
    fn a_lone_sig_for_the_blockless_call_says_nothing_of_the_other() {
        let (returns, shapes) = overloads(
            "class W\n  sig { params(block: NilClass).returns(Enumerator) }\n  def go(&block); end\nend\n",
        );
        assert_eq!(returns, None);
        assert_eq!(crate::core::returns_for(None, &shapes, Some(0), true), None);
        assert_eq!(
            crate::core::returns_for(None, &shapes, Some(0), false),
            Some("Enumerator")
        );
    }

    #[test]
    fn sigs_that_agree_are_one_answer() {
        let (returns, shapes) = overloads(
            "class W\n  sig { returns(String) }\n  sig { params(block: T.proc.void).returns(String) }\n  def go(&block); end\nend\n",
        );
        assert_eq!(returns.as_deref(), Some("String"));
        assert!(shapes.is_empty());
    }
}
