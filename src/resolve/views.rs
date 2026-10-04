//! An `@ivar` a template reads, typed by what the controller action that
//! renders it assigns (DEC-522).
//!
//! Rails copies a controller's instance variables into the view after the
//! action runs, so `@post` in `posts/show.html.erb` holds what
//! `PostsController#show` — or a `before_action` it runs — wrote. The
//! controller's file is read for those writes, and each is typed as an
//! assignment there is, in that file.

use super::{Receiver, declares_a_bound, type_of};
use crate::core::{Assign, Call, Facts, Kind};
use crate::tree::Tree;
use std::sync::Arc;

/// The callbacks that run before an action, whose writes it sees.
const BEFORE: [&str; 3] = [
    "before_action",
    "prepend_before_action",
    "append_before_action",
];

/// What `call`'s instance-variable receiver holds in the template at `path`.
pub(super) fn view_ivar(tree: &Tree, call: &Call, path: &str) -> Option<Receiver> {
    let target = call.recv_text.as_deref()?;
    let writes = controller_writes(tree, path, target);
    let total = writes.len();
    let votes: Vec<(String, bool, &'static str)> = writes
        .iter()
        .filter_map(|(file, facts, assign)| {
            type_of(
                tree,
                facts,
                &assign.value,
                &assign.nesting,
                assign.pos,
                file,
                0,
                0,
            )
        })
        .collect();
    let (fqn, singleton, _) = votes
        .iter()
        .rev()
        .max_by_key(|(f, _, _)| votes.iter().filter(|(g, _, _)| g == f).count())
        .cloned()?;
    let agreeing = votes.iter().filter(|(f, _, _)| *f == fqn).count();
    let bound = votes
        .iter()
        .any(|(f, _, via)| *f == fqn && declares_a_bound(via));
    let mut rivals: Vec<(String, bool)> = Vec::new();
    for (other, side, _) in &votes {
        if *other != fqn && !rivals.iter().any(|(r, _)| r == other) {
            rivals.push((other.clone(), *side));
        }
    }
    Some(Receiver {
        fqn,
        singleton,
        via: "controller",
        agreeing,
        total: total.max(votes.len()),
        ambiguous: !rivals.is_empty(),
        rivals,
        bound,
    })
}

/// The writes to `target` that a template at `path` sees: those of the
/// action that renders it, its `before_action`s and an action that renders
/// it by symbol (`render :edit` in `update`); for a partial or a layout, or
/// when none of those writes it, every write in the controller's chain.
fn controller_writes(tree: &Tree, path: &str, target: &str) -> Vec<(String, Arc<Facts>, Assign)> {
    let renders =
        crate::scan::is_template(path) && crate::tree::views::under(path, "views").is_some();
    let Some((controller, action)) = renders.then(|| tree.renderer_of(path)).flatten() else {
        return Vec::new();
    };
    let files: Vec<(String, Arc<Facts>)> = tree
        .ancestors(&controller)
        .chain
        .iter()
        .flat_map(|class| tree.sites(class))
        .map(|site| site.path)
        .filter(|path| tree.in_checkout(path))
        .fold(Vec::new(), |mut paths, path| {
            if !paths.contains(&path) {
                paths.push(path);
            }
            paths
        })
        .into_iter()
        .filter_map(|path| Some((path.clone(), tree.file_facts(&path)?)))
        .collect();
    let dir = crate::tree::views::under(path, "views")
        .and_then(|rest| rest.rsplit_once('/'))
        .map_or("", |(dir, _)| dir);
    let runs: Vec<String> = match &action {
        Some(action) => {
            let mut runs = vec![action.clone()];
            for (_, facts) in &files {
                if let Some(source) = &facts.source {
                    runs.extend(callbacks_before(source, action));
                }
                runs.extend(rendering(facts, action, dir));
            }
            runs
        }
        None => Vec::new(),
    };
    for narrow in [true, false] {
        let mut found = Vec::new();
        for (file, facts) in &files {
            for assign in facts
                .assigns
                .iter()
                .filter(|a| a.target == target && !a.singleton)
            {
                let method = super::enclosing_method(facts, assign.pos.line);
                let counts = method.is_some_and(|m| {
                    m.kind == Kind::Method && !m.singleton && (!narrow || runs.contains(&m.name))
                });
                if counts {
                    found.push((file.clone(), facts.clone(), assign.clone()));
                }
            }
        }
        if !found.is_empty() {
            return found;
        }
    }
    Vec::new()
}

/// The model a template's collection holds, by the constant its
/// controller's writes start from: `@posts = Post.where(…).page(n)`.
fn collection_model(tree: &Tree, path: &str, target: &str) -> Option<String> {
    let heads: Vec<String> = controller_writes(tree, path, target)
        .iter()
        .filter_map(|(_, facts, assign)| {
            let head = match &assign.value {
                crate::core::ValueShape::ConstCall { recv, .. } => recv.clone(),
                crate::core::ValueShape::Chain(at) => {
                    let mut call = facts.calls.iter().find(|c| c.pos == *at)?;
                    for _ in 0..8 {
                        match &call.recv_value {
                            Some(crate::core::RecvValue::Call(next)) => {
                                call = facts.calls.iter().find(|c| c.pos == *next)?;
                            }
                            _ => break,
                        }
                    }
                    (call.recv == crate::core::RecvShape::Const)
                        .then(|| call.recv_text.clone())??
                }
                _ => return None,
            };
            // A class the chain starts from; a module's method
            // (`Spree.user_class`) may return any class.
            tree.namespace_named(&tree.resolve(&head, &assign.nesting).fqn?)
                .filter(|fqn| tree.kind_of(fqn) == Some("class"))
        })
        .collect();
    let first = heads.first()?;
    heads.iter().all(|h| h == first).then(|| first.clone())
}

/// The class a value a template hands `render` holds — `@post`, `post` —
/// whose partial it renders (DEC-524). A collection's element is its
/// relation's model, or the class itself when the writes name it.
pub(crate) fn value_class(
    tree: &Tree,
    facts: &Facts,
    value: &str,
    at: crate::core::Pos,
    path: &str,
) -> Option<String> {
    let call = Call {
        name: "to_partial_path".to_string(),
        recv: match value.starts_with('@') {
            true => crate::core::RecvShape::Ivar,
            false => crate::core::RecvShape::Local,
        },
        recv_text: Some(value.to_string()),
        nesting: Vec::new(),
        singleton: false,
        recv_pos: Some(at),
        recv_value: None,
        block_owner: None,
        in_example: false,
        group_body: false,
        in_scope: false,
        stands_for: None,
        argc: Some(0),
        block: false,
        pos: at,
    };
    let typed = super::receiver_of(tree, facts, &call, path).filter(|typed| {
        !typed.singleton && !matches!(typed.fqn.as_str(), "Array" | super::RELATION | "Object")
    });
    match typed {
        Some(typed) => Some(typed.fqn),
        None if value.starts_with('@') => collection_model(tree, path, value),
        None => None,
    }
}

/// The methods of a controller's file that render `action`'s template by
/// name: `render :edit`, `render "edit"`, `render "posts/edit"`, `render
/// template: "posts/edit"` in `update`. A symbol is recorded as a call of
/// its name, so one on the line of a `render` is this; a string is a
/// template the call names (DEC-524). `dir` is the template's directory
/// under the views.
fn rendering(facts: &Facts, action: &str, dir: &str) -> Vec<String> {
    let renders: Vec<u32> = facts
        .calls
        .iter()
        .filter(|c| c.name == "render")
        .map(|c| c.pos.line)
        .collect();
    let by_symbol = facts
        .calls
        .iter()
        .filter(|c| c.name == action && renders.contains(&c.pos.line))
        .map(|c| c.pos.line);
    let whole = format!("{dir}/{action}");
    let by_name = facts.templates.iter().filter_map(|t| match &t.names {
        crate::core::Named::Render(name) | crate::core::Named::Template(name)
            if name == action || *name == whole =>
        {
            Some(t.pos.line)
        }
        _ => None,
    });
    by_symbol
        .chain(by_name)
        .filter_map(|line| super::enclosing_method(facts, line))
        .map(|m| m.name.clone())
        .collect()
}

/// The methods a file's `before_action`s run before `action`: each symbol
/// they are handed, less those whose `only:` leaves the action out or whose
/// `except:` names it. Read from the source, since facts keep no option's
/// value.
fn callbacks_before(source: &[u8], action: &str) -> Vec<String> {
    struct Callbacks<'a> {
        action: &'a str,
        found: Vec<String>,
    }
    fn names(node: &ruby_prism::Node<'_>) -> Vec<String> {
        let one = |n: &ruby_prism::Node<'_>| {
            n.as_symbol_node()
                .map(|s| String::from_utf8_lossy(s.unescaped()).into_owned())
                .or_else(|| {
                    n.as_string_node()
                        .map(|s| String::from_utf8_lossy(s.unescaped()).into_owned())
                })
        };
        match node.as_array_node() {
            Some(list) => list.elements().iter().filter_map(|e| one(&e)).collect(),
            None => one(node).into_iter().collect(),
        }
    }
    impl<'pr> ruby_prism::Visit<'pr> for Callbacks<'_> {
        fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
            let name = String::from_utf8_lossy(node.name().as_slice()).into_owned();
            if node.receiver().is_none()
                && BEFORE.contains(&name.as_str())
                && let Some(args) = node.arguments()
            {
                let mut symbols = Vec::new();
                let mut applies = true;
                for arg in args.arguments().iter() {
                    if let Some(hash) = arg.as_keyword_hash_node() {
                        for element in hash.elements().iter() {
                            let Some(assoc) = element.as_assoc_node() else {
                                continue;
                            };
                            let key = assoc
                                .key()
                                .as_symbol_node()
                                .map(|k| String::from_utf8_lossy(k.unescaped()).into_owned());
                            let listed = names(&assoc.value()).iter().any(|n| n == self.action);
                            match key.as_deref() {
                                Some("only") => applies &= listed,
                                Some("except") => applies &= !listed,
                                _ => {}
                            }
                        }
                    } else if let Some(symbol) = arg.as_symbol_node() {
                        symbols.push(String::from_utf8_lossy(symbol.unescaped()).into_owned());
                    }
                }
                if applies {
                    self.found.extend(symbols);
                }
            }
            ruby_prism::visit_call_node(self, node);
        }
    }
    let parsed = ruby_prism::parse(source);
    let mut callbacks = Callbacks {
        action,
        found: Vec::new(),
    };
    ruby_prism::Visit::visit(&mut callbacks, &parsed.node());
    callbacks.found
}

