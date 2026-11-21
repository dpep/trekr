//! The calls that run a method by a name they are handed (`ruby::BY_NAME`),
//! read once from the parse for every reader: `--dead`'s marker and caveat
//! for a file's sends (DEC-662), the shapes it sends a class (DEC-663), and
//! the symbols a send runs (DEC-664).

use super::{Extractor, receiver_shape};
use crate::core::ruby::BY_NAME;
use crate::core::{Dispatch, RecvShape, SentName, SentTo};
use ruby_prism::Node;

/// A local a method last assigned, and the conditionals open around it.
pub(super) struct HeldName {
    local: String,
    /// The shape of the interpolated name it holds, `None` once replaced.
    shape: Option<String>,
    /// The names written where they may not run, which it may hold
    /// instead of its shape: a string, or another shape, and where.
    beside: Vec<(SentName, crate::core::Pos)>,
    /// Written `nil`: no name a later write leaves standing.
    nothing: bool,
    conditional: usize,
}

impl Extractor<'_> {
    /// The sends a call makes. `method` and `public_method` answer a
    /// `Method`, which runs nothing until it is called: they count where
    /// it is, by `.call`, `.to_proc` or `&`.
    pub(super) fn record_send(&mut self, call: &ruby_prism::CallNode<'_>) {
        fn handle<'pr>(node: Option<Node<'pr>>) -> Option<ruby_prism::CallNode<'pr>> {
            node.and_then(|n| n.as_call_node())
                .filter(|c| matches!(c.name().as_slice(), b"method" | b"public_method"))
        }
        match call.name().as_slice() {
            b"method" | b"public_method" => self.push_send(call, false),
            b"call" | b"to_proc" => {
                if let Some(handle) = handle(call.receiver()) {
                    self.push_send(&handle, true);
                }
            }
            _ => self.push_send(call, true),
        }
        let passed = call
            .block()
            .and_then(|b| b.as_block_argument_node())
            .and_then(|b| b.expression());
        if let Some(handle) = handle(passed) {
            self.push_send(&handle, true);
        }
    }

    /// What a call in `BY_NAME` hands as the name: its first argument, or
    /// the next when the first names another send (`send(:public_send, x)`).
    fn push_send(&mut self, call: &ruby_prism::CallNode<'_>, run: bool) {
        let Some(by) = BY_NAME
            .iter()
            .find(|k| k.as_bytes() == call.name().as_slice())
        else {
            return;
        };
        // A `method(x)` handle is one send, run or not, however it is
        // visited; at its name, since a chain's calls all start together.
        let named = call.message_loc().unwrap_or_else(|| call.location());
        let call_at = self.pos(named.start_offset());
        if by.ends_with("method")
            && let Some(known) = self
                .facts
                .sends
                .iter_mut()
                .find(|s| s.by == *by && s.call == call_at)
        {
            known.run |= run;
            return;
        }
        let Some(arguments) = call.arguments() else {
            return;
        };
        let mut arguments = arguments.arguments().iter();
        let mut first = arguments.next();
        let mut relayed = None;
        // `send(:method, x)` answers a `Method`, which runs nothing.
        while let Some(text) = first.as_ref().and_then(literal_text)
            && let Some(by) = BY_NAME.iter().find(|by| **by == text)
            && !text.ends_with("method")
        {
            first = arguments.next();
            relayed = Some(*by);
        }
        let Some(first) = first else {
            return;
        };
        // A constant handed to `try` is the error class a service DSL's
        // `try` rescues, not a name.
        if by.starts_with("try") && error_class(&first) {
            return;
        }
        let at = first
            .as_symbol_node()
            .and_then(|symbol| symbol.value_loc())
            .unwrap_or_else(|| first.location());
        let send = Dispatch {
            by,
            name: self.sent_name(&first),
            held: first.as_local_variable_read_node().is_some(),
            to: sent_to(call),
            relayed,
            run,
            call: call_at,
            nesting: self.nesting.clone(),
            pos: self.pos(at.start_offset()),
        };
        // A name the local may hold instead of its shape is sent too, cited
        // where it is written.
        let beside = first
            .as_local_variable_read_node()
            .and_then(|read| {
                let local = String::from_utf8_lossy(read.name().as_slice());
                let held = &self.frames.last()?.held;
                Some(held.iter().rev().find(|h| h.local == local)?.beside.clone())
            })
            .unwrap_or_default();
        for (name, pos) in beside {
            self.facts.sends.push(Dispatch {
                name,
                pos,
                ..send.clone()
            });
        }
        self.facts.sends.push(send);
    }

    fn sent_name(&self, node: &Node<'_>) -> SentName {
        // A literal in a string of code that an interpolation spells part
        // of (`"list_#{kind}"` in a `class_eval <<~RUBY`) is computed.
        let at = node.location();
        let literal = node.as_symbol_node().is_some() || node.as_string_node().is_some();
        if literal && self.written(at.start_offset(), at.end_offset()).is_some() {
            return SentName::Computed;
        }
        if let Some(symbol) = node.as_symbol_node() {
            return SentName::Symbol(String::from_utf8_lossy(symbol.unescaped()).into_owned());
        }
        if let Some(string) = node.as_string_node() {
            return SentName::String(String::from_utf8_lossy(string.unescaped()).into_owned());
        }
        if let Some(shape) = held_shape(node) {
            return shape.map_or(SentName::Computed, SentName::Shape);
        }
        // A local the method last assigned an interpolated name.
        if let Some(read) = node.as_local_variable_read_node() {
            let local = String::from_utf8_lossy(read.name().as_slice());
            let held = self.frames.last().map(|f| &f.held[..]).unwrap_or_default();
            if let Some(shape) = held.iter().rev().find(|h| h.local == local) {
                return shape
                    .shape
                    .clone()
                    .map_or(SentName::Computed, SentName::Shape);
            }
        }
        SentName::Computed
    }

    /// A string may be code evaluated at runtime, built in ways no parse
    /// here follows: each send its text spells.
    /// Its computed names are read from its own parse, as far as one goes,
    /// for the constants they are sent to (`eval("Knob.public_send(k)")`).
    pub(super) fn spell_sends(&mut self, text: &[u8], at: usize) {
        let mut spelled = false;
        for by in sends_in_text(text) {
            spelled = true;
            if !self.facts.spelled_sends.contains(&by) {
                self.facts.spelled_sends.push(by);
            }
        }
        thread_local!(static SPELLING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });
        // One level: a string's own strings are not parsed again.
        if !spelled || SPELLING.get() {
            return;
        }
        SPELLING.set(true);
        let sends = super::extract(text).sends;
        SPELLING.set(false);
        let line = self.pos(at).line;
        let rebase = |pos: crate::core::Pos| crate::core::Pos {
            line: line + pos.line - 1,
            col: pos.col,
        };
        for send in sends {
            // A literal name in a string is no call the index reads.
            if matches!(send.name, SentName::Symbol(_) | SentName::String(_)) {
                continue;
            }
            self.facts.sends.push(Dispatch {
                call: rebase(send.call),
                pos: rebase(send.pos),
                nesting: self.nesting.clone(),
                ..send
            });
        }
    }

    /// Records what a method assigns a local: an interpolated name is held;
    /// anything else replaces it, unless written where the name was not and
    /// which may not run (DEC-655, DEC-666).
    pub(super) fn hold(&mut self, local: &str, value: Option<&Node<'_>>) {
        let conditional = self.uncertain();
        let shape = value.and_then(|value| held_shape(value).flatten());
        let nil = value.is_some_and(|v| v.as_nil_node().is_some());
        // A string is one more name the local may hold beside a shape (a
        // symbol is read where it is written); `nil` is none; anything else
        // may be any name.
        let string = value.and_then(|v| {
            let name = String::from_utf8_lossy(v.as_string_node()?.unescaped()).into_owned();
            Some((
                SentName::String(name),
                self.pos(v.location().start_offset()),
            ))
        });
        let unknown = shape.is_none()
            && !nil
            && value.is_none_or(|v| v.as_string_node().is_none() && v.as_symbol_node().is_none());
        let at = value.map(|v| self.pos(v.location().start_offset()));
        let held = &mut self.frame().held;
        let last = held.iter_mut().rev().find(|h| h.local == local);
        // `nil` over nothing holds nothing a later write must displace.
        if last.is_none() && nil {
            return;
        }
        // A write where it may not run leaves what was there standing: a
        // shape beside an unknown name is as good as computed.
        let replaces = last.as_ref().is_none_or(|h| {
            h.nothing || conditional <= h.conditional || (unknown && h.shape.is_some())
        });
        if replaces {
            held.push(HeldName {
                local: local.to_string(),
                shape,
                beside: Vec::new(),
                nothing: nil,
                conditional,
            });
        } else if let Some(last) = last
            && last.shape.is_some()
        {
            match (shape, at) {
                (Some(shape), Some(at)) => last.beside.push((SentName::Shape(shape), at)),
                _ => last.beside.extend(string),
            }
        }
    }

    /// A method's parameters hold whatever names its callers pass.
    pub(super) fn hold_params(&mut self, names: impl Iterator<Item = String>) {
        let conditional = self.uncertain();
        let held = &mut self.frame().held;
        held.extend(names.map(|local| HeldName {
            local,
            shape: None,
            beside: Vec::new(),
            nothing: false,
            conditional,
        }));
    }

    /// How deep in code that may not run a write is: a conditional, a
    /// loop or pattern match, or a block, which may never be called.
    fn uncertain(&mut self) -> usize {
        self.conditional + self.looping + self.frame().blocks
    }

    /// A block's own locals (its parameters, and what it first assigns)
    /// hide a method's locals of their names until it ends.
    pub(super) fn shadow(&mut self, locals: &ruby_prism::ConstantList<'_>) -> (usize, Vec<String>) {
        let conditional = self.uncertain();
        let names: Vec<String> = locals
            .iter()
            .map(|name| String::from_utf8_lossy(name.as_slice()).into_owned())
            .collect();
        let held = &mut self.frame().held;
        let start = held.len();
        held.extend(names.iter().map(|local| HeldName {
            local: local.clone(),
            shape: None,
            beside: Vec::new(),
            nothing: false,
            conditional,
        }));
        (start, names)
    }

    pub(super) fn unshadow(&mut self, (start, names): (usize, Vec<String>)) {
        let mut at = 0;
        self.frame().held.retain(|h| {
            at += 1;
            at <= start || !names.contains(&h.local)
        });
    }
}

