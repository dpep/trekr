//! A RABL template's object, and the names it serializes (DEC-526).
//!
//! RABL is Ruby read on a `Rabl::Engine`: `object @post` (or `collection
//! @posts`) says what the template serializes, `attributes :id, :title` and
//! `child(:comments) { … }` name methods of that object, and `node(:x) { |p|
//! … }` hands the object to its block. Read from the template's source, since
//! which object a symbol names a method of is the nesting of the calls
//! around it, which facts keep only as positions.

use super::Receiver;
use crate::core::{Call, Facts, Pos, RecvShape};
use crate::extract::LineIndex;
use crate::tree::Tree;
use ruby_prism::Visit;
use std::collections::HashMap;

/// The object a part of a template serializes.
#[derive(Clone, Debug, PartialEq)]
enum Object {
    /// `object @post`, or `child(@author)`.
    Value(String),
    /// `collection @posts`: each element.
    Each(String),
    /// `child(:comments)` in the scope of another: what that one's reader
    /// returns, an element of it for a collection.
    Child(Box<Object>, String),
    /// A template that names no object of its own serializes the object of
    /// the template that `extends` it, where it does.
    Extender,
}

/// What a template's calls say of its objects.
#[derive(Default)]
struct Read {
    /// Where each name `attributes`, `attribute`, `child` or `glue` is handed
    /// as a symbol, and the object it is a method of.
    symbols: Vec<(Pos, Object)>,
    /// Each `node` block's parameter, the lines the block spans, and the
    /// object it is handed.
    params: Vec<(String, (u32, u32), Object)>,
    /// Each template an `extends` or `partial` names, where, and the object
    /// of the scope it is written in.
    extends: Vec<(Pos, String, Object)>,
}

struct Reader<'a> {
    lines: &'a LineIndex,
    scopes: Vec<Option<Object>>,
    read: Read,
}

fn text(node: &ruby_prism::Node<'_>) -> String {
    String::from_utf8_lossy(node.location().as_slice()).into_owned()
}

fn symbol(node: &ruby_prism::Node<'_>) -> Option<(String, usize)> {
    let symbol = node.as_symbol_node()?;
    let at = symbol.value_loc()?.start_offset();
    Some((String::from_utf8_lossy(symbol.unescaped()).into_owned(), at))
}

impl Reader<'_> {
    fn current(&self) -> Option<Object> {
        self.scopes.last().cloned().flatten()
    }

    fn name(&mut self, node: &ruby_prism::Node<'_>, of: &Option<Object>) -> Option<String> {
        let (name, at) = symbol(node)?;
        if let Some(object) = of {
            self.read.symbols.push((self.lines.pos(at), object.clone()));
        }
        Some(name)
    }

    /// What `child`'s or `glue`'s first argument makes the object of its block.
    fn child(&mut self, arg: &ruby_prism::Node<'_>) -> Option<Object> {
        let parent = self.current();
        let target = match arg.as_keyword_hash_node() {
            Some(hash) => hash
                .elements()
                .iter()
                .next()?
                .as_assoc_node()
                .map(|assoc| assoc.key())?,
            None => match arg.as_hash_node() {
                Some(hash) => hash
                    .elements()
                    .iter()
                    .next()?
                    .as_assoc_node()
                    .map(|assoc| assoc.key())?,
                None => {
                    return self.object_of(arg, parent, false);
                }
            },
        };
        self.object_of(&target, parent, false)
    }

    fn object_of(
        &mut self,
        node: &ruby_prism::Node<'_>,
        parent: Option<Object>,
        collection: bool,
    ) -> Option<Object> {
        if node.as_instance_variable_read_node().is_some() {
            return Some(match collection {
                true => Object::Each(text(node)),
                false => Object::Value(text(node)),
            });
        }
        let name = self.name(node, &parent)?;
        Some(Object::Child(Box::new(parent?), name))
    }
}

