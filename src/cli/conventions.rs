//! Library conventions that call a method by a name no call site writes, each
//! keyed on the library's own code being in the tree, so a version that does
//! not have the convention does not claim it (DEC-362).

use std::collections::{HashMap, HashSet};

use crate::tree::Tree;

/// The symbols each file writes, read once per `--dead` run.
#[derive(Default)]
pub(super) struct Symbols {
    by_path: HashMap<String, Vec<(String, u32)>>,
}

impl Symbols {
    /// The first line of `path` with the symbol `:name`.
    fn line_of(&mut self, path: &str, name: &str) -> Option<u32> {
        let symbols = self.by_path.entry(path.to_string()).or_insert_with(|| {
            std::fs::read(path)
                .map(|source| {
                    crate::extract::symbol_literals(&source)
                        .into_iter()
                        .map(|(name, pos, _)| (name, pos.line))
                        .collect()
                })
                .unwrap_or_default()
        });
        symbols
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, line)| *line)
    }
}

/// A library that calls a method by a name no call site writes: who, why,
/// and the line that names it, when one does.
pub(super) struct Convention {
    pub(super) by: &'static str,
    pub(super) reason: String,
    pub(super) at: Option<(String, u32)>,
}

const SERIALIZER: &str = "ActiveModel::Serializer";

/// Where `attributes :x` (or `has_one :x`, …) names the attribute that
/// ActiveModel::Serializers 0.8/0.9 calls `include_x?` for, when `owner` is a
/// serializer or a module one mixes in: the symbol, in the serializer's own
/// file or an ancestor's. None when the method is no such hook, or the
/// indexed gem builds no `include_` methods (0.10 does not).
pub(super) fn serializer_include(
    tree: &Tree,
    owner: &str,
    name: &str,
    symbols: &mut Symbols,
) -> Option<Convention> {
    let attribute = name.strip_prefix("include_")?.strip_suffix('?')?;
    tree.lookup(SERIALIZER, true, "define_include_method")?;
    // A serializer, or a mixin whose includers are: the hook is called on
    // the serializer, which is where its attributes are declared.
    let serializers: Vec<String> = if tree.inherits(owner, SERIALIZER) {
        vec![owner.to_string()]
    } else {
        tree.includers_of(owner)
            .into_iter()
            .filter(|class| tree.inherits(class, SERIALIZER))
            .collect()
    };
    let mut seen = HashSet::new();
    for serializer in &serializers {
        for ancestor in &tree.ancestors(serializer).chain {
            if ancestor == SERIALIZER || !seen.insert(ancestor.clone()) {
                continue;
            }
            for site in tree.sites(ancestor) {
                let path = tree.site_path(&site.path);
                if let Some(line) = symbols.line_of(&path, attribute) {
                    return Some(Convention {
                        by: "ActiveModel::Serializers",
                        reason: format!(
                            "named only by a symbol ActiveModel::Serializers calls it for, at {}:{line}",
                            site.path
                        ),
                        at: Some((site.path, line)),
                    });
                }
            }
        }
    }
    None
}

const ASSIGNMENT: &str = "ActiveModel::AttributeAssignment";

/// Whether `name` is a public writer that Active Model's `assign_attributes`
/// may call by the key it is handed — `new(mode: …)`, `update(…)`, a form's
/// params — on `owner` or the classes that mix it in (DEC-364).
pub(super) fn assigned_writer(tree: &Tree, owner: &str, name: &str, public: bool) -> bool {
    let Some(attribute) = name.strip_suffix('=') else {
        return false;
    };
    let writer = attribute.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
        && attribute
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_');
    public
        && writer
        && (tree.inherits(owner, ASSIGNMENT)
            || tree
                .includers_of(owner)
                .iter()
                .any(|class| tree.inherits(class, ASSIGNMENT)))
}

/// The line spans of a file's blocks that decide whether Thor makes a
/// command of a `def` in them, read once per file.
#[derive(Default)]
pub(super) struct ThorBlocks {
    by_path: HashMap<String, Spans>,
}

