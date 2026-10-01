//! The controller actions a checkout's routes reach: what `--dead` cannot
//! otherwise see call a public action, and says per row (DEC-344).
//!
//! A reading of Rails' routing DSL in `config/routes.rb` and the files it
//! `draw`s, not a run of it: `get 'x', to: 'c#a'`, `'x' => 'c#a'`,
//! `controller:`/`action:`, a bare path `'c/a'`, `resources`/`resource` with
//! their default actions less `only:`/`except:`, `member`/`collection`,
//! `concern`/`concerns`, and the module `namespace` and `scope module:` nest
//! a controller in. A route it cannot read — a name built at runtime, a
//! `:controller` segment — is listed, so a row can say routes were not all
//! read rather than that none reaches it.

use crate::extract::line_index::LineIndex;
use ruby_prism::{Node, Visit};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Where a route is written: the file, relative to the checkout, and line.
pub(super) type At = (String, u32);

#[derive(Default)]
pub(super) struct Routes {
    /// Each route: its controller's path (`api/v1/widgets`), the engine
    /// whose routes it is (`Billing`), its action, where it is written.
    pub(super) routes: Vec<Route>,
    /// Routes that could not be read, and why.
    pub(super) unread: Vec<(At, &'static str)>,
    /// The routes files read.
    pub(super) files: usize,
}

pub(super) struct Route {
    pub(super) controller: String,
    pub(super) engine: Option<String>,
    pub(super) action: String,
    pub(super) at: At,
}

const RESOURCES: [&str; 7] = [
    "index", "create", "new", "edit", "show", "update", "destroy",
];
const RESOURCE: [&str; 6] = ["create", "new", "edit", "show", "update", "destroy"];

impl Routes {
    /// Every `config/routes.rb` git knows in the checkout at `root` — the
    /// app's and each engine's — and what they draw.
    pub(super) fn read(root: &Path) -> Routes {
        let mut routes = Routes::default();
        let Ok(out) = Command::new("git")
            .arg("-C")
            .arg(root)
            .args([
                "ls-files",
                "-z",
                "--",
                "config/routes.rb",
                "*/config/routes.rb",
            ])
            .output()
        else {
            return routes;
        };
        for path in out.stdout.split(|b| *b == 0).filter(|p| !p.is_empty()) {
            let path = String::from_utf8_lossy(path).into_owned();
            routes.read_file(root, &path, &Scope::default(), 0);
        }
        routes
    }

    /// Read one routes file in `scope`. `depth` bounds `draw`s that draw.
    fn read_file(&mut self, root: &Path, path: &str, scope: &Scope, depth: usize) {
        let Ok(source) = std::fs::read(root.join(path)) else {
            return;
        };
        self.files += 1;
        self.read_source(root, path, &source, scope, depth);
    }

    fn read_source(&mut self, root: &Path, path: &str, source: &[u8], scope: &Scope, depth: usize) {
        let parsed = ruby_prism::parse(source);
        let lines = LineIndex::new(source);
        let mut reader = Reader {
            routes: self,
            root,
            path,
            source,
            lines: &lines,
            scopes: vec![scope.clone()],
            depth,
            concerns: HashMap::new(),
        };
        reader.visit(&parsed.node());
    }

