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
