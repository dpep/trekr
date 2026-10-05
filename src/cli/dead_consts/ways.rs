//! What reaches a class, module or constant by its name rather than by a
//! reference — a route, a framework's or a library's naming convention, a
//! registry, a string — and what may, which a row says as a caveat
//! (DEC-421).

#![expect(
    clippy::disallowed_methods,
    reason = "reads here predate scan::read_source; converting the last one fails this expect"
)]

use std::collections::{HashMap, HashSet};

use super::named::{self, Named, plain};
use super::{in_tests, spellings};
use crate::cli::conventions::Convention;
use crate::cli::routes::{self, Routes};
use crate::cli::views::Views;
use crate::tree::Tree;

/// Each controller a route reaches, with the first route that does: the class
/// its path camelizes to, matched without case or underscores, an engine's
/// own first — as DEC-344 finds a route's action. A routes file's string
/// that spells a controller's path reaches it too: a gem's routes
/// (`devise_for :users, controllers: { sessions: 'users/sessions' }`).
///
/// A controller is also known by the name its file spells, as Zeitwerk loads
/// it: `module Admin; class Users::RolesController` in
/// `admin/users/roles_controller.rb` is the route's `admin/users/roles`.
/// The checkout files a RuboCop config loads: each `require:` entry that is
/// a path (`./rubocop/my_cop.rb`, `lib/cops/x`), relative to the checkout.
pub(super) fn rubocop_requires(root: &std::path::Path) -> HashMap<String, named::At> {
    let mut found = HashMap::new();
    for config in [".rubocop.yml", ".standard.yml"] {
        let Ok(text) = std::fs::read_to_string(root.join(config)) else {
            continue;
        };
        let mut listing = false;
        for (n, line) in text.lines().enumerate() {
            if !line.starts_with([' ', '-']) {
                listing = line.trim_end() == "require:";
                continue;
            }
            let Some(entry) = line.trim().strip_prefix('-').map(str::trim) else {
                continue;
            };
            if !listing {
                continue;
            }
            let entry = entry.trim_matches(['"', '\'']);
            let path = entry.trim_start_matches("./");
            if !(entry.starts_with('.') || entry.contains('/')) {
                continue; // a gem: `rubocop-rails`
            }
            let path = if path.ends_with(".rb") {
                path.to_string()
            } else {
                format!("{path}.rb")
            };
            found.insert(path, (config.to_string(), n as u32 + 1));
        }
    }
    found
}

/// A controller's name compared segment by segment, each as `plain` compares
/// it: `Admin::WidgetsController` is `admin/widgetscontroller`, and is not
/// `AdminWidgetsController`.
fn controller_key(name: &str) -> String {
    name.trim_start_matches("::")
        .replace("::", "/")
        .split('/')
        .map(plain)
        .collect::<Vec<_>>()
        .join("/")
}

pub(super) fn routed_controllers(
    tree: &Tree,
    all: &[(String, String)],
    routes: &Routes,
    named: &Named,
) -> HashMap<String, routes::At> {
    let mut controllers: HashMap<String, String> = HashMap::new();
    for (fqn, kind) in all {
        if kind != "class" || !fqn.ends_with("Controller") {
            continue;
        }
        for site in tree.sites(fqn) {
            if let Some((_, file)) = site.path.rsplit_once("/controllers/")
                && let Some(file) = file.strip_suffix(".rb")
            {
                controllers
                    .entry(controller_key(file))
                    .or_insert_with(|| fqn.clone());
            }
        }
        controllers.insert(controller_key(fqn), fqn.clone());
    }
    let mut routed = HashMap::new();
    for route in &routes.routes {
        let class = route.controllers.iter().find_map(|controller| {
            let path = format!("{}controller", controller.replace('/', "::"));
            let engine = route
                .engine
                .as_ref()
                .map(|engine| format!("{engine}::{path}"));
            engine
                .iter()
                .chain(std::iter::once(&path))
                .find_map(|name| controllers.get(&controller_key(name)))
        });
        if let Some(class) = class {
            routed
                .entry(class.clone())
                .or_insert_with(|| route.at.clone());
        }
    }
    // The earliest string that names a controller is the one cited.
    let mut strings: Vec<_> = named.route_strings.iter().collect();
    strings.sort_by(|a, b| a.1.cmp(b.1));
    for (path, at) in strings {
        if let Some(class) = controllers.get(&controller_key(path)) {
            routed.entry(class.clone()).or_insert_with(|| at.clone());
        }
    }
    routed
}

