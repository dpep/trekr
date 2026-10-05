//! What a view template runs on: Rails' view context, assembled from the
//! checkout (DEC-521).
//!
//! A template is compiled into a method of a class Rails makes per
//! controller: a subclass of `ActionView::Base` that includes the
//! controller's helper module — every module under `app/helpers/` by
//! default (`helper :all`), and a method for each name `helper_method`
//! exposes, which sends it to the controller. Path conventions, read here
//! because the tree is the layer that knows where a declaration is written.

#![expect(
    clippy::disallowed_methods,
    reason = "reads here predate scan::read_source; converting the last one fails this expect"
)]

use super::Tree;
use crate::core::Facts;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// The class a template's `self` is an instance of, or the nearest the index
/// can name: Rails' own subclass of it is anonymous.
pub(crate) const ACTION_VIEW: &str = "ActionView::Base";

/// A file's modification time and length, which a read is kept while.
type Stamp = (std::time::SystemTime, u64);

/// A file's facts, the stamp they were read at, and when last asked for.
type Read = (Stamp, Arc<Facts>, u64);

/// The templates a file names, and the stamp they were read at.
type NamedRead = (Stamp, Arc<[crate::core::TemplateRef]>);

/// How many files' whole facts are kept: what a template's answer reads in
/// depth — a controller's chain, the renders that reach a partial — is a
/// few files, and past this the least recently asked for is dropped and
/// read again when asked. The scan for renders keeps only templates.
const FACTS_KEPT: usize = 64;

/// An editor's unsaved copies of checkout files, by absolute path, and the
/// session's count of edits they were copied at. Only the LSP sets them.
#[derive(Default)]
pub(crate) struct Open(Mutex<Copies>);

#[derive(Default)]
struct Copies {
    generation: Option<u64>,
    texts: HashMap<String, Arc<[u8]>>,
}

/// The checkout's view conventions, read once per tree.
#[derive(Default)]
pub(crate) struct Views {
    /// The checkout's helper modules — each module named for its file under
    /// an `app/helpers/` directory — in the order `helper :all` includes
    /// them: by path. The last included is the first found. A module not in
    /// a `*_helper.rb` is one `helper :all` leaves to a controller's `helper
    /// X`, which is taken as the app's too.
    helpers: Vec<String>,
    /// A name `helper_method` exposes to views → the classes whose body
    /// exposes it, and where.
    exposed: HashMap<String, Vec<(String, super::Site)>>,
    /// A class's name, folded (no `::`, no `_`, lowercase) → the name: a
    /// path names a controller as Rails' inflector spells it, and an
    /// acronym inflection (`OAuth`) spells it otherwise (DEC-344).
    classes: HashMap<String, String>,
    /// A controller's file read for what its actions assign, by path, with
    /// the stamp it was read at; at most `FACTS_KEPT`, and a counter of asks.
    files: Mutex<(HashMap<String, Read>, u64)>,
    /// The templates each file names, for the scans over every file that
    /// calls `render`: kept for every file, being small.
    named: Mutex<HashMap<String, NamedRead>>,
    /// The checkout's files that call a name (`render`, `extends`),
    /// relative to it.
    calling: Mutex<HashMap<String, Arc<[String]>>>,
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

/// A view template a controller renders, by what its `self` is: one under
/// `app/views/`. A generator's `.erb` template, or a config file's, runs on
/// something else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ViewTemplate {
    /// ERB, run on a view context.
    Erb,
    /// RABL, run on its engine.
    Rabl,
}