/// An interpolated string or symbol, or its `.to_sym`: `Some` of its shape,
/// or of `None` when its literal text is no name's.
fn held_shape(node: &Node<'_>) -> Option<Option<String>> {
    if let Some(call) = node.as_call_node()
        && call.name().as_slice() == b"to_sym"
        && call.arguments().is_none()
    {
        return held_shape(&call.receiver()?);
    }
    let parts = match (
        node.as_interpolated_string_node(),
        node.as_interpolated_symbol_node(),
    ) {
        (Some(string), _) => string.parts(),
        (_, Some(symbol)) => symbol.parts(),
        _ => return None,
    };
    let mut shape = String::new();
    let mut named = true;
    for part in parts.iter() {
        match part.as_string_node() {
            Some(text) => {
                let text = text.unescaped();
                named &= text
                    .iter()
                    .all(|b| b.is_ascii_alphanumeric() || b"_?!=".contains(b));
                shape.push_str(&String::from_utf8_lossy(text));
            }
            None if !shape.ends_with('*') => shape.push('*'),
            None => {}
        }
    }
    Some((named && shape.contains('*')).then_some(shape))
}

fn literal_text(node: &Node<'_>) -> Option<String> {
    let text = match (node.as_symbol_node(), node.as_string_node()) {
        (Some(symbol), _) => String::from_utf8_lossy(symbol.unescaped()).into_owned(),
        (_, Some(string)) => String::from_utf8_lossy(string.unescaped()).into_owned(),
        _ => return None,
    };
    Some(text)
}

