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
use crate::tree::views::ViewTemplate;
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

/// A controller that renders a template, and the methods whose writes the
/// template sees: the action, the `before_action`s it runs, and an action
/// that renders it by name — `None` for every method in the controller's
/// chain, a partial's or a layout's.
pub(crate) struct Rendering {
    pub(crate) controller: String,
    runs: Option<Vec<String>>,
    /// Named by the path convention (DEC-522), whose writes widen to the
    /// whole chain when the action's write nothing. A controller that names
    /// the template in a `render` is read only for what that action runs.
    conventional: bool,
}

/// How many partials deep the templates rendering a partial are followed.
const PARTIAL_DEPTH: usize = 3;

/// The controllers that render the template at `path` (relative to the
/// checkout): its directory's by the path convention; one whose action
/// names it in a `render` (`render template: "widgets/show"` in
/// `GadgetsController#show`); and for a partial, those of each template
/// that renders it. The convention's first.
pub(crate) fn renderings(tree: &Tree, path: &str) -> Vec<Rendering> {
    let mut seen = vec![path.to_string()];
    renderings_at(tree, path, 0, &mut seen)
}

fn renderings_at(tree: &Tree, path: &str, depth: usize, seen: &mut Vec<String>) -> Vec<Rendering> {
    use crate::core::Named;
    use crate::tree::views::under;
    let Some(rest) = under(path, "views").filter(|_| ViewTemplate::of(path).is_some()) else {
        return Vec::new();
    };
    let (dir, file) = rest.rsplit_once('/').unwrap_or(("", rest));
    let mut found: Vec<Rendering> = Vec::new();
    if let Some((controller, action)) = tree.renderer_of(path) {
        let runs = action.map(|action| {
            let mut runs = vec![action.clone()];
            for (_, facts) in chain_files(tree, &controller) {
                if let Some(source) = &facts.source {
                    runs.extend(callbacks_before(source, &action));
                }
                runs.extend(rendering(&facts, &action, dir));
            }
            runs
        });
        found.push(Rendering {
            controller,
            runs,
            conventional: true,
        });
    }
    let partial = file.starts_with('_');
    let base = file
        .trim_start_matches('_')
        .split('.')
        .next()
        .unwrap_or(file);
    let root = std::path::Path::new(tree.checkout_root());
    for caller in tree.files_calling("render").iter() {
        let in_controller = under(caller, "controllers").is_some();
        if !in_controller && !(partial && under(caller, "views").is_some()) {
            continue;
        }
        // A controller names another directory's template by its path; a
        // view may name a partial beside it by its name alone.
        let naming = match in_controller && !dir.is_empty() {
            true => format!("{dir}/{base}"),
            false => base.to_string(),
        };
        let absolute = root.join(caller).to_string_lossy().into_owned();
        let Some(named) = tree.file_templates(&absolute, Some(&naming)) else {
            continue;
        };
        for template in named.iter() {
            let (Named::Render(name) | Named::Partial(name) | Named::Template(name)) =
                &template.names
            else {
                continue;
            };
            let reaches = name.rsplit('/').next() == Some(base)
                && crate::tree::views::template_files(root, caller, &template.names, None)
                    .iter()
                    .any(|file| file == path);
            if !reaches {
                continue;
            }
            if !in_controller {
                if depth < PARTIAL_DEPTH && !seen.contains(caller) {
                    seen.push(caller.clone());
                    found.extend(renderings_at(tree, caller, depth + 1, seen));
                }
                continue;
            }
            let Some(facts) = tree.file_facts(&absolute) else {
                continue;
            };
            let Some(method) = super::enclosing_method(&facts, template.pos.line) else {
                continue;
            };
            let Some(controller) = tree.scope_fqn(&method.nesting) else {
                continue;
            };
            // The convention's own controller already counts the actions
            // that render the template by name (`rendering`).
            if found
                .iter()
                .any(|r| r.conventional && r.controller == controller)
            {
                continue;
            }
            let mut runs = vec![method.name.clone()];
            for (_, facts) in chain_files(tree, &controller) {
                if let Some(source) = &facts.source {
                    runs.extend(callbacks_before(source, &method.name));
                }
            }
            found.push(Rendering {
                controller,
                runs: Some(runs),
                conventional: false,
            });
        }
    }
    found
}

