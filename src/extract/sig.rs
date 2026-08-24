//! Sorbet signatures, read as the ordinary Ruby they are.
//!
//! Lifted in shape from rwr's `src/sigs.rs`. A `sig` names a usable class for
//! 64% of signatures against 3.9% from syntax alone (PLAN §2) — the highest
//! yield per line of code anywhere in the ladder, and free on repos with no
//! Sorbet at all.

use ruby_prism::Node;

/// The class a `sig { ... }` says its method returns, if it names one.
///
/// Handles `sig { returns(X) }`, `sig { params(..).returns(X) }`, and
/// `sig(:final) { void }` — the whole family is one chain of calls, so walking
/// receivers inward covers it without enumerating the forms.
pub(super) fn returns(node: &Node<'_>) -> Option<String> {
    let call = node.as_call_node()?;
    if call.name().as_slice() != b"sig" {
        return None;
    }
    let body = call.block()?.as_block_node()?.body()?;
    let mut current = body.as_statements_node()?.body().iter().next()?;
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