fn error_class(node: &Node<'_>) -> bool {
    let constant =
        |n: &Node<'_>| n.as_constant_read_node().is_some() || n.as_constant_path_node().is_some();
    constant(node)
        || node
            .as_splat_node()
            .and_then(|splat| splat.expression())
            .is_some_and(|splatted| constant(&splatted))
}

/// What a send is written to. `new` answers an instance and `with` is
/// ActionMailer's, whose answer runs the mailer's actions; any other call
/// may answer anything (`Widget.for(1)`).
fn sent_to(call: &ruby_prism::CallNode<'_>) -> SentTo {
    let Some(mut node) = call.receiver() else {
        return SentTo::Own;
    };
    let mut through = Vec::new();
    loop {
        let only = |known: &[&[u8]]| through.iter().all(|c: &Vec<u8>| known.contains(&&c[..]));
        if node.as_self_node().is_some() {
            return match only(&[b"class", b"new"]) {
                true => SentTo::Own,
                false => SentTo::Value,
            };
        }
        if let Some(inner) = node.as_call_node() {
            let name = inner.name().as_slice().to_vec();
            let Some(receiver) = inner.receiver() else {
                return match name == b"new" && only(&[b"class", b"new"]) {
                    true => SentTo::Own,
                    false => SentTo::Value,
                };
            };
            through.push(name);
            node = receiver;
            continue;
        }
        return match receiver_shape(&node) {
            (RecvShape::Const, Some(written)) if only(&[b"new", b"with"]) => {
                let written = written.trim_start_matches("::").to_string();
                match through.is_empty() {
                    true => SentTo::Constant(written),
                    false => SentTo::Made(written),
                }
            }
            _ => SentTo::Value,
        };
    }
}