impl ViewTemplate {
    pub(crate) fn of(path: &str) -> Option<ViewTemplate> {
        use crate::scan::Reader;
        under(path, "views")?;
        match Reader::of(path) {
            Reader::Erb => Some(ViewTemplate::Erb),
            Reader::Rabl => Some(ViewTemplate::Rabl),
            Reader::Ruby | Reader::StructureSql => None,
        }
    }
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
                    "module" => {
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
            let mut exposed: HashMap<String, Vec<(String, super::Site)>> = HashMap::new();
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
                let site = super::Site {
                    path: row.path.clone(),
                    line: row.line,
                    col: 1,
                    kind: "method".to_string(),
                };
                for name in row.args.into_iter().flatten() {
                    let owners = exposed.entry(name).or_default();
                    if !owners.iter().any(|(o, _)| *o == owner) {
                        owners.push((owner.clone(), site.clone()));
                    }
                }
            }
            Views {
                helpers: helpers.into_iter().map(|(_, fqn)| fqn).collect(),
                exposed,
                classes,
                files: Mutex::default(),
                named: Mutex::default(),
                calling: Mutex::default(),
            }
        })
    }

    /// The method a call on a view's `self` runs when no controller's
    /// `helper_method` exposes the name: the checkout's helpers, last
    /// included first, then `ActionView::Base`'s own chain. An exposed name
    /// is sent to the controller that renders the template
    /// (`resolve::views::exposed_receiver`).
    pub(crate) fn lookup_in_view(&self, name: &str) -> Option<super::MethodDef> {
        self.views()
            .helpers
            .iter()
            .rev()
            .find_map(|helper| self.lookup(helper, false, name))
            .or_else(|| self.lookup(ACTION_VIEW, false, name))
    }

    /// What a view's `self` offers, in the order a call finds it: every name
    /// a `helper_method` exposes, with its controller, then the helper
    /// modules, last included first.
    pub(crate) fn view_context(&self) -> (Vec<(String, String)>, Vec<String>) {
        let views = self.views();
        let mut exposed: Vec<(String, String)> = views
            .exposed
            .iter()
            .flat_map(|(name, owners)| {
                owners
                    .iter()
                    .map(move |(owner, _)| (name.clone(), owner.clone()))
            })
            .collect();
        exposed.sort();
        (exposed, views.helpers.iter().rev().cloned().collect())
    }

    /// The classes whose body exposes `name` to views with `helper_method`.
    pub(crate) fn exposers(&self, name: &str) -> Vec<String> {
        self.views()
            .exposed
            .get(name)
            .map(|owners| owners.iter().map(|(owner, _)| owner.clone()).collect())
            .unwrap_or_default()
    }

    /// The checkout the tree was built for, absolute.
    pub(crate) fn checkout_root(&self) -> &str {
        &self.root
    }

    /// The checkout's files that call `name` — `render`, every place a
    /// partial may be rendered from; RABL's `extends` — relative to it.
    pub(crate) fn files_calling(&self, name: &str) -> Arc<[String]> {
        let views = self.views();
        if let Some(files) = views.calling.lock().ok().and_then(|c| c.get(name).cloned()) {
            return files;
        }
        let files: Arc<[String]> = self
            .loader
            .as_ref()
            .and_then(|loader| {
                loader
                    .with(|store, _| store.files_calling(&self.root, name))
                    .ok()
            })
            .unwrap_or_default()
            .into();
        if let Ok(mut calling) = views.calling.lock() {
            calling.insert(name.to_string(), files.clone());
        }
        files
    }

    /// Where `owner`'s body exposes `name` to views with `helper_method`: the
    /// method Rails generates there is what a template's call enters first.
    pub(crate) fn exposed_at(&self, owner: &str, name: &str) -> Option<super::Site> {
        let owners = self.views().exposed.get(name)?;
        owners
            .iter()
            .find(|(exposer, _)| exposer == owner || self.inherits(owner, exposer))
            .map(|(_, site)| site.clone())
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

    /// Whether the editor's copies were last set at edit `generation`.
    pub(crate) fn open_at(&self, generation: u64) -> bool {
        self.open
            .0
            .lock()
            .is_ok_and(|open| open.generation == Some(generation))
    }

    /// The editor's unsaved copies, by absolute path as the tree spells it,
    /// as of edit `generation`: what `file_facts` and `file_templates` read
    /// in place of the disk.
    pub(crate) fn set_open(&self, generation: u64, texts: HashMap<String, Arc<[u8]>>) {
        if let Ok(mut open) = self.open.0.lock() {
            *open = Copies {
                generation: Some(generation),
                texts,
            };
        }
    }

    fn open_text(&self, path: &str) -> Option<Arc<[u8]>> {
        self.open.0.lock().ok()?.texts.get(path).cloned()
    }

    /// A checkout file's facts, read from the editor's copy or the disk: what
    /// a controller's actions assign is read where a template reads it. A
    /// disk read is kept while the file is unchanged, for the `FACTS_KEPT`
    /// files most recently asked for.
    pub(crate) fn file_facts(&self, path: &str) -> Option<Arc<Facts>> {
        if let Some(text) = self.open_text(path) {
            return Some(Arc::new(crate::extract::extract_file(path, &text)));
        }
        let stamp = stamp_of(path)?;
        let mut guard = self.views().files.lock().ok()?;
        let (files, asks) = &mut *guard;
        *asks += 1;
        let now = *asks;
        if let Some((read, facts, used)) = files.get_mut(path)
            && *read == stamp
        {
            *used = now;
            return Some(facts.clone());
        }
        let bytes = std::fs::read(path).ok()?;
        let facts = Arc::new(crate::extract::extract_file(path, &bytes));
        files.insert(path.to_string(), (stamp, facts.clone(), now));
        if files.len() > FACTS_KEPT
            && let Some(oldest) = files
                .iter()
                .min_by_key(|(_, (_, _, used))| *used)
                .map(|(path, _)| path.clone())
        {
            files.remove(&oldest);
        }
        Some(facts)
    }

    /// The templates a checkout file names (`render "row"`, `extends "x"`),
    /// read from the editor's copy, or from disk and kept while it is
    /// unchanged — what a scan over every file that calls `render` needs of
    /// each. With `naming`, a file not yet read whose text lacks that word is
    /// not parsed: it names no template by it.
    pub(crate) fn file_templates(
        &self,
        path: &str,
        naming: Option<&str>,
    ) -> Option<Arc<[crate::core::TemplateRef]>> {
        if let Some(text) = self.open_text(path) {
            return Some(crate::extract::extract_file(path, &text).templates.into());
        }
        let stamp = stamp_of(path)?;
        if let Some((read, named)) = self.views().named.lock().ok()?.get(path)
            && *read == stamp
        {
            return Some(named.clone());
        }
        let bytes = std::fs::read(path).ok()?;
        if let Some(word) = naming
            && !bytes.windows(word.len()).any(|w| w == word.as_bytes())
        {
            return None;
        }
        let named: Arc<[crate::core::TemplateRef]> =
            crate::extract::extract_file(path, &bytes).templates.into();
        self.views()
            .named
            .lock()
            .ok()?
            .insert(path.to_string(), (stamp, named.clone()));
        Some(named)
    }
}