impl<'pr> Visit<'pr> for Reader<'_> {
    fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
        if node.receiver().is_some() {
            return ruby_prism::visit_call_node(self, node);
        }
        let name = String::from_utf8_lossy(node.name().as_slice()).into_owned();
        let args: Vec<ruby_prism::Node<'pr>> = node
            .arguments()
            .map(|a| a.arguments().iter().collect())
            .unwrap_or_default();
        let current = self.current();
        let opened = match name.as_str() {
            "object" | "collection" if self.scopes.len() == 1 => {
                let collection = name == "collection";
                let root = args.first().and_then(|arg| {
                    let target = match arg.as_keyword_hash_node() {
                        Some(hash) => hash.elements().iter().next()?.as_assoc_node()?.key(),
                        None => return self.object_of(arg, None, collection),
                    };
                    self.object_of(&target, None, collection)
                });
                self.scopes[0] = root;
                None
            }
            "attributes" | "attribute" => {
                for arg in &args {
                    match arg.as_keyword_hash_node() {
                        Some(hash) => {
                            for element in hash.elements().iter() {
                                if let Some(assoc) = element.as_assoc_node() {
                                    self.name(&assoc.key(), &current);
                                }
                            }
                        }
                        None => {
                            self.name(arg, &current);
                        }
                    }
                }
                None
            }
            "child" | "glue" => Some(args.first().and_then(|arg| self.child(arg))),
            "extends" | "partial" => {
                if let (Some(arg), Some(object)) = (args.first(), current)
                    && let Some(string) = arg.as_string_node()
                {
                    let name = String::from_utf8_lossy(string.unescaped()).into_owned();
                    let at = self.lines.pos(arg.location().start_offset());
                    self.read.extends.push((at, name, object));
                }
                None
            }
            "node" => {
                if let (Some(block), Some(object)) =
                    (node.block().and_then(|b| b.as_block_node()), current)
                    && let Some(param) = block
                        .parameters()
                        .and_then(|p| p.as_block_parameters_node())
                        .and_then(|p| p.parameters())
                        .and_then(|p| p.requireds().iter().next())
                        .and_then(|p| p.as_required_parameter_node())
                {
                    let span = (
                        self.lines.pos(block.location().start_offset()).line,
                        self.lines.pos(block.location().end_offset()).line,
                    );
                    let name = String::from_utf8_lossy(param.name().as_slice()).into_owned();
                    self.read.params.push((name, span, object));
                }
                None
            }
            _ => None,
        };
        match opened {
            Some(object) => {
                if let Some(arguments) = node.arguments() {
                    self.visit_arguments_node(&arguments);
                }
                self.scopes.push(object);
                if let Some(block) = node.block() {
                    self.visit(&block);
                }
                self.scopes.pop();
            }
            None => ruby_prism::visit_call_node(self, node),
        }
    }
}

/// A template's reading, once per content: a symbol's object may be asked of
/// every template that `extends` it, for each symbol.
fn read(source: &[u8]) -> std::rc::Rc<Read> {
    thread_local! {
        static READ: std::cell::RefCell<HashMap<Vec<u8>, std::rc::Rc<Read>>> =
            std::cell::RefCell::new(HashMap::new());
    }
    if let Some(read) = READ.with(|cache| cache.borrow().get(source).cloned()) {
        return read;
    }
    let read = std::rc::Rc::new(read_fresh(source));
    READ.with(|cache| {
        let mut cache = cache.borrow_mut();
        // Bounded: a process answering a whole checkout reads each once.
        if cache.len() > 4096 {
            cache.clear();
        }
        cache.insert(source.to_vec(), read.clone());
    });
    read
}

fn read_fresh(source: &[u8]) -> Read {
    let parsed = ruby_prism::parse(source);
    let lines = LineIndex::new(source);
    let mut reader = Reader {
        lines: &lines,
        scopes: vec![Some(Object::Extender)],
        read: Read::default(),
    };
    reader.visit(&parsed.node());
    reader.read
}

/// What a RABL template is evaluated on.
pub(crate) const ENGINE: &str = "Rabl::Engine";

/// Is this file a RABL template a controller renders?
pub(crate) fn is_rabl(path: &str) -> bool {
    path.ends_with(".rabl") && crate::tree::views::under(path, "views").is_some()
}

/// How many templates an `extends` chain is followed through.
const MAX_EXTENDS: usize = 4;

/// The class of an object a template serializes.
fn class_of(
    tree: &Tree,
    facts: &Facts,
    object: &Object,
    path: &str,
    depth: usize,
) -> Option<String> {
    match object {
        Object::Extender => extended_as(tree, path, depth),
        Object::Value(value) | Object::Each(value) => {
            super::views::value_class(tree, facts, value, Pos { line: 1, col: 1 }, path)
        }
        Object::Child(parent, name) => {
            let parent = class_of(tree, facts, parent, path, depth)?;
            let reader = tree.lookup(&parent, false, name)?;
            if let Some(records) = reader.records.as_deref() {
                return tree.returned_class(&reader, records);
            }
            let (declarer, returns) = match reader.returns_for(Some(0), false) {
                Some(returns) => (reader.clone(), returns.to_string()),
                None => tree.declared_returns(&reader, Some(0), false)?,
            };
            tree.returned_class(&declarer, &returns)
        }
    }
}