#[derive(Default)]
struct Spans {
    /// `no_commands do` and `no_tasks do`: Thor makes no command of these.
    hidden: Vec<(u32, u32)>,
    /// A concern's `included do`, whose `def`s land on the includer.
    included: Vec<(u32, u32)>,
}

impl ThorBlocks {
    fn of(&mut self, path: &str) -> &Spans {
        self.by_path.entry(path.to_string()).or_insert_with(|| {
            let mut spans = Spans::default();
            let Ok(source) = std::fs::read(path) else {
                return spans;
            };
            let parsed = ruby_prism::parse(&source);
            let lines = crate::extract::line_index::LineIndex::new(&source);
            let mut reader = SpanReader {
                spans: &mut spans,
                lines: &lines,
            };
            ruby_prism::Visit::visit(&mut reader, &parsed.node());
            spans
        })
    }
}

struct SpanReader<'a> {
    spans: &'a mut Spans,
    lines: &'a crate::extract::line_index::LineIndex,
}

impl<'pr> ruby_prism::Visit<'pr> for SpanReader<'_> {
    fn visit_call_node(&mut self, call: &ruby_prism::CallNode<'pr>) {
        if call.receiver().is_none()
            && let Some(block) = call.block().and_then(|b| b.as_block_node())
        {
            let at = block.location();
            let span = (
                self.lines.pos(at.start_offset()).line,
                self.lines.pos(at.end_offset()).line,
            );
            match call.name().as_slice() {
                b"no_commands" | b"no_tasks" => self.spans.hidden.push(span),
                b"included" => self.spans.included.push(span),
                _ => {}
            }
        }
        ruby_prism::visit_call_node(self, call);
    }
}

/// Whether Thor runs this public method, `def`'d at `path:line`, by its
/// name: Thor's `method_added` makes a command of each public method a
/// `Thor` subclass defines (`desc "prune"`, then `cli prune`) and a step of
/// a `Thor::Group`'s — every Rails generator's — less those under
/// `no_commands`. A module's method is the module's, which `method_added`
/// never sees, unless a concern defines it in `included do` on a Thor class
/// that includes it (DEC-371).
pub(super) fn thor_command(
    tree: &Tree,
    owner: &str,
    public: bool,
    (path, line): (&str, u32),
    blocks: &mut ThorBlocks,
) -> Option<Convention> {
    if !public {
        return None;
    }
    let group = |class: &str| tree.inherits(class, "Thor::Group");
    let thor = |class: &str| tree.inherits(class, "Thor") || group(class);
    let classes: Vec<String> = if thor(owner) {
        vec![owner.to_string()]
    } else {
        tree.includers_of(owner)
            .into_iter()
            .filter(|class| thor(class))
            .collect()
    };
    if classes.is_empty() {
        return None;
    }
    let spans = blocks.of(path);
    let within = |(from, to): &(u32, u32)| (*from..=*to).contains(&line);
    if spans.hidden.iter().any(within) {
        return None;
    }
    if classes[0] != owner && !spans.included.iter().any(within) {
        return None;
    }
    Some(Convention {
        by: "Thor",
        reason: if classes.iter().any(|c| group(c)) {
            "a Thor::Group's public method, which Thor runs in turn".to_string()
        } else {
            "a Thor command, which Thor runs by its name".to_string()
        },
        at: None,
    })
}

