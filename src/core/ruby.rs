//! Ruby's own vocabulary, read by extraction and resolution alike: what a
//! block's `self` is by the call it is handed to, and which calls send a
//! method by its name. One table, so the next DSL that `instance_exec`s a
//! block is one row here rather than a row in each layer.

/// Run their block with their receiver as `self`.
const EVALUATES: [&str; 6] = [
    "instance_eval",
    "instance_exec",
    "class_eval",
    "class_exec",
    "module_eval",
    "module_exec",
];

/// Run the method whose name they are handed: `send(:x)`, and
/// ActiveSupport's `try(:x)`, which a `nil` receiver skips.
pub(crate) const SENDS: [&str; 5] = ["send", "public_send", "__send__", "try", "try!"];

/// Name a method of their receiver without running it.
const NAMES: [&str; 3] = ["method", "public_method", "respond_to?"];

/// Does this call's first symbol name a method of its receiver (DEC-093)?
pub(crate) fn names_a_method(name: &str) -> bool {
    SENDS.contains(&name) || NAMES.contains(&name)
}

/// The receiver a call is written with, as far as a block's `self` cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Recv<'a> {
    /// None, or a literal `self`.
    OnSelf,
    /// A constant, as written, without a leading `::`.
    Const(&'a str),
    /// Anything else.
    Other,
}

/// What `self` is in a block handed to a call, by the call's name and
/// receiver alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BlockSelf {
    /// `x.instance_eval`, `x.class_exec` and kin: the receiver. On `self`,
    /// that is `self` again, so the block keeps it.
    Receiver,
    /// `Class.new`, `Module.new`, `Struct.new`, `Data.define`: the class or
    /// module the call makes.
    Made,
    /// `define_method`, `define_singleton_method`: the object the method it
    /// defines is called on.
    Method,
    /// A class-level Rails callback that `instance_exec`s what it is handed
    /// (DEC-342, DEC-401): `before_action`, `after_commit`, `around_perform`,
    /// a model's own `define_model_callbacks`, `validate`, `rescue_from`. Its
    /// body is ActiveSupport's, which builds the call at runtime, so it is
    /// known by its name rather than read.
    Instance,
    /// Nothing the name says: whatever the method does with it.
    Unknown,
}

impl BlockSelf {
    /// Does the block run with a `self` other than the caller's own, by
    /// Ruby's own means: an evaluation on another object, or the body of a
    /// class it makes?
    pub(crate) fn elsewhere(self) -> bool {
        matches!(self, BlockSelf::Receiver | BlockSelf::Made)
    }
}

pub(crate) fn block_self(recv: Recv<'_>, name: &str) -> BlockSelf {
    match (recv, name) {
        (Recv::OnSelf, name) if EVALUATES.contains(&name) => BlockSelf::Unknown,
        (_, name) if EVALUATES.contains(&name) => BlockSelf::Receiver,
        (Recv::Const("Class" | "Module" | "Struct"), "new") | (Recv::Const("Data"), "define") => {
            BlockSelf::Made
        }
        (_, "define_method" | "define_singleton_method") => BlockSelf::Method,
        (Recv::OnSelf, name) if is_callback(name) => BlockSelf::Instance,
        _ => BlockSelf::Unknown,
    }
}

fn is_callback(name: &str) -> bool {
    matches!(name, "validate" | "rescue_from")
        || ["before_", "after_", "around_"]
            .iter()
            .any(|prefix| name.len() > prefix.len() && name.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blocks_self_is_read_from_the_call_it_is_handed_to() {
        use BlockSelf::*;
        let cases = [
            (Recv::Other, "instance_eval", Receiver),
            (Recv::Const("Widget"), "class_exec", Receiver),
            (Recv::OnSelf, "class_eval", Unknown),
            (Recv::Const("Struct"), "new", Made),
            (Recv::Const("Data"), "define", Made),
            (Recv::Const("Data"), "new", Unknown),
            (Recv::Const("Widget"), "new", Unknown),
            (Recv::OnSelf, "define_method", Method),
            (Recv::OnSelf, "before_save", Instance),
            (Recv::OnSelf, "rescue_from", Instance),
            (Recv::Other, "before_save", Unknown),
            (Recv::OnSelf, "before_", Unknown),
            (Recv::OnSelf, "each", Unknown),
        ];
        for (recv, name, expected) in cases {
            assert_eq!(block_self(recv, name), expected, "{recv:?} {name}");
        }
    }
}