fn typed(fqn: String) -> Receiver {
    Receiver {
        fqn,
        singleton: false,
        via: "rabl:object",
        agreeing: 1,
        total: 1,
        ambiguous: false,
        rivals: Vec::new(),
        bound: true,
    }
}

/// What a symbol in a RABL template names a method of: `:title` in
/// `attributes :id, :title` is `@post.title` for `object @post`.
pub(super) fn symbol_receiver(
    tree: &Tree,
    facts: &Facts,
    call: &Call,
    path: &str,
) -> Option<Receiver> {
    if call.recv != RecvShape::Symbol || !is_rabl(path) {
        return None;
    }
    let read = read(facts.source.as_deref()?);
    let (_, object) = read.symbols.iter().find(|(at, _)| *at == call.pos)?;
    class_of(tree, facts, object, path, 0).map(typed)
}

/// The object the templates that `extends` this one serialize where they
/// do, when they agree.
fn extended_as(tree: &Tree, path: &str, depth: usize) -> Option<String> {
    if depth >= MAX_EXTENDS {
        return None;
    }
    let root = std::path::Path::new(tree.checkout_root());
    let mut classes: Vec<String> = Vec::new();
    for name in ["extends", "partial"] {
        for caller in tree.files_calling(name).iter().filter(|f| is_rabl(f)) {
            let absolute = root.join(caller).to_string_lossy().into_owned();
            let Some(facts) = tree.file_facts(&absolute) else {
                continue;
            };
            let Some(source) = facts.source.as_deref() else {
                continue;
            };
            for (_, template, object) in read(source).extends.iter() {
                let named = crate::core::Named::Template(template.clone());
                let reaches = crate::tree::views::template_files(root, caller, &named, None)
                    .iter()
                    .any(|file| file == path);
                if reaches
                    && let Some(class) = class_of(tree, &facts, object, caller, depth + 1)
                    && !classes.contains(&class)
                {
                    classes.push(class);
                }
            }
        }
    }
    match classes.as_slice() {
        [one] => Some(one.clone()),
        _ => None,
    }
}

/// What a `node` block's parameter holds: the object of the scope the block
/// is written in.
pub(super) fn param_receiver(
    tree: &Tree,
    facts: &Facts,
    call: &Call,
    path: &str,
) -> Option<Receiver> {
    if call.recv != RecvShape::Local || !is_rabl(path) {
        return None;
    }
    let name = call.recv_text.as_deref()?;
    let line = call.recv_pos.unwrap_or(call.pos).line;
    let read = read(facts.source.as_deref()?);
    let (_, _, object) = read
        .params
        .iter()
        .filter(|(param, (from, to), _)| param == name && *from <= line && line <= *to)
        .min_by_key(|(_, (from, to), _)| to - from)?;
    class_of(tree, facts, object, path, 0).map(typed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_symbol_names_a_method_of_the_object_of_its_scope() {
        let source = b"object @post\nattributes :id, :title => :name\nchild(:comments) do\n  attributes :body\nend\nnode(:x) { |p| p.slug }\n";
        let read = read_fresh(source);
        let at = |line, col| Pos { line, col };
        let post = Object::Value("@post".into());
        let comments = Object::Child(Box::new(post.clone()), "comments".into());
        assert_eq!(
            read.symbols,
            [
                (at(2, 13), post.clone()),
                (at(2, 18), post.clone()),
                (at(3, 8), post.clone()),
                (at(4, 15), comments),
            ]
        );
        assert_eq!(read.params, [("p".to_string(), (6, 6), post)]);
    }

    #[test]
    fn a_collection_and_an_ivar_child_are_each_of_their_value() {
        let read =
            read_fresh(b"collection @posts\nchild(@author => :author) { attributes :name }\n");
        assert_eq!(read.symbols[0].1, Object::Value("@author".into()));
        let read = super::read_fresh(b"collection @posts\nattributes :id\n");
        assert_eq!(read.symbols[0].1, Object::Each("@posts".into()));
    }
}