    /// Write the actions of `controller` (a path) in `scope` reached by default.
    fn push(&mut self, scope: &Scope, controller: String, action: &str, at: &At) {
        self.routes.push(Route {
            controller,
            engine: scope.engine.clone(),
            action: action.replace('-', "_"),
            at: at.clone(),
        });
    }
}

/// What the blocks around a route say about it.
#[derive(Clone, Default)]
struct Scope {
    /// Module path a controller is nested in: `namespace :api`.
    module: Vec<String>,
    /// The controller a bare action goes to: `controller :x`, `scope
    /// controller:`, or the resource whose block this is.
    controller: Option<String>,
    /// The engine whose `routes.draw` this is (`Billing::Engine`).
    engine: Option<String>,
}

struct Reader<'r, 'a> {
    routes: &'r mut Routes,
    root: &'a Path,
    path: &'a str,
    source: &'a [u8],
    lines: &'a LineIndex,
    scopes: Vec<Scope>,
    depth: usize,
    /// Each `concern`'s block, as source, read again where it is used.
    concerns: HashMap<String, Vec<u8>>,
}

/// A literal name: a symbol's or a string's text.
fn literal(node: &Node<'_>) -> Option<String> {
    if let Some(symbol) = node.as_symbol_node() {
        return String::from_utf8(symbol.unescaped().to_vec()).ok();
    }
    node.as_string_node()
        .and_then(|s| String::from_utf8(s.unescaped().to_vec()).ok())
}

/// Literal names, one or a list; `None` when any is not literal.
fn literals(node: &Node<'_>) -> Option<Vec<String>> {
    match node.as_array_node() {
        Some(list) => list.elements().iter().map(|e| literal(&e)).collect(),
        None => literal(node).map(|name| vec![name]),
    }
}

/// A controller written in `module`.
fn controller_in(module: &[String], written: &str) -> String {
    if let Some(top) = written.strip_prefix('/') {
        return top.to_string();
    }
    let mut path = module.join("/");
    if !path.is_empty() {
        path.push('/');
    }
    path.push_str(written);
    path
}

/// `widgets` for `resource :widget`, as Rails' controller for a singular
/// resource is plural. The common English rules only.
fn plural(name: &str) -> String {
    if let Some(stem) = name.strip_suffix('y')
        && !stem.ends_with(['a', 'e', 'i', 'o', 'u'])
    {
        return format!("{stem}ies");
    }
    if name.ends_with('s') || name.ends_with('x') || name.ends_with("ch") || name.ends_with("sh") {
        return format!("{name}es");
    }
    format!("{name}s")
}

impl<'pr> Reader<'_, '_> {
    fn scope(&self) -> &Scope {
        self.scopes
            .last()
            .expect("a file's own scope is never popped")
    }

    fn at(&self, node: &Node<'_>) -> At {
        let line = self.lines.pos(node.location().start_offset()).line;
        (self.path.to_string(), line)
    }

    fn unread(&mut self, node: &Node<'_>, why: &'static str) {
        let at = self.at(node);
        self.routes.unread.push((at, why));
    }

    /// A controller written in this scope: `users` in `namespace :admin` is
    /// `admin/users`; a leading `/` is from the top.
    fn controller(&self, written: &str) -> String {
        controller_in(&self.scope().module, written)
    }