/// Where a controller action named as a policy's predicate is: Pundit's
/// `authorize record` asks the record's policy `"#{action_name}?"`, so
/// `WidgetPolicy#publish?` is called for every controller's `publish` that
/// authorizes. Only when Pundit is in the tree, on a public predicate of a
/// class named a policy or below one.
pub(super) fn pundit_predicate(
    tree: &Tree,
    owner: &str,
    name: &str,
    public: bool,
    root: &str,
) -> Option<Convention> {
    let action = name.strip_suffix('?')?;
    if !public
        || tree
            .lookup("Pundit::Authorization", false, "authorize")
            .is_none()
    {
        return None;
    }
    let policy = |class: &str| crate::tree::public_name(class).ends_with("Policy");
    if !tree.ancestors(owner).chain.iter().any(|a| policy(a)) && !policy(owner) {
        return None;
    }
    // Pundit asks `"#{record.class}Policy"`: credited when that policy's
    // predicate is this one, so a base policy's predicate every subclass
    // overrides is answered by the overrides. A record of no class read may
    // be any policy's that no subclass's own predicate answers instead.
    // A module no class mixes in where the tree can see (`prepend_mod_with`)
    // may be any policy's.
    let unplaced = tree.kind_of(owner) == Some("module") && tree.includers_of(owner).is_empty();
    let answers = |record: &Option<String>| match record {
        _ if unplaced => true,
        Some(record) => policy_of(tree, record)
            .and_then(|policy| tree.lookup(&policy, false, name))
            .is_some_and(|found| found.owner == owner),
        None => !tree
            .named(name)
            .iter()
            .any(|other| other.owner != owner && tree.inherits(&other.owner, owner)),
    };
    let site = tree.named(action).iter().find_map(|method| {
        let in_controller = tree.in_checkout(&method.site.path)
            && method.site.path.contains("/controllers/")
            && !method.singleton
            && method.visibility == "public";
        (in_controller && records_authorized(tree, method, action).iter().any(answers))
            .then(|| method.site.clone())
    })?;
    let path = site
        .path
        .strip_prefix(&format!("{root}/"))
        .unwrap_or(&site.path)
        .to_string();
    Some(Convention {
        by: "Pundit",
        reason: format!(
            "Pundit's `authorize` asks a policy `{name}` for the action `{action}`, at {}:{}",
            path, site.line
        ),
        at: Some((path, site.line)),
    })
}

/// Where an ancestor outside the checkout — a gem's base class — sends
/// `self` a name it computes: CommonMarker's renderer `send(node.type, …)`,
/// Liquid's drop `public_send(method_or_key)`. A subclass's public method
/// may be run so, by a name no call site writes. Each file read once.
#[derive(Default)]
pub(super) struct ForeignSends {
    by_path: HashMap<String, Option<u32>>,
}

impl ForeignSends {
    pub(super) fn of(&mut self, tree: &Tree, owner: &str) -> Option<(String, u32)> {
        // Superclasses only: a gem's mixins (ActiveModel's attribute
        // methods) send computed names for their own purposes, and every
        // model has them.
        let classes = tree.ancestors(owner).chain.clone();
        for ancestor in classes.iter().filter(|a| tree.kind_of(a) == Some("class")) {
            let sites = tree.sites(ancestor);
            // Ruby's own classes, which gems reopen (`Object#with`), are
            // every class's.
            if sites
                .iter()
                .any(|site| crate::tree::is_core(&site.path) || tree.in_stdlib(&site.path))
            {
                continue;
            }
            for site in sites {
                if tree.in_checkout(&site.path) {
                    continue;
                }
                let line = *self
                    .by_path
                    .entry(site.path.clone())
                    .or_insert_with(|| first_computed_send(&site.path));
                if let Some(line) = line {
                    return Some((site.path, line));
                }
            }
        }
        None
    }
}

/// The first line of a file that sends `self` a computed name: `send(x`,
/// `public_send(x`, `__send__(x`, with no receiver or `self.`, whose first
/// argument is no literal.
fn first_computed_send(path: &str) -> Option<u32> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines().enumerate().find_map(|(n, line)| {
        let code = line.trim_start();
        if code.starts_with('#') {
            return None;
        }
        ["public_send(", "__send__(", "send("]
            .iter()
            .find_map(|call| {
                let at = line.find(call)?;
                let before = line[..at].trim_end_matches("self.");
                let on_self = before
                    .chars()
                    .last()
                    .is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '.' || c == ':'));
                let arg = line[at + call.len()..].trim_start();
                let computed = arg.starts_with(|c: char| c.is_ascii_lowercase() || c == '@');
                (on_self && computed).then_some(n as u32 + 1)
            })
    })
}

/// `Widget` → `WidgetPolicy`, when the checkout has it.
fn policy_of(tree: &Tree, record: &str) -> Option<String> {
    tree.resolve(&format!("{record}Policy"), &[]).fqn
}