/// The checkout's files that write a controller's chain, each read once.
fn chain_files(tree: &Tree, controller: &str) -> Vec<(String, Arc<Facts>)> {
    tree.ancestors(controller)
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
        .collect()
}

/// The writes to `target` that a template at `path` sees, from every
/// controller that renders it: those of the methods each runs for it; for a
/// partial or a layout, or when the conventional action's write nothing,
/// every write in that controller's chain.
fn controller_writes(tree: &Tree, path: &str, target: &str) -> Vec<(String, Arc<Facts>, Assign)> {
    let mut found: Vec<(String, Arc<Facts>, Assign)> = Vec::new();
    for rendering in renderings(tree, path) {
        let files = chain_files(tree, &rendering.controller);
        let widths: &[bool] = match (&rendering.runs, rendering.conventional) {
            (None, _) => &[false],
            (Some(_), true) => &[true, false],
            (Some(_), false) => &[true],
        };
        for &narrow in widths {
            let mut writes = Vec::new();
            for (file, facts) in &files {
                for assign in facts
                    .assigns
                    .iter()
                    .filter(|a| a.target == target && !a.singleton)
                {
                    let method = super::enclosing_method(facts, assign.pos.line);
                    let counts = method.is_some_and(|m| {
                        m.kind == Kind::Method
                            && !m.singleton
                            && (!narrow
                                || rendering.runs.as_ref().is_some_and(|r| r.contains(&m.name)))
                    });
                    if counts {
                        writes.push((file.clone(), facts.clone(), assign.clone()));
                    }
                }
            }
            if writes.is_empty() {
                continue;
            }
            for write in writes {
                if !found
                    .iter()
                    .any(|(f, _, a)| *f == write.0 && a.pos == write.2.pos)
                {
                    found.push(write);
                }
            }
            break;
        }
    }
    found
}

/// The controller a call on a view's `self` runs a name on that a
/// `helper_method` exposes (DEC-521): the one that renders the template,
/// whose own method — an override, too — is what the generated helper
/// sends to. Rendered by several that land on different methods, or by none
/// that exposes the name — a shared partial — every exposing controller and
/// each subclass overriding it is a rival, and the answer is ambiguous.
pub(super) fn exposed_receiver(
    tree: &Tree,
    name: &str,
    path: &str,
    via: &'static str,
) -> Option<Receiver> {
    let owners = tree.exposers(name);
    if owners.is_empty() {
        return None;
    }
    let exposes = |class: &str| {
        owners
            .iter()
            .any(|owner| owner == class || tree.inherits(class, owner))
    };
    let mut pool: Vec<String> = Vec::new();
    for rendering in renderings(tree, path) {
        if exposes(&rendering.controller) && !pool.contains(&rendering.controller) {
            pool.push(rendering.controller);
        }
    }
    if pool.is_empty() {
        pool = owners.clone();
        for method in tree.named(name).iter() {
            if !method.singleton
                && !pool.contains(&method.owner)
                && tree.kind_of(&method.owner) == Some("class")
                && exposes(&method.owner)
            {
                pool.push(method.owner.clone());
            }
        }
    }
    let mut landings: Vec<(String, crate::tree::MethodDef)> = Vec::new();
    for class in pool {
        let Some(found) = tree.lookup(&class, false, name) else {
            continue;
        };
        let same = |(_, other): &(String, crate::tree::MethodDef)| {
            other.owner == found.owner
                && other.site.path == found.site.path
                && other.site.line == found.site.line
        };
        if !landings.iter().any(same) {
            landings.push((class, found));
        }
    }
    let ((first, _), rest) = landings.split_first()?;
    Some(Receiver {
        fqn: first.clone(),
        singleton: false,
        via,
        agreeing: 1,
        total: landings.len(),
        ambiguous: !rest.is_empty(),
        rivals: rest
            .iter()
            .map(|(class, _)| (class.clone(), false))
            .collect(),
        bound: false,
    })
}

