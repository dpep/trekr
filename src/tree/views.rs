//! What a view template runs on: Rails' view context, assembled from the
//! checkout (DEC-521).
//!
//! A template is compiled into a method of a class Rails makes per
//! controller: a subclass of `ActionView::Base` that includes the
//! controller's helper module — every module under `app/helpers/` by
//! default (`helper :all`), and a method for each name `helper_method`
//! exposes, which sends it to the controller. Path conventions, read here
//! because the tree is the layer that knows where a declaration is written.

use super::Tree;
use crate::core::Facts;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// The class a template's `self` is an instance of, or the nearest the index
/// can name: Rails' own subclass of it is anonymous.
pub(crate) const ACTION_VIEW: &str = "ActionView::Base";

/// A file's facts, with the modification time and length they were read at.
type Read = (std::time::SystemTime, u64, Arc<Facts>);

/// The checkout's view conventions, read once per tree.
#[derive(Default)]
pub(crate) struct Views {
    /// The checkout's helper modules, in the order `helper :all` includes
    /// them: by path. The last included is the first found.
    helpers: Vec<String>,
    /// A name `helper_method` exposes to views → the classes whose body
    /// exposes it.
    exposed: HashMap<String, Vec<String>>,
    /// A class's name, folded (no `::`, no `_`, lowercase) → the name: a
    /// path names a controller as Rails' inflector spells it, and an
    /// acronym inflection (`OAuth`) spells it otherwise (DEC-344).
    classes: HashMap<String, String>,
    /// A controller's file read for what its actions assign, by path, with
    /// the modification time and length it was read at.
    files: Mutex<HashMap<String, Read>>,
}

/// `Admin::OAuthController` and `admin/o_auth` alike.
fn fold(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// The part of a path after `app/views/` (or `app/helpers/`), at any depth: an
/// engine's views are its own app's.
pub(crate) fn under<'a>(path: &'a str, dir: &str) -> Option<&'a str> {
    let marker = format!("app/{dir}/");
    if let Some(rest) = path.strip_prefix(&marker) {
        return Some(rest);
    }
    path.find(&format!("/{marker}"))
        .map(|at| &path[at + marker.len() + 1..])
}

/// Is this file a view template whose `self` is a view context: an ERB
/// template under `app/views/`. A generator's `.erb` template, or a config
/// file's, runs on something else.
pub(crate) fn is_view(path: &str) -> bool {
    crate::scan::is_erb(path) && under(path, "views").is_some()
}

impl Tree {
    fn views(&self) -> &Views {
        self.views.get_or_init(|| {
            let mut helpers: Vec<(String, String)> = Vec::new();
            let mut classes = HashMap::new();
            for (fqn, kind) in self.declared() {
                match kind.as_str() {
                    "class" => {
                        classes.entry(fold(&fqn)).or_insert(fqn);
                    }
                    "module" if fqn.ends_with("Helper") => {
                        let site = self.sites(&fqn).into_iter().find(|site| {
                            self.in_checkout(&site.path)
                                && under(&site.path, "helpers").is_some_and(|rest| {
                                    fold(rest.trim_end_matches(".rb")) == fold(&fqn)
                                })
                        });
                        if let Some(site) = site {
                            helpers.push((site.path, fqn));
                        }
                    }
                    _ => {}
                }
            }
            helpers.sort();
            let mut exposed: HashMap<String, Vec<String>> = HashMap::new();
            let rows = self
                .loader
                .as_ref()
                .and_then(|loader| {
                    loader
                        .with(|store, roots| store.body_calls(roots, "helper_method"))
                        .ok()
                })
                .unwrap_or_default();
            for row in rows {
                let Some(owner) = self.scope_fqn(&row.nesting) else {
                    continue;
                };
                for name in row.args.into_iter().flatten() {
                    let owners = exposed.entry(name).or_default();
                    if !owners.contains(&owner) {
                        owners.push(owner.clone());
                    }
                }
            }
            Views {
                helpers: helpers.into_iter().map(|(_, fqn)| fqn).collect(),
                exposed,
                classes,
                files: Mutex::default(),
            }
        })
    }