/// The listing of `name`, or of a namespace it is written as the tail of:
/// the longest, so `Lexer::Punctuation`'s is cited before a `Punctuation`'s,
/// whatever order the map holds them in.
fn listing_of<'m, V>(map: &'m HashMap<String, V>, name: &str) -> Option<(&'m String, &'m V)> {
    map.iter()
        .filter(|(listed, _)| name == *listed || name.ends_with(&format!("::{listed}")))
        .max_by_key(|(listed, _)| listed.len())
}

/// A class's last segment less the suffix a library finds it by:
/// `WidgetSerializer` is `Widget`'s.
fn stem<'a>(fqn: &'a str, suffix: &str) -> Option<&'a str> {
    let tail = fqn.rsplit("::").next().unwrap_or(fqn);
    tail.strip_suffix(suffix).filter(|stem| !stem.is_empty())
}

/// A convention: who follows it, the row's reason, and the line that names
/// the class, when one does.
fn convention(by: &'static str, reason: String, at: Option<&named::At>) -> Option<Convention> {
    Some(Convention {
        by,
        reason,
        at: at.cloned(),
    })
}

/// The relation classes Active Record makes per model by name
/// (`relation_delegate_class`).
const RELATIONS: [&str; 3] = [
    "ActiveRecord_Relation",
    "ActiveRecord_AssociationRelation",
    "ActiveRecord_Associations_CollectionProxy",
];

/// The classes a test case inherits, whose runner finds it.
const TEST_CASES: [&str; 4] = [
    "Minitest::Test",
    "Test::Unit::TestCase",
    "ActiveSupport::TestCase",
    "Minitest::Spec",
];

/// What reaches a constant by its name rather than by a reference, and the
/// caveats on one nothing seems to reach.
pub(super) struct Ways<'a> {
    pub(super) tree: &'a Tree,
    pub(super) routes: &'a Routes,
    pub(super) views: &'a Views,
    pub(super) named: Named,
    pub(super) routed: HashMap<String, routes::At>,
    /// The checkout's root, with a trailing `/`.
    pub(super) prefix: String,
    /// The tails of the checkout's classes and modules, `plain`: what a
    /// library's naming convention may name.
    pub(super) classes: HashSet<String>,
    /// How many of the checkout's constants end in each tail: a string of
    /// one segment names one of them only when it is the only one.
    pub(super) tails: HashMap<String, usize>,
    /// Each namespace whose own files list its constants, read once.
    pub(super) own_listings: std::cell::RefCell<HashMap<String, Option<(String, u32)>>>,
    /// The checkout's classes, by the namespace that holds them.
    pub(super) children: HashMap<String, Vec<String>>,
    /// The checkout files `.rubocop.yml` or `.standard.yml` loads by
    /// `require:` — custom cops — and the line that names each.
    pub(super) rubocop: HashMap<String, named::At>,
    /// The constants each file outside the checkout reads on a value, read
    /// once.
    pub(super) foreign_reads: std::cell::RefCell<HashMap<String, HashMap<String, named::At>>>,
}