/// The classes whose policies a controller action `name` asks Pundit for
/// its own predicate: each `authorize record` in the action, or in a
/// `before_action` that runs for it, read for the record's class — `None`
/// for a record of no class read. Empty when the action names its query
/// (`authorize x, :edit?`), skips authorization, or authorizes nothing.
fn records_authorized(
    tree: &Tree,
    action: &crate::tree::MethodDef,
    name: &str,
) -> Vec<Option<String>> {
    let Some(own) = Source::read(&action.site.path) else {
        return Vec::new();
    };
    let controllers: Vec<String> = std::iter::once(action.owner.clone())
        .chain(tree.includers_of(&action.owner))
        .collect();
    // Each `authorize record`'s classes read, and the class the call that
    // led to it names (`before_action -> { check(Widget) }`).
    let mut asks: Vec<(Vec<String>, Option<String>)> = Vec::new();
    if let Some(def) = own.method(name) {
        let body = def.pos.line..=def.end_line;
        let calls: Vec<_> = own
            .facts
            .calls
            .iter()
            .filter(|call| body.contains(&call.pos.line))
            .collect();
        let elsewhere = calls.iter().any(|call| {
            call.name == "skip_authorization"
                || call.name == "authorize" && call.argc.is_some_and(|n| n > 1)
        });
        if elsewhere {
            return Vec::new();
        }
        asks.extend(
            own.authorizing(|line| body.contains(&line))
                .map(|call| (own.record_classes(tree, call), None)),
        );
    }
    if asks.is_empty() {
        // A `before_action` the controller or an ancestor declares, which
        // authorizes in its own method or in a lambda on its line.
        let files: HashSet<String> = controllers
            .iter()
            .flat_map(|class| tree.ancestors(class).chain.clone())
            .flat_map(|class| tree.sites(&class))
            .filter(|site| tree.in_checkout(&site.path))
            .map(|site| site.path)
            .collect();
        let sources: Vec<Source> = files.iter().filter_map(|path| Source::read(path)).collect();
        let filters = sources.iter().flat_map(|source| {
            source
                .before_actions()
                .into_iter()
                .filter(|filter| filter.runs_for(name))
                .map(move |filter| (source, filter))
        });
        for (declared_in, filter) in filters {
            let in_filter = |line: u32| filter.lines.contains(&line);
            let lambda = declared_in.authorizing(in_filter);
            asks.extend(lambda.map(|call| (declared_in.record_classes(tree, call), None)));
            // A lambda's own call to a method that authorizes, and the
            // constant it hands that method.
            let called = declared_in.facts.calls.iter().filter(|call| {
                in_filter(call.pos.line)
                    && call.recv == crate::core::RecvShape::Implicit
                    && call.name != "authorize"
            });
            let callbacks = filter
                .callbacks
                .iter()
                .map(|callback| (callback.clone(), None))
                .chain(called.map(|call| {
                    let hint = declared_in.argument(call).and_then(constant_at);
                    (call.name.clone(), hint)
                }));
            for (callback, hint) in callbacks {
                for source in &sources {
                    if let Some(def) = source.method(&callback) {
                        let body = def.pos.line..=def.end_line;
                        let calls = source.authorizing(|line| body.contains(&line));
                        asks.extend(
                            calls.map(|call| (source.record_classes(tree, call), hint.clone())),
                        );
                    }
                }
            }
        }
    }
    let mut records: Vec<Option<String>> = Vec::new();
    for (read, hint) in asks {
        let mut found: Vec<Option<String>> = read.into_iter().map(Some).collect();
        if found.is_empty() {
            found.extend(hint.map(Some));
        }
        // A record no reading types (`authorize model`) is the controller's
        // resource, when it has a policy: the controller is named for it.
        if found.is_empty() {
            found = controllers
                .iter()
                .filter_map(|c| resource_of(c))
                .filter(|resource| policy_of(tree, resource).is_some())
                .map(Some)
                .collect();
        }
        if found.is_empty() {
            found.push(None);
        }
        for class in found {
            if !records.contains(&class) {
                records.push(class);
            }
        }
    }
    records
}