    /// The method a call on a view's `self` runs: one `helper_method`
    /// exposes from `controller` (or from any controller, when the template
    /// names none the index has), then the checkout's helpers, last included
    /// first, then `ActionView::Base`'s own chain.
    pub(crate) fn lookup_in_view(
        &self,
        controller: Option<&str>,
        name: &str,
    ) -> Option<super::MethodDef> {
        let views = self.views();
        if let Some(owners) = views.exposed.get(name) {
            let reaching =
                |owner: &String| controller.is_none_or(|c| c == owner || self.inherits(c, owner));
            if let Some(owner) = owners.iter().find(|owner| reaching(owner)) {
                let on = controller.unwrap_or(owner);
                if let Some(found) = self.lookup(on, false, name) {
                    return Some(found);
                }
            }
        }
        views
            .helpers
            .iter()
            .rev()
            .find_map(|helper| self.lookup(helper, false, name))
            .or_else(|| self.lookup(ACTION_VIEW, false, name))
    }

    /// The class whose action renders a template, and the action, by Rails'
    /// path convention: `app/views/admin/posts/show.html.erb` is
    /// `Admin::PostsController#show`, `app/views/user_mailer/welcome.text.erb`
    /// `UserMailer#welcome`. A partial (`_form`) is no action's own, and a
    /// layout (`layouts/posts`) is every action's of the class it is named
    /// for — `application` is `ApplicationController`. `None` when the index
    /// holds no such class.
    pub(crate) fn renderer_of(&self, template: &str) -> Option<(String, Option<String>)> {
        let rest = under(template, "views")?;
        let (dir, file) = rest.rsplit_once('/')?;
        let base = file.split('.').next().unwrap_or(file);
        let (dir, action) = match dir.rsplit_once('/').map_or(dir, |(_, last)| last) {
            "layouts" => (base, None),
            _ if base.starts_with('_') => (dir, None),
            _ => (dir, Some(base.to_string())),
        };
        let classes = &self.views().classes;
        let class = [format!("{dir}_controller"), dir.to_string()]
            .iter()
            .filter_map(|name| classes.get(&fold(name)))
            .find(|fqn| self.kind_of(fqn) == Some("class"))
            .cloned()?;
        Some((class, action))
    }

    /// A checkout file's facts, read from disk: what a controller's actions
    /// assign is read where a template reads it. Kept while the file is
    /// unchanged.
    pub(crate) fn file_facts(&self, path: &str) -> Option<Arc<Facts>> {
        let meta = std::fs::metadata(path).ok()?;
        let stamp = (meta.modified().ok()?, meta.len());
        let mut files = self.views().files.lock().ok()?;
        if let Some((modified, len, facts)) = files.get(path)
            && (*modified, *len) == stamp
        {
            return Some(facts.clone());
        }
        let bytes = std::fs::read(path).ok()?;
        let facts = Arc::new(crate::extract::extract_file(path, &bytes));
        files.insert(path.to_string(), (stamp.0, stamp.1, facts.clone()));
        Some(facts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_views_directory_is_found_at_any_depth() {
        assert_eq!(
            under("app/views/posts/show.html.erb", "views"),
            Some("posts/show.html.erb")
        );
        assert_eq!(
            under("engines/shop/app/views/carts/show.html.erb", "views"),
            Some("carts/show.html.erb")
        );
        assert_eq!(under("lib/templates/x.erb", "views"), None);
        assert!(is_view("app/views/a/b.html.erb"));
        assert!(!is_view("lib/generators/x/templates/migration.erb"));
    }

    #[test]
    fn a_path_and_a_class_name_fold_alike() {
        assert_eq!(
            fold("admin/o_auth_controller"),
            fold("Admin::OAuthController")
        );
    }
}