impl Ways<'_> {
    fn inherits(&self, fqn: &str, ancestor: &str) -> bool {
        self.tree.inherits(fqn, ancestor)
    }

    /// Where a whole string spells `fqn`: the name in full, a path ending in
    /// it, or its last segment when no other constant of the checkout ends
    /// so.
    fn string_naming(&self, fqn: &str) -> Option<(&str, &named::At)> {
        let tail = fqn.rsplit("::").next().unwrap_or(fqn);
        spellings(fqn)
            .into_iter()
            .filter(|spelling| {
                spelling.contains("::") || spelling != tail || self.tails.get(tail) == Some(&1)
            })
            .find_map(|spelling| self.named.strings.get_key_value(&spelling))
            .map(|(written, at)| (written.as_str(), at))
    }

    /// A convention that names it, by who follows it.
    pub(super) fn convention(
        &self,
        fqn: &str,
        kind: &str,
        relative: &str,
        line: u32,
    ) -> Option<Convention> {
        let tree = self.tree;
        let named = &self.named;
        if let Some(at) = self.routed.get(fqn) {
            return convention(
                "routes",
                format!("named only by a route, at {}:{}", at.0, at.1),
                Some(at),
            );
        }
        if let Some(at) = self.rubocop.get(relative) {
            return convention(
                "RuboCop",
                format!(
                    "in a file RuboCop loads by `require:`, at {}:{}",
                    at.0, at.1
                ),
                Some(at),
            );
        }
        if kind == "module" && relative.starts_with("app/helpers/") {
            return convention(
                "Rails helpers",
                "a helper, which Rails mixes into every view".to_string(),
                None,
            );
        }
        // ActiveSupport::Concern extends its includers with `ClassMethods`,
        // which it finds by name.
        if let Some(concern) = fqn.strip_suffix("::ClassMethods")
            && tree
                .lookup(concern, true, "append_features")
                .is_some_and(|m| m.owner == "ActiveSupport::Concern")
        {
            return convention(
                "ActiveSupport::Concern",
                format!(
                    "the class methods ActiveSupport::Concern extends {concern}'s includers with"
                ),
                None,
            );
        }
        // Active Record makes a model's relation classes by these names and
        // reopens one a model already defines.
        if let Some((model, relation)) = fqn.rsplit_once("::")
            && RELATIONS.contains(&relation)
            && self.inherits(model, "ActiveRecord::Base")
        {
            return convention(
                "Active Record",
                format!("{model}'s relation class, which Active Record makes and reopens by name"),
                None,
            );
        }
        // Rails runs what inherits these by inheriting them.
        for (base, by, what) in [
            (
                "Rails::Railtie",
                "Rails",
                "a Railtie, which Rails runs because it inherits one",
            ),
            (
                "Rails::Generators::Base",
                "Rails generators",
                "a generator, which Rails finds by its path",
            ),
            (
                "ActiveRecord::Migration",
                "Rails migrations",
                "a migration, which Rails runs by its file",
            ),
            (
                "ActionCable::Channel::Base",
                "Action Cable",
                "a channel, which a client subscribes to by name",
            ),
            (
                "ActionMailer::Preview",
                "Action Mailer",
                "a mailer preview, which Rails finds by its path",
            ),
            (
                "ActionCable::Connection::Base",
                "Action Cable",
                "a connection class, which Action Cable finds by name",
            ),
        ] {
            if self.inherits(fqn, base) {
                return convention(by, what.to_string(), None);
            }
        }
        if tree
            .lookup(fqn, true, "every")
            .is_some_and(|m| m.owner == "MiniScheduler::Schedule")
        {
            return convention(
                "MiniScheduler",
                "a scheduled job, which MiniScheduler runs".to_string(),
                None,
            );
        }
        if let Some(found) = self.library_convention(fqn, relative) {
            return Some(found);
        }
        // A name built from a symbol the checkout writes: `"Jobs::#{type
        // .camelize}".constantize` and `Jobs.enqueue(:purge_widgets)`.
        for (shape, at) in &named.shapes {
            if !crate::core::shape_matches(shape, fqn) {
                continue;
            }
            let (before, after) = shape.split_once('*').unwrap_or((shape, ""));
            let built = &fqn[before.len()..fqn.len() - after.len()];
            if let Some(from) = named.symbol_outside(&plain(built), relative) {
                return convention(
                    "constantize",
                    format!(
                        "built at runtime (`{shape}` at {}:{}) from a name written at {}:{}",
                        at.0, at.1, from.0, from.1
                    ),
                    Some(at),
                );
            }
        }
        // `"#{self.class.name}Drop".constantize`: built from a class's name,
        // so a class of the checkout spells the built part.
        for (shape, at) in &named.class_shapes {
            if !crate::core::shape_matches(shape, fqn) {
                continue;
            }
            let (before, after) = shape.split_once('*').unwrap_or((shape, ""));
            let built = &fqn[before.len()..fqn.len() - after.len()];
            let class = built.rsplit("::").next().unwrap_or(built);
            if !after.is_empty() && self.classes.contains(&plain(class)) {
                return convention(
                    "constantize",
                    format!(
                        "built at runtime (`{shape}` at {}:{}) from a class's name ({built})",
                        at.0, at.1
                    ),
                    Some(at),
                );
            }
        }
        let tail = fqn.rsplit("::").next().unwrap_or(fqn);
        if self.inherits(fqn, "ActiveRecord::Base")
            && let Some((written, at)) = named.associated.get(&plain(tail))
        {
            return convention(
                "association",
                format!(
                    "named only by an association's name ({written} at {}:{})",
                    at.0, at.1
                ),
                Some(at),
            );
        }
        // A test framework loads a test by its file and runs its cases by
        // what they inherit.
        if kind == "class" && in_tests(relative) {
            let file = relative.rsplit('/').next().unwrap_or(relative);
            let base = TEST_CASES.iter().find(|base| self.inherits(fqn, base));
            if base.is_some()
                || file.starts_with("test_")
                || file.ends_with("_test.rb")
                || file.ends_with("_spec.rb")
            {
                return convention(
                    "test runner",
                    "a test case, which its runner loads by file and runs".to_string(),
                    None,
                );
            }
        }
        // A class an ancestor registers as it is defined or mixed in: an
        // `inherited` or `included` hook written in the checkout that keeps
        // what it is handed, or a listing of the ancestor's subclasses.
        for ancestor in &tree.ancestors(fqn).chain {
            let ancestor = crate::tree::public_name(ancestor);
            if ancestor == fqn || !self.in_checkout(ancestor) {
                continue;
            }
            if let Some((_, at)) = listing_of(&named.listed, ancestor) {
                return convention(
                    "subclasses",
                    format!(
                        "a subclass of {ancestor}, whose subclasses are listed at {}:{}",
                        at.0, at.1
                    ),
                    Some(at),
                );
            }
            for hook in ["inherited", "included"] {
                if let Some(method) = tree.lookup(ancestor, true, hook)
                    && method.owner == ancestor
                    && keeps_its_argument(&method.site.path, method.site.line)
                {
                    let relative = method
                        .site
                        .path
                        .strip_prefix(&self.prefix)
                        .unwrap_or(&method.site.path);
                    let at = (relative.to_string(), method.site.line);
                    return convention(
                        "registration",
                        format!(
                            "{ancestor}'s `{hook}` hook registers it, at {}:{}",
                            at.0, at.1
                        ),
                        Some(&at),
                    );
                }
            }
        }
        if kind != "constant"
            && let Some(namespace) = self.gem_namespace(fqn)
            // In its own file too: a plugin defines a strategy and registers
            // it by its symbol in one place.
            && let Some(at) = named.symbols.get(&plain(tail)).and_then(|ats| ats.first())
        {
            return convention(
                "gem namespace",
                format!(
                    "added to {namespace}, a gem's namespace, and named by a symbol at {}:{}",
                    at.0, at.1
                ),
                Some(at),
            );
        }
        if kind != "constant"
            && let Some(at) = self.registration(&format!("{}{relative}", self.prefix), line)
        {
            return convention(
                "registration",
                format!(
                    "its body hands it to another's method as it loads, at {}:{}",
                    at.0, at.1
                ),
                Some(&at),
            );
        }
        if let Some((written, at)) = self.string_naming(fqn) {
            return convention(
                "string",
                format!("named only by a string, \"{written}\" at {}:{}", at.0, at.1),
                Some(at),
            );
        }
        None
    }

    /// A library that finds a class by a name it builds from another's, when
    /// the library is in the tree: ActiveModel::Serializers, Pundit, Draper,
    /// ActiveModel's `validates`, simple_form.
    fn library_convention(&self, fqn: &str, relative: &str) -> Option<Convention> {
        let tree = self.tree;
        let known = |stem: &str| self.classes.contains(&plain(stem));
        let symbol = |stem: &str| {
            self.named
                .symbol_outside(&plain(stem), relative)
                .cloned()
                .or_else(|| {
                    self.views
                        .naming(&underscore(stem))
                        .map(|path| (path.to_string(), 0))
                })
        };
        if let Some(stem) = stem(fqn, "Serializer")
            && self.inherits(fqn, "ActiveModel::Serializer")
            && known(stem)
        {
            return convention(
                "ActiveModel::Serializers",
                format!(
                    "a serializer ActiveModel::Serializers finds by its object's class ({stem})"
                ),
                None,
            );
        }
        if let Some(stem) = stem(fqn, "Policy")
            && tree.is_known("Pundit")
            && (known(stem) || symbol(stem).is_some())
        {
            return convention(
                "Pundit",
                format!("a policy Pundit finds by its record's name ({stem})"),
                None,
            );
        }
        if let Some(stem) = stem(fqn, "Mailbox")
            && self.inherits(fqn, "ActionMailbox::Base")
            && let Some(at) = symbol(stem)
        {
            return convention(
                "Action Mailbox",
                format!(
                    "a mailbox Action Mailbox's `routing` names by a symbol, at {}:{}",
                    at.0, at.1
                ),
                Some(&at),
            );
        }
        if let Some(stem) = stem(fqn, "Dashboard")
            && self.inherits(fqn, "Administrate::BaseDashboard")
            && known(stem)
        {
            return convention(
                "Administrate",
                format!("a dashboard Administrate finds by its resource's class ({stem})"),
                None,
            );
        }
        if let Some(stem) = stem(fqn, "Decorator")
            && self.inherits(fqn, "Draper::Decorator")
            && known(stem)
        {
            return convention(
                "Draper",
                format!("a decorator Draper finds by its object's class ({stem})"),
                None,
            );
        }
        for (suffix, base, by, how) in [
            (
                "Validator",
                "ActiveModel::Validator",
                "ActiveModel validates",
                "`validates`",
            ),
            (
                "Input",
                "SimpleForm::Inputs::Base",
                "simple_form",
                "simple_form's `as:`",
            ),
        ] {
            if let Some(stem) = stem(fqn, suffix)
                && self.inherits(fqn, base)
                && let Some(at) = symbol(stem)
            {
                return convention(
                    by,
                    format!("found by {how} from its name, written at {}:{}", at.0, at.1),
                    Some(&at),
                );
            }
        }
        None
    }

    /// Where an ancestor outside the checkout — a gem's base class — reads
    /// this constant on a value: Administrate's `BaseDashboard` reads
    /// `self.class::COLLECTION_ATTRIBUTES` of every dashboard.
    fn read_by_a_foreign_ancestor(&self, fqn: &str) -> Option<named::At> {
        let (owner, tail) = fqn.rsplit_once("::")?;
        for ancestor in &self.tree.ancestors(owner).chain {
            for site in self.tree.sites(ancestor) {
                if self.tree.in_checkout(&site.path) {
                    continue;
                }
                let mut reads = self.foreign_reads.borrow_mut();
                let read = reads
                    .entry(site.path.clone())
                    .or_insert_with(|| Named::read_on_a_value(&site.path));
                if let Some(at) = read.get(tail) {
                    return Some(at.clone());
                }
            }
        }
        None
    }

    /// Why one nothing reaches may still be reached.
    pub(super) fn caveats(&self, fqn: &str) -> Vec<String> {
        let mut caveats = Vec::new();
        // A class named `…Controller` elsewhere — a RuboCop cop — is no route's.
        let controller = self.inherits(fqn, "ActionController::Metal")
            || fqn.ends_with("Controller")
                && self
                    .tree
                    .sites(fqn)
                    .iter()
                    .any(|site| site.path.contains("/controllers/"));
        if controller {
            let unread = match self.routes.unread.first() {
                _ if self.routes.files == 0 => Some("no routes file read".to_string()),
                Some(((path, line), why)) => Some(format!("{why} at {path}:{line}")),
                None => None,
            };
            if let Some(unread) = unread {
                caveats.push(format!("a controller routes may reach ({unread})"));
            }
            if let Some((call, at)) = self.named.gem_routes.first() {
                caveats.push(format!(
                    "a gem's routes, which are not read, may reach it ({call} at {}:{})",
                    at.0, at.1
                ));
            }
        }
        // A framework that has not been indexed may find what inherits it.
        let unseen: Vec<String> = self
            .tree
            .ancestors(fqn)
            .unresolved
            .iter()
            .filter(|name| !self.tree.is_known(name))
            .cloned()
            .collect();
        if !unseen.is_empty() {
            caveats.push(format!(
                "it inherits {}, which trekr has not indexed",
                unseen.join(", ")
            ));
        }
        if let Some(namespace) = self.gem_namespace(fqn) {
            caveats.push(format!(
                "it is added to {namespace}, a gem's namespace, where the gem may find it by name"
            ));
        }
        let tail = fqn.rsplit("::").next().unwrap_or(fqn);
        if let Some(at) = self
            .named
            .dynamic
            .get(tail)
            .cloned()
            .or_else(|| self.read_by_a_foreign_ancestor(fqn))
        {
            caveats.push(format!(
                "a `{tail}` is read on a value, which trekr does not resolve, at {}:{}",
                at.0, at.1
            ));
        }
        if let Some((namespace, (call, at))) = fqn
            .rsplit_once("::")
            .and_then(|(namespace, _)| listing_of(&self.named.constants_listed, namespace))
        {
            let how = match call.as_str() {
                "constants" => "listed",
                _ => "looked up by a name computed",
            };
            caveats.push(format!(
                "its namespace's constants are {how} at runtime ({namespace}.{call} at {}:{})",
                at.0, at.1
            ));
        } else if let Some((namespace, _)) = fqn.rsplit_once("::")
            && let Some(at) = self.lists_own_constants(namespace)
        {
            caveats.push(format!(
                "its namespace lists its own constants at runtime, at {}:{}",
                at.0, at.1
            ));
        } else if let Some((namespace, _)) = fqn.rsplit_once("::")
            && let Some((sibling, at)) = self.namespace_listed_by_sibling(namespace, fqn)
        {
            caveats.push(format!(
                "{sibling}'s ancestor lists its namespace's constants at runtime, at {}:{}",
                at.0, at.1
            ));
        }
        // A factory in an ancestor builds the name of the class it makes.
        if let Some((ancestor, path, line)) = self
            .tree
            .ancestors(fqn)
            .chain
            .iter()
            .map(|a| crate::tree::public_name(a))
            .filter(|a| *a != fqn)
            .find_map(|ancestor| {
                self.tree.sites(ancestor).iter().find_map(|site| {
                    let relative = site.path.strip_prefix(&self.prefix)?;
                    let line = self.named.computed_in.get(relative)?;
                    Some((ancestor, relative.to_string(), *line))
                })
            })
        {
            caveats.push(format!(
                "{ancestor} looks a class up by a name it computes, at {path}:{line}"
            ));
        }
        // Active Record instantiates a model's subclass by its `type` column.
        if self.inherits(fqn, "ActiveRecord::Base")
            && let Some(parent) = self
                .tree
                .ancestors(fqn)
                .chain
                .iter()
                .map(|a| crate::tree::public_name(a))
                .find(|a| *a != fqn && self.tree.kind_of(a) == Some("class"))
            && self.inherits(parent, "ActiveRecord::Base")
            && self
                .tree
                .sites(parent)
                .iter()
                .any(|site| site.path.starts_with(&self.prefix))
            && !self.abstract_model(parent)
        {
            caveats.push(format!(
                "a subclass of {parent}, which Active Record instantiates by a `type` column"
            ));
        }
        if let Some((shape, at)) = self
            .named
            .shapes
            .iter()
            .find(|(shape, _)| crate::core::shape_matches(shape, fqn))
        {
            caveats.push(format!(
                "a name of its shape is built at runtime (`{shape}` at {}:{})",
                at.0, at.1
            ));
        }
        caveats
    }
}