/// `authorize record`, the record alone: Pundit then asks for the action.
fn by_record(call: &crate::core::Call) -> bool {
    call.name == "authorize" && call.argc == Some(1) && call.recv != crate::core::RecvShape::Symbol
}

/// `Admin::WidgetsController` → `Widget`.
fn resource_of(controller: &str) -> Option<String> {
    let plain = crate::tree::public_name(controller);
    let plain = plain.rsplit("::").next()?.strip_suffix("Controller")?;
    let snake: String = plain
        .chars()
        .enumerate()
        .flat_map(|(i, c)| {
            let gap = (i > 0 && c.is_uppercase()).then_some('_');
            gap.into_iter().chain(c.to_lowercase())
        })
        .collect();
    let last = snake.rsplit('_').next()?;
    let stem = &snake[..snake.len() - last.len()];
    Some(crate::extract::camelize(&format!(
        "{stem}{}",
        crate::inflect::singular(last)
    )))
}

/// A controller file, read for Pundit.
struct Source {
    text: String,
    facts: crate::core::Facts,
}

/// A `before_action` and the actions it runs for.
struct Filter {
    callbacks: Vec<String>,
    only: Option<Vec<String>>,
    except: Vec<String>,
    /// The lines the declaration spans, where a lambda's body is.
    lines: std::ops::RangeInclusive<u32>,
}

impl Filter {
    fn runs_for(&self, action: &str) -> bool {
        self.only
            .as_ref()
            .is_none_or(|only| only.iter().any(|a| a == action))
            && !self.except.iter().any(|a| a == action)
    }
}

impl Source {
    fn read(path: &str) -> Option<Source> {
        let text = std::fs::read_to_string(path).ok()?;
        let facts = crate::extract::extract(text.as_bytes());
        Some(Source { text, facts })
    }

    fn method(&self, name: &str) -> Option<&crate::core::Def> {
        self.facts
            .defs
            .iter()
            .find(|def| def.name == name && def.kind == crate::core::Kind::Method && !def.singleton)
    }

    /// Its `authorize record` calls on the lines `within` accepts.
    fn authorizing(
        &self,
        within: impl Fn(u32) -> bool,
    ) -> impl Iterator<Item = &crate::core::Call> {
        self.facts
            .calls
            .iter()
            .filter(move |call| by_record(call) && within(call.pos.line))
    }

    /// The first argument of `call`, as written on its line.
    fn argument(&self, call: &crate::core::Call) -> Option<&str> {
        let line = self.line(call.pos.line);
        let at = line.find(call.name.as_str())?;
        let arg = line[at + call.name.len()..]
            .trim_start_matches(['(', ' '])
            .split([',', ')', '}'])
            .next()?
            .split(" if ")
            .next()?
            .split(" unless ")
            .next()?
            .trim();
        (!arg.is_empty()).then_some(arg)
    }

    fn line(&self, line: u32) -> &str {
        self.text.lines().nth(line as usize - 1).unwrap_or_default()
    }

    /// Each `before_action :x, only: [:a]` declaration, read as text: its
    /// line and those it continues on while one ends in a comma.
    fn before_actions(&self) -> Vec<Filter> {
        let lines: Vec<&str> = self.text.lines().collect();
        let mut filters = Vec::new();
        let mut at = 0;
        while at < lines.len() {
            let code = lines[at].trim_start();
            let declares = [
                "before_action",
                "prepend_before_action",
                "append_before_action",
            ]
            .iter()
            .any(|word| {
                code.strip_prefix(word)
                    .is_some_and(|rest| rest.starts_with([' ', '(']))
            });
            if !declares {
                at += 1;
                continue;
            }
            let first = at;
            let mut statement = lines[at].to_string();
            while statement.trim_end().ends_with(',') && at + 1 < lines.len() {
                at += 1;
                statement.push(' ');
                statement.push_str(lines[at]);
            }
            let (callbacks, options) = match statement.find(" only:").or(statement.find(" except:"))
            {
                Some(cut) => statement.split_at(cut),
                None => (statement.as_str(), ""),
            };
            filters.push(Filter {
                callbacks: symbols_in(callbacks, false),
                // A list it cannot read (a constant) may name the action.
                only: options
                    .find("only:")
                    .map(|i| actions_in(&options[i + 5..]))
                    .filter(|only| !only.is_empty()),
                except: options
                    .find("except:")
                    .map(|i| actions_in(&options[i + 7..]))
                    .unwrap_or_default(),
                lines: first as u32 + 1..=at as u32 + 1,
            });
            at += 1;
        }
        filters
    }