/// A name a partial reads that a `render` reaching it hands it as a local
/// (DEC-525): where each render names it, and the class of each value it
/// is handed that the index can type.
pub(crate) struct PartialLocal {
    pub(crate) sites: Vec<crate::tree::Site>,
    pub(crate) types: Vec<String>,
}

/// `post` in `posts/_post.html.erb`: a local Rails assigns from `locals:`
/// (or a `render`'s keywords), or from the object `render @post` renders,
/// named for the partial. Rails declares them before the template's code,
/// so a local shadows a helper of its name. Found by reading the
/// checkout's files that call `render` for one that reaches this partial.
pub(crate) fn partial_local(tree: &Tree, call: &Call, path: &str) -> Option<PartialLocal> {
    if call.recv != crate::core::RecvShape::Implicit
        || call.argc != Some(0)
        || call.block
        || !call.nesting.is_empty()
        || !crate::scan::is_template(path)
        || crate::tree::views::under(path, "views").is_none()
    {
        return None;
    }
    let file = path.rsplit('/').next()?;
    let base = file.strip_prefix('_')?.split('.').next()?;
    let root = std::path::Path::new(tree.checkout_root());
    let mut found = PartialLocal {
        sites: Vec::new(),
        types: Vec::new(),
    };
    for caller in tree.files_calling("render").iter() {
        let absolute = root.join(caller).to_string_lossy().into_owned();
        let Some(facts) = tree.file_facts(&absolute) else {
            continue;
        };
        for template in &facts.templates {
            let names_base = match &template.names {
                crate::core::Named::Render(name) | crate::core::Named::Partial(name) => {
                    name.rsplit('/').next() == Some(base)
                }
                crate::core::Named::Template(_) => false,
                crate::core::Named::Object { .. } => true,
            };
            if !names_base {
                continue;
            }
            let class = match &template.names {
                crate::core::Named::Object { value, .. } => {
                    value_class(tree, &facts, value, template.pos, caller)
                }
                _ => None,
            };
            let reaches =
                crate::tree::views::template_files(root, caller, &template.names, class.as_deref())
                    .iter()
                    .any(|file| file == path);
            if !reaches {
                continue;
            }
            let site = |at: crate::core::Pos| crate::tree::Site {
                path: absolute.clone(),
                line: at.line,
                col: at.col,
                kind: "local".to_string(),
            };
            for local in template.locals.iter().filter(|l| l.name == call.name) {
                found.sites.push(site(local.pos));
                if let Some((value, at)) = &local.value
                    && let Some(class) = value_class(tree, &facts, value, *at, caller)
                {
                    found.types.push(class);
                }
            }
            if matches!(template.names, crate::core::Named::Object { .. }) && call.name == base {
                found.sites.push(site(template.pos));
                found.types.extend(class);
            }
        }
    }
    (!found.sites.is_empty()).then_some(found)
}

/// What a partial's local holds, when every typed value handed it agrees.
pub(super) fn partial_local_type(tree: &Tree, call: &Call, path: &str) -> Option<Receiver> {
    let local = partial_local(tree, call, path)?;
    let first = local.types.first()?.clone();
    let agreeing = local.types.iter().filter(|t| **t == first).count();
    let mut rivals: Vec<(String, bool)> = Vec::new();
    for other in &local.types {
        if *other != first && !rivals.iter().any(|(r, _)| r == other) {
            rivals.push((other.clone(), false));
        }
    }
    Some(Receiver {
        fqn: first,
        singleton: false,
        via: "render",
        agreeing,
        total: local.sites.len().max(local.types.len()),
        ambiguous: !rivals.is_empty(),
        rivals,
        bound: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_before_action_runs_for_the_actions_its_options_leave_in() {
        let source = b"class C\n  before_action :a\n  before_action :b, only: [:show]\n  \
            before_action :c, except: :show\n  prepend_before_action :d, only: %i[index]\nend\n";
        assert_eq!(callbacks_before(source, "show"), ["a", "b"]);
        assert_eq!(callbacks_before(source, "index"), ["a", "c", "d"]);
    }
}