impl Ways<'_> {
    /// Is `fqn` declared in the checkout?
    fn in_checkout(&self, fqn: &str) -> bool {
        self.tree
            .sites(fqn)
            .iter()
            .any(|site| site.path.starts_with(&self.prefix))
    }

    /// Where the namespace `fqn` is written in lists its own constants —
    /// `constants.select { … }` in one of its methods — the first file and
    /// line that does.
    fn lists_own_constants(&self, namespace: &str) -> Option<(String, u32)> {
        if let Some(known) = self.own_listings.borrow().get(namespace) {
            return known.clone();
        }
        // A call of `constants` on `self`, or on the `base` a module's
        // `extended` hook is handed.
        let lists = |path: &str, receivers: &[&str]| {
            let text = std::fs::read_to_string(path).ok()?;
            let line = text.lines().position(|line| {
                let code = line.split(" #").next().unwrap_or(line);
                code.match_indices("constants").any(|(at, _)| {
                    let before = &code[..at];
                    let after = code[at + "constants".len()..].chars().next();
                    let bare = !before.ends_with(|c: char| {
                        c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == ':'
                    });
                    (bare || receivers.iter().any(|r| before.ends_with(&format!("{r}."))))
                        && !after.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
                })
            })?;
            let relative = path.strip_prefix(&self.prefix).unwrap_or(path);
            Some((relative.to_string(), line as u32 + 1))
        };
        let sites = self.tree.sites(namespace);
        let found = sites
            .iter()
            .find_map(|site| lists(&site.path, &["self"]))
            .or_else(|| {
                // `extend Enumerated`, whose `extended(base)` lists `base`'s.
                sites.iter().find_map(|site| {
                    let text = std::fs::read_to_string(&site.path).ok()?;
                    text.lines().find_map(|line| {
                        let module = line.trim_start().strip_prefix("extend ")?.trim();
                        let fqn = self.tree.resolve(module, &[namespace.to_string()]).fqn?;
                        self.tree
                            .sites(&fqn)
                            .iter()
                            .find_map(|site| lists(&site.path, &["base", "self"]))
                    })
                })
            });
        self.own_listings
            .borrow_mut()
            .insert(namespace.to_string(), found.clone());
        found
    }

    /// A class beside `fqn` whose ancestor lists the constants of the
    /// namespace that holds it (`self.class.name.deconstantize.constantize
    /// .constants`) — a registry of steps that is its namespace.
    fn namespace_listed_by_sibling(
        &self,
        namespace: &str,
        fqn: &str,
    ) -> Option<(String, (String, u32))> {
        if self.named.lists_namespace.is_empty() {
            return None;
        }
        self.children
            .get(namespace)?
            .iter()
            .filter(|k| *k != fqn)
            .find_map(|sibling| {
                self.tree
                    .ancestors(sibling)
                    .chain
                    .iter()
                    .map(|a| crate::tree::public_name(a))
                    .filter(|a| a != sibling)
                    .find_map(|ancestor| {
                        self.tree.sites(ancestor).iter().find_map(|site| {
                            let relative = site.path.strip_prefix(&self.prefix)?;
                            let line = self.named.lists_namespace.get(relative)?;
                            Some((sibling.clone(), (relative.to_string(), *line)))
                        })
                    })
            })
    }

    /// The namespace a gem or Ruby declares that `fqn` is written into
    /// (`Chewy::Strategy`, `Paperclip`): a library looks such a class up by a
    /// name it builds.
    fn gem_namespace<'f>(&self, fqn: &'f str) -> Option<&'f str> {
        let (namespace, _) = fqn.rsplit_once("::")?;
        self.tree
            .sites(namespace)
            .iter()
            .any(|site| !site.path.starts_with(&self.prefix))
            .then_some(namespace)
    }

    /// `self.abstract_class = true` in a model's file, or the conventional
    /// `ApplicationRecord`: no table, so no `type` column.
    fn abstract_model(&self, fqn: &str) -> bool {
        fqn == "ApplicationRecord"
            || self.tree.sites(fqn).iter().any(|site| {
                std::fs::read_to_string(&site.path)
                    .is_ok_and(|text| text.contains("abstract_class = true"))
            })
    }

    /// Where the class's own body hands the class to another's method as it
    /// loads — `HTTP::Options.register_feature(:x, self)` — a registration
    /// that reaches it without its name.
    fn registration(&self, path: &str, line: u32) -> Option<(String, u32)> {
        let text = std::fs::read_to_string(path).ok()?;
        let lines: Vec<&str> = text.lines().collect();
        let opening = lines.get(line.checked_sub(1)? as usize)?;
        let indent = opening.len() - opening.trim_start().len();
        for (n, body) in lines.iter().enumerate().skip(line as usize) {
            let trimmed = body.trim_start();
            if trimmed.is_empty() {
                continue;
            }
            let depth = body.len() - trimmed.len();
            if depth <= indent {
                break;
            }
            let handed = ["(self)", "(self,", ", self)", ", self,"]
                .iter()
                .any(|shape| trimmed.contains(shape))
                || trimmed.ends_with(", self");
            if depth == indent + 2
                && handed
                && trimmed
                    .trim_start_matches("::")
                    .starts_with(|c: char| c.is_ascii_uppercase())
            {
                let relative = path.strip_prefix(&self.prefix).unwrap_or(path);
                return Some((relative.to_string(), n as u32 + 1));
            }
        }
        None
    }
}

/// Does the hook `def` at `path:line` keep what it is handed — `@@all <<
/// klass`, `registry.push(base)`, `register(base)` — rather than only extend
/// it?
fn keeps_its_argument(path: &str, line: u32) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let lines: Vec<&str> = text.lines().collect();
    let Some(opening) = lines.get((line as usize).saturating_sub(1)) else {
        return false;
    };
    let indent = opening.len() - opening.trim_start().len();
    lines[line as usize..]
        .iter()
        .take_while(|body| {
            let trimmed = body.trim_start();
            trimmed.is_empty() || body.len() - trimmed.len() > indent || !trimmed.starts_with("end")
        })
        .any(|body| {
            ["<<", ".push(", ".add(", "register", ".append("]
                .iter()
                .any(|keep| body.contains(keep))
        })
}

/// `WidgetPart` → `widget_part`, for a name a template writes.
fn underscore(word: &str) -> String {
    let mut out = String::with_capacity(word.len() + 4);
    for (at, c) in word.char_indices() {
        if c.is_ascii_uppercase() && at > 0 {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}