fn stamp_of(path: &str) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// Where a template a file names is looked for: the directory under the
/// views root, and the start of the file's name (`_form.`, `show.`), each
/// read by Rails' rules (DEC-524). `from` is the naming file, relative to
/// the checkout; `class` names the partial of a `render @post`.
pub(crate) fn template_dirs(
    from: &str,
    named: &crate::core::Named,
    class: Option<&str>,
) -> Option<(String, String, String)> {
    use crate::core::Named;
    // The views root the naming file belongs to, and its own directory under
    // it: a template's, or a controller's (`admin/posts_controller.rb` is
    // `admin/posts`).
    let (root, here) = match under(from, "views") {
        Some(rest) => (
            from[..from.len() - rest.len()].to_string(),
            rest.rsplit_once('/').map_or("", |(dir, _)| dir).to_string(),
        ),
        None => {
            let rest = under(from, "controllers")?;
            let root = format!(
                "{}views/",
                &from[..from.len() - rest.len() - "controllers/".len()]
            );
            (
                root,
                rest.trim_end_matches(".rb")
                    .trim_end_matches("_controller")
                    .to_string(),
            )
        }
    };
    let in_view = under(from, "views").is_some();
    let split = |name: &str, partial: bool| {
        let (dir, base) = match name.rsplit_once('/') {
            Some((dir, base)) => (dir.to_string(), base),
            None => (here.clone(), name),
        };
        let base = match partial {
            true => format!("_{base}."),
            false => format!("{base}."),
        };
        (root.clone(), dir, base)
    };
    Some(match named {
        Named::Render(name) => split(name, in_view),
        Named::Partial(name) => split(name, true),
        Named::Template(name) => split(name, false),
        Named::Object { .. } => {
            // `Admin::Post#to_partial_path` is `admin/posts/_post`.
            let class = class?;
            let segments: Vec<String> = class
                .split("::")
                .map(crate::scan::near::underscore)
                .collect();
            let (last, scope) = segments.split_last()?;
            let mut dir: Vec<String> = scope.to_vec();
            dir.push(crate::inflect::plural(last));
            (root, dir.join("/"), format!("_{last}."))
        }
    })
}