/// Where a template's `@ivar` is set: each write the controllers that render
/// it make, as its reads are typed (DEC-522). Absolute paths.
pub(crate) fn template_ivar_writes(tree: &Tree, path: &str, name: &str) -> Vec<crate::tree::Site> {
    controller_writes(tree, path, name)
        .into_iter()
        .map(|(file, _, assign)| crate::tree::Site {
            path: file,
            line: assign.pos.line,
            col: assign.pos.col,
            kind: "ivar".to_string(),
        })
        .collect()
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
    object_value(tree, facts, value, at, path).map(|(class, _)| class)
}

/// The class a value handed `render` holds, and whether that is the
/// element of a collection it holds (a relation's, by its model).
fn object_value(
    tree: &Tree,
    facts: &Facts,
    value: &str,
    at: crate::core::Pos,
    path: &str,
) -> Option<(String, bool)> {
    let call = Call {
        written: None,
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
    let typed = super::receiver_of(tree, facts, &call, path).filter(|typed| !typed.singleton);
    match typed {
        Some(typed) if !matches!(typed.fqn.as_str(), "Array" | super::RELATION | "Object") => {
            Some((typed.fqn, false))
        }
        // Only a value typed as a collection is known to be one.
        typed if value.starts_with('@') => {
            let many = typed.is_some_and(|t| t.fqn != "Object");
            collection_model(tree, path, value).map(|c| (c, many))
        }
        _ => None,
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
        || ViewTemplate::of(path).is_none()
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
        let Some(named) = tree.file_templates(&absolute, None) else {
            continue;
        };
        // The whole file is read only for a render that names this partial.
        let mut read: Option<Arc<Facts>> = None;
        for template in named.iter() {
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
            let facts = match &read {
                Some(facts) => Arc::clone(facts),
                None => {
                    let Some(facts) = tree.file_facts(&absolute) else {
                        break;
                    };
                    read = Some(Arc::clone(&facts));
                    facts
                }
            };
            let (class, many) = match &template.names {
                crate::core::Named::Object { value, collection } => {
                    // Untyped, a value named the partial's plural (`@posts`
                    // for `_post`) is taken to be the collection it reads as.
                    let plural = value.trim_start_matches('@') == crate::inflect::plural(base);
                    match object_value(tree, &facts, value, template.pos, caller) {
                        Some((class, many)) => (Some(class), many || *collection || plural),
                        None => (None, *collection || plural),
                    }
                }
                _ => (None, false),
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
            // Every local a render hands, as a Hash keyed by name.
            if call.name == "local_assigns" {
                found.sites.push(site(template.pos));
                found.types.push("Hash".to_string());
                continue;
            }
            for local in template.locals.iter().filter(|l| l.name == call.name) {
                found.sites.push(site(local.pos));
                if let Some((value, at)) = &local.value
                    && let Some(class) = value_class(tree, &facts, value, *at, caller)
                {
                    found.types.push(class);
                }
            }
            // `render @posts` hands `post` (and `post_counter`, for a
            // collection) — unless an `as:` names the local instead.
            let renamed = template
                .locals
                .iter()
                .any(|l| l.value.as_ref().is_some_and(|(_, at)| *at == template.pos));
            if matches!(template.names, crate::core::Named::Object { .. }) && !renamed {
                let counter = [format!("{base}_counter"), format!("{base}_iteration")];
                if call.name == base {
                    found.sites.push(site(template.pos));
                    found.types.extend(class);
                } else if many && counter.contains(&call.name) {
                    found.sites.push(site(template.pos));
                }
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