    /// The classes the record handed to `authorize` may be: a constant
    /// written there, or a variable's assignments that start with one, or
    /// failing those a class its name spells (`@post` → `Post`).
    fn record_classes(&self, tree: &Tree, call: &crate::core::Call) -> Vec<String> {
        let Some(arg) = self.argument(call) else {
            return Vec::new();
        };
        if let Some(constant) = constant_at(arg) {
            return vec![constant];
        }
        let variable = arg.trim_start_matches('@');
        let is_name = !variable.is_empty()
            && variable
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if !is_name {
            return Vec::new();
        }
        let mut classes: Vec<String> = Vec::new();
        for assign in self.facts.assigns.iter().filter(|a| a.target == arg) {
            let written = self.line(assign.pos.line);
            let value = written
                .split_once('=')
                .map(|(_, value)| value.trim_start())
                .unwrap_or_default();
            if let Some(constant) = constant_at(value)
                && !classes.contains(&constant)
            {
                classes.push(constant);
            }
        }
        if classes.is_empty() {
            let spelled = crate::extract::camelize(&crate::inflect::singular(variable));
            if tree.resolve(&spelled, &[]).fqn.is_some() {
                classes.push(spelled);
            }
        }
        classes
    }
}

/// The constant path a piece of code starts with: `Widget` of `Widget.new`.
fn constant_at(code: &str) -> Option<String> {
    let code = code.trim_start_matches("::");
    if !code.starts_with(|c: char| c.is_ascii_uppercase()) {
        return None;
    }
    let end = code
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
        .unwrap_or(code.len());
    Some(code[..end].trim_end_matches(':').to_string())
}

/// What an `only:` or `except:` lists: `[:a, :b]`, `%i[a b]` or `:a`.
fn actions_in(options: &str) -> Vec<String> {
    let options = options.trim_start();
    if let Some(rest) = options.strip_prefix("%i[") {
        return symbols_in(rest.split(']').next().unwrap_or_default(), true);
    }
    if let Some(rest) = options.strip_prefix('[') {
        return symbols_in(rest.split(']').next().unwrap_or_default(), false);
    }
    symbols_in(options.split([',', ')']).next().unwrap_or_default(), false)
}

/// The names a piece of code writes as symbols, or as `%i[]`'s bare words.
fn symbols_in(code: &str, bare: bool) -> Vec<String> {
    let words = code.split(|c: char| {
        !(c.is_ascii_alphanumeric() || c == '_' || c == ':' || c == '?' || c == '!')
    });
    words
        .filter_map(|word| match word.strip_prefix(':') {
            Some(name) if !name.is_empty() && !name.contains(':') => Some(name.to_string()),
            _ if bare && !word.is_empty() => Some(word.to_string()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_controllers_resource_is_its_last_word_singularized() {
        assert_eq!(
            resource_of("Admin::BlogPostsController").as_deref(),
            Some("BlogPost")
        );
        assert_eq!(resource_of("PeopleController").as_deref(), Some("Person"));
        assert_eq!(resource_of("WidgetHelper"), None);
    }

    #[test]
    fn a_filters_action_list_is_read_in_each_spelling() {
        assert_eq!(actions_in(" [:show, :edit], if: :x?"), ["show", "edit"]);
        assert_eq!(actions_in(" %i[show edit]"), ["show", "edit"]);
        assert_eq!(actions_in(" :show"), ["show"]);
        assert!(
            actions_in(" BILLING_ACTIONS").is_empty(),
            "a constant is not read"
        );
    }
}