/// The templates under `checkout` a name reaches: every format and handler of
/// it (`_form.html.erb`, `_form.text.erb`), in path order.
pub(crate) fn template_files(
    checkout: &std::path::Path,
    from: &str,
    named: &crate::core::Named,
    class: Option<&str>,
) -> Vec<String> {
    let Some((root, dir, base)) = template_dirs(from, named, class) else {
        return Vec::new();
    };
    let dir = match dir.is_empty() {
        true => root,
        false => format!("{root}{dir}/"),
    };
    let Ok(entries) = std::fs::read_dir(checkout.join(&dir)) else {
        return Vec::new();
    };
    let mut found: Vec<String> = entries
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with(&base))
        .map(|name| format!("{dir}{name}"))
        .collect();
    found.sort();
    found
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
        for (path, kind) in [
            ("app/views/a/b.html.erb", Some(ViewTemplate::Erb)),
            (
                "engines/e/app/views/a/b.json.rabl",
                Some(ViewTemplate::Rabl),
            ),
            ("lib/generators/x/templates/migration.erb", None),
            ("app/views/a/b.html.haml", None),
            ("app/models/a.rb", None),
        ] {
            assert_eq!(ViewTemplate::of(path), kind, "{path}");
        }
    }

    #[test]
    fn a_template_is_named_by_rails_rules() {
        use crate::core::Named;
        let at = |from: &str, named: Named, class: Option<&str>| {
            let (root, dir, base) = template_dirs(from, &named, class).unwrap();
            format!("{root}{dir}/{base}")
        };
        let view = "app/views/posts/show.html.erb";
        assert_eq!(
            at(view, Named::Render("form".into()), None),
            "app/views/posts/_form."
        );
        assert_eq!(
            at(view, Named::Render("shared/nav".into()), None),
            "app/views/shared/_nav."
        );
        assert_eq!(
            at(view, Named::Template("posts/base".into()), None),
            "app/views/posts/base."
        );
        let object = Named::Object {
            value: "@post".into(),
            collection: false,
        };
        assert_eq!(
            at(view, object, Some("Admin::BlogPost")),
            "app/views/admin/blog_posts/_blog_post."
        );
        assert_eq!(
            at(
                "engines/shop/app/controllers/carts_controller.rb",
                Named::Render("edit".into()),
                None
            ),
            "engines/shop/app/views/carts/edit."
        );
    }

    #[test]
    fn whole_facts_are_kept_for_the_files_last_asked_for_and_read_again_after() {
        let dir = std::env::temp_dir().join(format!("trekr-views-kept-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = |i: usize| dir.join(format!("w{i}.rb")).to_string_lossy().into_owned();
        for i in 0..=FACTS_KEPT {
            std::fs::write(path(i), format!("class W{i}\nend\n")).unwrap();
        }
        let tree = crate::tree::for_test(&[]);
        for i in 0..=FACTS_KEPT {
            assert!(tree.file_facts(&path(i)).is_some());
        }
        let kept = |p: &str| tree.views().files.lock().unwrap().0.contains_key(p);
        assert_eq!(tree.views().files.lock().unwrap().0.len(), FACTS_KEPT);
        assert!(!kept(&path(0)), "the least recently asked for is dropped");
        let again = tree.file_facts(&path(0)).unwrap();
        assert_eq!(again.defs[0].name, "W0", "and read again when asked");
        // The scan's templates are kept for every file.
        for i in 0..=FACTS_KEPT {
            tree.file_templates(&path(i), None).unwrap();
        }
        assert_eq!(tree.views().named.lock().unwrap().len(), FACTS_KEPT + 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_path_and_a_class_name_fold_alike() {
        assert_eq!(
            fold("admin/o_auth_controller"),
            fold("Admin::OAuthController")
        );
    }
}