/// The calls in `BY_NAME` a string's text spells as called: `send(`, or
/// `.send ` and `__send__ ` without parentheses; `method` only where it is
/// run, as the parse counts it.
pub(crate) fn sends_in_text(text: &[u8]) -> impl Iterator<Item = &'static str> + '_ {
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let contains = |needle: &[u8]| text.windows(needle.len()).any(|w| w == needle);
    let run = contains(b".call") || contains(b".to_proc");
    BY_NAME.iter().copied().filter(move |name| {
        let name = name.as_bytes();
        (0..text.len().saturating_sub(name.len() - 1)).any(|at| {
            if &text[at..at + name.len()] != name {
                return false;
            }
            let before = at.checked_sub(1).map(|b| text[b]);
            let after = text.get(at + name.len()).copied();
            if before.is_some_and(ident) {
                return false;
            }
            let opens = matches!(
                before,
                None | Some(b' ' | b'\t' | b'\n' | b';' | b'(' | b'=' | b'{' | b'|')
            );
            // `send ` and `try ` after a space are English as often as code.
            let unworded = opens && (name == b"__send__" || name == b"public_send");
            let called =
                after == Some(b'(') || (after == Some(b' ') && (before == Some(b'.') || unworded));
            match name.ends_with(b"method") {
                true => called && (run || before == Some(b'&')),
                false => called,
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sends a file makes: each name and what it is sent to.
    fn sends(source: &str) -> Vec<(&'static str, SentName, SentTo)> {
        let facts = super::super::extract(source.as_bytes());
        facts
            .sends
            .into_iter()
            .map(|s| (s.by, s.name, s.to))
            .collect()
    }

    fn shape(text: &str) -> SentName {
        SentName::Shape(text.to_string())
    }

    #[test]
    fn a_send_is_read_with_its_name_and_receiver() {
        use SentName::{Computed, String as Str, Symbol};
        use SentTo::{Constant, Made, Own, Value};
        let constant = |c: &str| Constant(c.to_string());
        let made = |c: &str| Made(c.to_string());
        for (source, said) in [
            ("send(:x)", vec![("send", Symbol("x".into()), Own)]),
            ("record.try(\"x\")", vec![("try", Str("x".into()), Value)]),
            ("send(name)", vec![("send", Computed, Own)]),
            (
                r##"Q.new(u).public_send("list_#{f}", u)"##,
                vec![("public_send", shape("list_*"), made("Q"))],
            ),
            (
                r##"::Mail::Admin.with(to: x).public_send(:"#{k}_attrs")"##,
                vec![("public_send", shape("*_attrs"), made("Mail::Admin"))],
            ),
            (
                r##"self.class.send("to_#{k}")"##,
                vec![("send", shape("to_*"), Own)],
            ),
            (
                r##"new(r).send("to_#{k}")"##,
                vec![("send", shape("to_*"), Own)],
            ),
            (
                r##"Widget.for(1).send("to_#{k}")"##,
                vec![("send", shape("to_*"), Value)],
            ),
            (r##"self.helper.send(k)"##, vec![("send", Computed, Value)]),
            (
                r##"__send__ "#{k}_attrs".to_sym, k"##,
                vec![("__send__", shape("*_attrs"), Own)],
            ),
            // A name of no method's text is computed.
            (r##"send("a b #{c}")"##, vec![("send", Computed, Own)]),
            // Handed on by a send of a send.
            ("send(:public_send, name)", vec![("send", Computed, Own)]),
            // `method` counts where it is run.
            (
                r##"method(:"#{k}_attrs").call"##,
                vec![("method", shape("*_attrs"), Own)],
            ),
            (
                "list.map(&public_method(:x))",
                vec![("public_method", Symbol("x".into()), Own)],
            ),
            // Recorded unrun: its shape may still be sent later.
            (
                "Report.method(n).source_location",
                vec![("method", Computed, constant("Report"))],
            ),
            (
                "send(:method, :x)",
                vec![("send", Symbol("method".into()), Own)],
            ),
            // A constant handed to `try` is an error class.
            ("try(ActiveRecord::RecordInvalid) { }; try(*ERRORS)", vec![]),
            // Split over lines.
            (
                "Q.new\n  .public_send(\n    \"list_#{f}\")",
                vec![("public_send", shape("list_*"), made("Q"))],
            ),
        ] {
            assert_eq!(sends(source), said, "{source}");
        }
    }

    #[test]
    fn a_local_is_sent_as_its_method_last_assigned_it() {
        let held = |body: &str| sends(&format!("class W\n  def a(x, v)\n{body}  end\nend\n"));
        let sent = vec![("send", shape("handle_*"), SentTo::Own)];
        assert_eq!(held("    m = \"handle_#{x}\"; send(m, v)\n"), sent);
        assert_eq!(held("    m = :\"handle_#{x}\"\n    send(m)\n"), sent);
        assert_eq!(held("    m = \"handle_#{x}\".to_sym\n    send(m)\n"), sent);
        // Replaced before the send: not the name any more.
        assert_eq!(
            held("    m = \"handle_#{x}\"\n    m = \"other\"\n    send(m)\n"),
            vec![("send", SentName::Computed, SentTo::Own)]
        );
        // Replaced by a multiple assignment or in place: not the name.
        for replaced in ["    m, n = x\n", "    m += x\n"] {
            let body = format!("    m = \"handle_#{{x}}\"\n{replaced}    send(m)\n");
            assert_eq!(
                held(&body),
                vec![("send", SentName::Computed, SentTo::Own)],
                "{body}"
            );
        }
        // Replaced only on a branch, or under a modifier: may still be, and
        // the literal is sent too.
        for replaced in [
            "    if x\n      m = \"other\"\n    end\n",
            "    m = \"other\" if x\n",
        ] {
            let body = format!("    m = \"handle_#{{x}}\"\n{replaced}    send(m)\n");
            let both = vec![
                ("send", SentName::String("other".into()), SentTo::Own),
                ("send", shape("handle_*"), SentTo::Own),
            ];
            assert_eq!(held(&body), both, "{body}");
        }
        // Another method's local of the same name is another local.
        assert_eq!(
            sends(
                "class W\n  def a(x)\n    m = \"handle_#{x}\"\n  end\n  def b(m) = send(m)\nend\n"
            ),
            vec![("send", SentName::Computed, SentTo::Own)]
        );
    }

    #[test]
    fn a_send_a_string_spells_is_named() {
        for (text, said) in [
            ("def go = send(:x)", vec!["send"]),
            ("o.public_send \\#{k}", vec!["public_send"]),
            ("x.try!(y)", vec!["try!"]),
            ("please send it; try again; resend(x)", vec![]),
            ("  __send__ n", vec!["__send__"]),
            ("public_send", vec![]),
            ("the payment method(s)", vec![]),
            ("helpers.method(name).call()", vec!["method"]),
            ("map(&method(:x))", vec!["method"]),
        ] {
            assert_eq!(
                sends_in_text(text.as_bytes()).collect::<Vec<_>>(),
                said,
                "{text}"
            );
        }
    }
}
