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
    if !crate::tree::views::is_view(path) {
        return None;
    }
    let target = call.recv_text.as_deref()?;
    let (controller, action) = tree.renderer_of(path)?;
    let chain: Vec<String> = tree
        .ancestors(&controller)
        .chain
        .iter()
        .filter(|class| tree.sites(class).iter().any(|s| tree.in_checkout(&s.path)))
        .cloned()
        .collect();
    let files: Vec<(String, Arc<Facts>)> = chain
        .iter()
        .flat_map(|class| tree.sites(class))
        .map(|site| site.path)
        .fold(Vec::new(), |mut paths, path| {
            if !paths.contains(&path) {
                paths.push(path);
            }
            paths
        })
        .into_iter()
        .filter_map(|path| Some((path.clone(), tree.file_facts(&path)?)))
        .collect();
    let writes = |facts: &Facts| -> Vec<Assign> {
        facts
            .assigns
            .iter()
            .filter(|a| a.target == target && !a.singleton)
            .cloned()
            .collect()
    };
    // The action's own writes, its callbacks', and those of an action that
    // renders this template by its symbol (`render :edit` in `update`).
    let runs: Vec<String> = match &action {
        Some(action) => {
            let mut runs = vec![action.clone()];
            for (_, facts) in &files {
                if let Some(source) = &facts.source {
                    runs.extend(callbacks_before(source, action));
                }
                runs.extend(rendering(facts, action));
            }
            runs
        }
        None => Vec::new(),
    };
    let mut votes: Vec<(String, bool, &'static str)> = Vec::new();
    let mut total = 0;
    for pass in [true, false] {
        for (file, facts) in &files {
            for assign in writes(facts) {
                let method = super::enclosing_method(facts, assign.pos.line);
                let counts = match pass {
                    true => method.is_some_and(|m| {
                        m.kind == Kind::Method && !m.singleton && runs.contains(&m.name)
                    }),
                    // No action of its own, or none of the action's runs
                    // writes it: every write in the controller's chain.
                    false => method.is_some_and(|m| !m.singleton),
                };
                if !counts {
                    continue;
                }
                total += 1;
                if let Some(vote) = type_of(
                    tree,
                    facts,
                    &assign.value,
                    &assign.nesting,
                    assign.pos,
                    file,
                    0,
                    0,
                ) {
                    votes.push(vote);
                }
            }
        }
        if total > 0 {
            break;
        }
    }
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

/// The methods of a controller's file that render `action`'s template by
/// name: a `render :edit` in `update`. The symbol is
/// recorded as a call of its name; one on the line of a `render` is this.
fn rendering<'f>(facts: &'f Facts, action: &str) -> impl Iterator<Item = String> + 'f {
    let renders: Vec<u32> = facts
        .calls
        .iter()
        .filter(|c| c.name == "render")
        .map(|c| c.pos.line)
        .collect();
    let action = action.to_string();
    facts
        .calls
        .iter()
        .filter(move |c| c.name == action && renders.contains(&c.pos.line))
        .filter_map(|c| super::enclosing_method(facts, c.pos.line))
        .map(|m| m.name.clone())
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