    /// The options a call is handed, by key: `to:`, `only:`, and a hash
    /// rocket's path key (`'x' => 'c#a'`) as `=>`.
    fn options(call: &ruby_prism::CallNode<'pr>) -> HashMap<String, Node<'pr>> {
        let mut options = HashMap::new();
        let args = call
            .arguments()
            .map(|a| a.arguments().iter().collect::<Vec<_>>());
        for arg in args.unwrap_or_default() {
            let Some(hash) = arg.as_keyword_hash_node() else {
                continue;
            };
            for element in hash.elements().iter() {
                let Some(assoc) = element.as_assoc_node() else {
                    continue;
                };
                let key = assoc.key();
                // Any other key is a path: `"#{root}/x" => "c#a"`.
                let name = match key.as_symbol_node() {
                    Some(symbol) => String::from_utf8_lossy(symbol.unescaped()).into_owned(),
                    None => "=>".to_string(),
                };
                options.entry(name).or_insert(assoc.value());
            }
        }
        options
    }

    fn positional(call: &ruby_prism::CallNode<'pr>) -> Vec<Node<'pr>> {
        call.arguments()
            .map(|a| {
                a.arguments()
                    .iter()
                    .filter(|arg| arg.as_keyword_hash_node().is_none())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Visit a call's block in `scope`.
    fn within(&mut self, call: &ruby_prism::CallNode<'pr>, scope: Scope) {
        let Some(block) = call.block().and_then(|b| b.as_block_node()) else {
            return;
        };
        self.scopes.push(scope);
        if let Some(body) = block.body() {
            self.visit(&body);
        }
        self.scopes.pop();
    }

    /// `get 'path', to: 'c#a'` and its kin.
    fn verb(&mut self, call: &ruby_prism::CallNode<'pr>, node: &Node<'pr>) {
        let at = self.at(node);
        let options = Self::options(call);
        let first = Self::positional(call).into_iter().next();
        // `root 'home#index'` writes its target where a verb writes its path.
        let root = call.name().as_slice() == b"root";
        let to = options
            .get("to")
            .or(options.get("=>"))
            .or(first.as_ref().filter(|_| root));
        if let Some(to) = to {
            // A Rack app, a `redirect`, a lambda: no action.
            let Some(to) = to.as_string_node() else {
                return;
            };
            let to = String::from_utf8_lossy(to.unescaped()).into_owned();
            let Some((controller, action)) = to.split_once('#') else {
                return;
            };
            let controller = match controller {
                "" => self.scope().controller.clone(),
                written => Some(self.controller(written)),
            };
            if let Some(controller) = controller {
                let scope = self.scope().clone();
                self.routes.push(&scope, controller, action, &at);
            }
            return;
        }
        let controller = options
            .get("controller")
            .and_then(literal)
            .map(|c| self.controller(&c))
            .or_else(|| self.scope().controller.clone());
        if let Some(action) = options.get("action") {
            match (literal(action), controller) {
                (Some(action), Some(controller)) => {
                    let scope = self.scope().clone();
                    self.routes.push(&scope, controller, &action, &at);
                }
                (None, _) => self.unread(node, "an action built at runtime"),
                _ => {}
            }
            return;
        }
        let Some(first) = first else {
            return;
        };
        let Some(path) = literal(&first) else {
            self.unread(node, "a path built at runtime");
            return;
        };
        if path.contains(":controller") || path.contains(":action") {
            self.unread(node, "a route whose controller or action is a path segment");
            return;
        }
        let path = path.trim_matches('/');
        let words = |s: &str| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '/')
        };
        if !words(path) {
            return;
        }
        let scope = self.scope().clone();
        match (controller, path.rsplit_once('/')) {
            // In a controller's scope a bare path is its action.
            (Some(controller), None) => self.routes.push(&scope, controller, path, &at),
            (Some(controller), Some((_, action))) if first.as_symbol_node().is_some() => {
                self.routes.push(&scope, controller, action, &at)
            }
            // `get 'photos/search'` is `photos#search`.
            (None, Some((controller, action))) => {
                let controller = self.controller(&controller.replace('-', "_"));
                self.routes.push(&scope, controller, action, &at);
            }
            _ => {}
        }
    }

    /// `resources :widgets` and `resource :profile`.
    fn resources(&mut self, call: &ruby_prism::CallNode<'pr>, node: &Node<'pr>, plural_form: bool) {
        let at = self.at(node);
        let options = Self::options(call);
        let mut names: Vec<String> = Vec::new();
        for arg in Self::positional(call) {
            match literal(&arg) {
                Some(name) => names.push(name),
                None => {
                    self.unread(node, "a resource named at runtime");
                    return;
                }
            }
        }
        let defaults: &[&str] = if plural_form { &RESOURCES } else { &RESOURCE };
        let mut actions: Vec<&str> = defaults.to_vec();
        if let Some(only) = options.get("only") {
            let Some(only) = literals(only) else {
                self.unread(node, "`only:` built at runtime");
                return;
            };
            actions.retain(|a| only.iter().any(|o| o == a));
        }
        if let Some(except) = options.get("except") {
            let Some(except) = literals(except) else {
                self.unread(node, "`except:` built at runtime");
                return;
            };
            actions.retain(|a| !except.iter().any(|e| e == a));
        }
        let concerns = options
            .get("concerns")
            .and_then(literals)
            .unwrap_or_default();
        for name in names {
            let written = match options.get("controller").and_then(literal) {
                Some(controller) => controller,
                None if plural_form => name,
                None => plural(&name),
            };
            let mut scope = self.scope().clone();
            if let Some(module) = options.get("module").and_then(literal) {
                scope.module.push(module);
            }
            let controller = controller_in(&scope.module, &written);
            for action in &actions {
                self.routes.push(&scope, controller.clone(), action, &at);
            }
            let inner = Scope {
                controller: Some(controller),
                ..scope
            };
            for concern in &concerns {
                self.concern(concern, inner.clone());
            }
            self.within(call, inner);
        }
    }

    /// A `concern`'s block, read again in `scope`.
    fn concern(&mut self, name: &str, scope: Scope) {
        let Some(source) = self.concerns.get(name).cloned() else {
            return;
        };
        let parsed = ruby_prism::parse(&source);
        let lines = LineIndex::new(&source);
        let mut reader = Reader {
            routes: self.routes,
            root: self.root,
            path: self.path,
            source: &source,
            lines: &lines,
            scopes: vec![scope],
            depth: self.depth,
            concerns: self.concerns.clone(),
        };
        reader.visit(&parsed.node());
    }

    /// `draw :admin` reads `config/routes/admin.rb` in this scope.
    fn draw(&mut self, call: &ruby_prism::CallNode<'pr>, node: &Node<'pr>) {
        let Some(name) = Self::positional(call).first().and_then(literal) else {
            self.unread(node, "a `draw` named at runtime");
            return;
        };
        if self.depth > 8 {
            return;
        }
        // Rails' `config/routes/`, beside the app's or engine's routes.rb.
        let config = Path::new(self.path)
            .ancestors()
            .find(|dir| dir.file_name().is_some_and(|n| n == "config"))
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("config"));
        let file = config.join("routes").join(format!("{name}.rb"));
        let scope = self.scope().clone();
        self.routes
            .read_file(self.root, &file.to_string_lossy(), &scope, self.depth + 1);
    }
}

impl<'pr> Visit<'pr> for Reader<'_, '_> {
    fn visit_call_node(&mut self, call: &ruby_prism::CallNode<'pr>) {
        let name = String::from_utf8_lossy(call.name().as_slice()).into_owned();
        let node = call.as_node();
        // `Rails.application.routes.draw`, `Billing::Engine.routes.draw`.
        if call.receiver().is_some() {
            if name == "draw" {
                let mut scope = self.scope().clone();
                let receiver = call.receiver().map(|r| {
                    let at = r.location();
                    String::from_utf8_lossy(&self.source[at.start_offset()..at.end_offset()])
                        .into_owned()
                });
                if let Some(engine) = receiver
                    .as_deref()
                    .and_then(|r| r.strip_suffix("::Engine.routes"))
                {
                    scope.engine = Some(engine.trim_start_matches("::").to_string());
                }
                self.within(call, scope);
                return;
            }
            ruby_prism::visit_call_node(self, call);
            return;
        }
        match name.as_str() {
            "get" | "post" | "put" | "patch" | "delete" | "match" | "options" | "root" => {
                self.verb(call, &node);
                // `get :preview, on: :member do … end` takes no block of routes.
            }
            "resources" => self.resources(call, &node, true),
            "resource" => self.resources(call, &node, false),
            "namespace" => {
                let options = Self::options(call);
                let mut scope = self.scope().clone();
                let module = options
                    .get("module")
                    .and_then(literal)
                    .or_else(|| Self::positional(call).first().and_then(literal));
                match module {
                    Some(module) => scope.module.push(module),
                    None => self.unread(&node, "a namespace named at runtime"),
                }
                self.within(call, scope);
            }
            "scope" => {
                let options = Self::options(call);
                let mut scope = self.scope().clone();
                if let Some(module) = options.get("module").and_then(literal) {
                    scope.module.push(module);
                }
                if let Some(controller) = options.get("controller").and_then(literal) {
                    scope.controller = Some(self.controller(&controller));
                }
                self.within(call, scope);
            }
            "controller" => {
                let mut scope = self.scope().clone();
                if let Some(controller) = Self::positional(call).first().and_then(literal) {
                    scope.controller = Some(self.controller(&controller));
                }
                self.within(call, scope);
            }
            "concern" => {
                let name = Self::positional(call).first().and_then(literal);
                let body = call
                    .block()
                    .and_then(|b| b.as_block_node())
                    .and_then(|b| b.body());
                if let (Some(name), Some(body)) = (name, body) {
                    let at = body.location();
                    let source = self.source[at.start_offset()..at.end_offset()].to_vec();
                    self.concerns.insert(name, source);
                }
            }
            "concerns" => {
                let scope = self.scope().clone();
                for name in Self::positional(call).iter().filter_map(literals).flatten() {
                    self.concern(&name, scope.clone());
                }
            }
            "draw" => self.draw(call, &node),
            "mount" | "direct" | "resolve" | "redirect" => {}
            // `member`, `collection`, `constraints`, `defaults`, `devise_scope`
            // and the like: their routes are this scope's.
            _ => ruby_prism::visit_call_node(self, call),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(source: &str) -> Vec<String> {
        let mut routes = Routes::default();
        routes.read_source(
            Path::new("."),
            "config/routes.rb",
            source.as_bytes(),
            &Scope::default(),
            0,
        );
        routes
            .routes
            .iter()
            .map(|r| format!("{}#{}", r.controller, r.action))
            .collect()
    }

    #[test]
    fn a_verb_reaches_the_action_it_names() {
        let routes = read(
            "Rails.application.routes.draw do\n  post 'w/initiate', to: 'widgets#initiate'\n  \
             get '.well-known/x' => 'api/v1/oauth#discovery'\n  \
             get 'photos/search'\n  root 'home#index'\nend\n",
        );
        assert_eq!(
            routes,
            [
                "widgets#initiate",
                "api/v1/oauth#discovery",
                "photos#search",
                "home#index"
            ]
        );
    }

    #[test]
    fn a_resource_reaches_its_default_actions_and_its_blocks() {
        let routes = read(
            "Rails.application.routes.draw do\n  namespace :admin do\n    \
             resources :widgets, only: [:index, :show] do\n      member do\n        \
             post :archive\n      end\n    end\n    resource :profile, except: :destroy\n  \
             end\nend\n",
        );
        assert_eq!(
            routes,
            [
                "admin/widgets#index",
                "admin/widgets#show",
                "admin/widgets#archive",
                "admin/profiles#create",
                "admin/profiles#new",
                "admin/profiles#edit",
                "admin/profiles#show",
                "admin/profiles#update",
            ]
        );
    }

    #[test]
    fn a_scope_module_and_a_concern_nest_their_routes() {
        let routes = read(
            "Rails.application.routes.draw do\n  concern :pinnable do\n    post :pin\n  end\n  \
             scope module: :api do\n    resources :posts, only: [], concerns: :pinnable\n    \
             controller :health do\n      get :ping\n    end\n  end\nend\n",
        );
        assert_eq!(routes, ["api/posts#pin", "api/health#ping"]);
    }

    #[test]
    fn a_route_it_cannot_read_is_listed() {
        let mut routes = Routes::default();
        routes.read_source(
            Path::new("."),
            "config/routes.rb",
            b"Rails.application.routes.draw do\n  %w[a b].each { |n| resources n }\n  \
              match ':controller(/:action)', via: :get\nend\n",
            &Scope::default(),
            0,
        );
        assert!(routes.routes.is_empty());
        assert_eq!(routes.unread.len(), 2);
    }
}
