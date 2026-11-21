//! Which gem owns what the checkout names: a site's layer (the app, a
//! gem, neither), a constant's and a method's owning gems, a required
//! feature's.

use super::*;

/// Which checkout a site's path is in: the app, a gem by name, or neither
/// (Ruby core, the stdlib).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Layer {
    App,
    Gem(String),
    /// A stub declaring what a library compiles, or what RSpec wires: it
    /// neither owns a constant nor keeps a gem from owning one.
    Stub,
    Other,
}

pub(super) struct Layers {
    pub(super) app: String,
    /// Gem roots, longest first, so a gem nested in another's clone wins.
    pub(super) gems: Vec<(String, String)>,
    /// The stdlib's files a default gem owns, when the manifest declares
    /// that gem: Ruby's copy of json is json's, whichever copy answers.
    pub(super) default: HashMap<String, String>,
    /// The directories of path gems inside the checkout, indexed with it.
    pub(super) in_checkout: HashSet<String>,
}

impl Layers {
    pub(super) fn new(
        app: &str,
        installed: &HashMap<String, String>,
        default: HashMap<String, String>,
        in_checkout: HashSet<String>,
    ) -> Layers {
        let mut gems: Vec<(String, String)> = installed
            .iter()
            .map(|(name, dir)| (format!("{dir}/"), name.clone()))
            .collect();
        gems.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.cmp(b)));
        Layers {
            app: format!("{app}/"),
            gems,
            default,
            in_checkout,
        }
    }

    /// Whether `path` is the checkout's own code: the app's, or a path gem's
    /// inside it — not a gem bundled into `vendor/bundle`.
    pub(super) fn is_checkouts(&self, path: &str) -> bool {
        match self.of(path) {
            Layer::App => true,
            Layer::Gem(_) => self.in_checkout.iter().any(|dir| {
                path.strip_prefix(dir.as_str())
                    .is_some_and(|r| r.starts_with('/'))
            }),
            _ => false,
        }
    }

    /// The store root holding a gem's files, and the prefix of its paths
    /// there: its own, or the checkout's for a path gem inside it. A gem
    /// bundled into `vendor/bundle` is under the checkout too, but is its
    /// own root.
    pub(super) fn store_root<'a>(&'a self, dir: &'a str) -> (&'a str, String) {
        let app = self.app.trim_end_matches('/');
        match dir.strip_prefix(&self.app) {
            Some(relative) if self.in_checkout.contains(dir) => (app, format!("{relative}/")),
            _ => (dir, String::new()),
        }
    }

    pub(super) fn of(&self, path: &str) -> Layer {
        if let Some((_, name)) = self
            .gems
            .iter()
            .find(|(dir, _)| path.starts_with(dir.as_str()))
        {
            return Layer::Gem(name.clone());
        }
        if let Some(name) = self.default.get(path) {
            return Layer::Gem(name.clone());
        }
        if path.starts_with(&self.app) {
            return Layer::App;
        }
        if crate::tree::is_stdlib_stub(path) || crate::tree::is_rspec_stub(path) {
            return Layer::Stub;
        }
        Layer::Other
    }
}

/// Which gem each constant belongs to. One only gems declare belongs to the
/// gem every other declaring gem depends on (the one they reopen); when no
/// one gem is that, to each of them. Ruby core's and the stdlib's belong to
/// none.
pub(super) struct Owned {
    pub(super) by_fqn: BTreeMap<String, Vec<String>>,
    /// Each owned constant's last segment: a reference naming none of them
    /// cannot reach one, and is never resolved.
    pub(super) leaves: HashSet<String>,
    /// Each gem's Railtie or Engine, with its file and line.
    pub(super) railties: HashMap<String, (String, String, u32)>,
}

impl Owned {
    pub(super) fn new(tree: &Tree, layers: &Layers, lock: &Lock) -> Owned {
        let mut by_fqn = BTreeMap::new();
        let mut railties = HashMap::new();
        let mut declared = tree.declared();
        declared.sort();
        for (fqn, kind) in declared {
            let sites = tree.sites(&fqn);
            let mut gems: BTreeSet<String> = BTreeSet::new();
            let mut other = false;
            for site in &sites {
                match layers.of(&site.path) {
                    Layer::Gem(name) => {
                        gems.insert(name);
                    }
                    Layer::Other => other = true,
                    Layer::App | Layer::Stub => {}
                }
            }
            if other || gems.is_empty() {
                continue;
            }
            let owners = owners_of(&fqn, &kind, &sites, gems, layers, lock);
            if owners.is_empty() {
                continue;
            }
            if kind == "class" && owners.len() == 1 && tree.inherits(&fqn, "Rails::Railtie") {
                let site = sites
                    .iter()
                    .find(|s| layers.of(&s.path) == Layer::Gem(owners[0].clone()));
                if let Some(site) = site {
                    railties.entry(owners[0].clone()).or_insert((
                        fqn.clone(),
                        site.path.clone(),
                        site.line,
                    ));
                }
            }
            by_fqn.insert(fqn, owners);
        }
        let leaves = by_fqn
            .keys()
            .map(|fqn| fqn.rsplit("::").next().unwrap_or(fqn).to_string())
            .collect();
        Owned {
            by_fqn,
            leaves,
            railties,
        }
    }

    /// The gems owning `fqn` or the nearest namespace holding it that one owns.
    pub(super) fn of_name(&self, fqn: &str) -> Vec<String> {
        let mut name = fqn;
        loop {
            if let Some(gems) = self.by_fqn.get(name) {
                return gems.clone();
            }
            match name.rsplit_once("::") {
                Some((outer, _)) => name = outer,
                None => return Vec::new(),
            }
        }
    }

    pub(super) fn owns(&self, gem: &str, fqn: &str) -> bool {
        self.by_fqn
            .get(fqn)
            .is_some_and(|gems| gems.iter().any(|g| g == gem))
    }
}

/// Of the gems declaring `fqn`, the one it belongs to: the one whose
/// `lib/` has the file named for it (`lib/rack.rb` for `Rack`), else the
/// one named for it, else the one every other depends on. When none picks
/// one, a class is each of theirs and a module none of theirs.
pub(super) fn owners_of(
    fqn: &str,
    kind: &str,
    sites: &[crate::tree::Site],
    gems: BTreeSet<String>,
    layers: &Layers,
    lock: &Lock,
) -> Vec<String> {
    if gems.len() == 1 {
        return gems.into_iter().collect();
    }
    let flat = |s: &str| s.to_ascii_lowercase().replace('_', "");
    let wanted: Vec<String> = fqn.split("::").map(flat).collect();
    let named: BTreeSet<String> = sites
        .iter()
        .filter(|site| {
            site.path
                .rsplit_once("/lib/")
                .and_then(|(_, rest)| rest.strip_suffix(".rb"))
                .is_some_and(|rest| rest.split('/').map(flat).collect::<Vec<_>>() == wanted)
        })
        .filter_map(|site| match layers.of(&site.path) {
            Layer::Gem(name) => Some(name),
            _ => None,
        })
        .collect();
    if named.len() == 1 {
        return named.into_iter().collect();
    }
    // The gem named for it: rubocop's `RuboCop`, whose `lib/rubocop.rb`
    // only requires the files that declare it.
    let whole = fqn.to_ascii_lowercase().replace(['_', ':'], "");
    let by_name: Vec<&String> = gems
        .iter()
        .filter(|g| g.to_ascii_lowercase().replace(['_', '-'], "") == whole)
        .collect();
    if let [only] = by_name.as_slice() {
        return vec![(*only).clone()];
    }
    let base: Vec<String> = gems
        .iter()
        .filter(|g| {
            gems.iter()
                .all(|o| o == *g || lock.closure(o).contains(g.as_str()))
        })
        .cloned()
        .collect();
    match base.len() {
        1 => base,
        // A namespace several gems open is none of theirs: what each puts
        // in it is.
        _ if kind == "module" => Vec::new(),
        _ => gems.into_iter().collect(),
    }
}

/// How many gems may define a name for a call of it to count for each:
/// `push_bulk` is sidekiq-bulk's mixin and Sidekiq::Client's own method.
pub(super) const FEW: usize = 3;

/// The gems defining a method name, each with one row, when no more than
/// [`FEW`] do and neither the checkout nor its Ruby's stdlib does.
pub(super) struct Defines {
    pub(super) rows: Vec<(String, crate::store::MethodRow)>,
}

pub(super) fn method_names(
    store: &Store,
    root: &str,
    layers: &Layers,
) -> anyhow::Result<HashMap<String, Defines>> {
    let roots = store.tree_roots(root)?;
    let mut by_name: HashMap<String, Option<Defines>> = HashMap::new();
    store.each_method(&roots, |row| {
        if matches!(
            row.via.as_deref(),
            Some("private" | "protected" | "public" | "module_function")
        ) {
            return;
        }
        let entry = by_name
            .entry(row.name.clone())
            .or_insert_with(|| Some(Defines { rows: Vec::new() }));
        let Some(defines) = entry else { return };
        let Layer::Gem(gem) = layers.of(&row.path) else {
            *entry = None;
            return;
        };
        if defines.rows.iter().any(|(g, _)| *g == gem) {
            return;
        }
        if defines.rows.len() == FEW {
            *entry = None;
            return;
        }
        defines.rows.push((gem, row));
    })?;
    Ok(by_name
        .into_iter()
        .filter_map(|(name, defines)| Some((name, defines?)))
        .collect())
}

/// The method names the bundle's rbs declares for Ruby core, under its
/// `core/`. Core is read from the Ruby's own rbs, which may be older: rbs
/// 4's `Module#ruby2_keywords` is core's, not a polyfill rbs is used for.
pub(super) fn rbs_core_names(dir: &Path) -> HashSet<String> {
    let mut names = HashSet::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(dir) = todo.pop() {
        for path in std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
        {
            if path.is_dir() {
                todo.push(path);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rbs") {
                continue;
            }
            let Ok(text) = crate::scan::read_text(&path) else {
                continue;
            };
            for line in text.lines() {
                let line = line.trim_start();
                let line = line
                    .strip_prefix("private ")
                    .or_else(|| line.strip_prefix("public "))
                    .unwrap_or(line);
                let Some(rest) = line.strip_prefix("def ") else {
                    continue;
                };
                let rest = rest
                    .strip_prefix("self?.")
                    .or_else(|| rest.strip_prefix("self."))
                    .unwrap_or(rest);
                if let Some((name, _)) = rest.split_once(':') {
                    names.insert(name.trim().to_string());
                }
            }
        }
    }
    names
}

/// Each `require` path a gem's files answer to, from what the index holds
/// of its `lib/`: `lib/kaminari/config.rb` is `kaminari/config`.
pub(super) struct Features {
    pub(super) by_path: HashMap<String, Vec<String>>,
    pub(super) names: HashSet<String>,
}

impl Features {
    pub(super) fn new(
        store: &Store,
        layers: &Layers,
        installed: &HashMap<String, String>,
    ) -> anyhow::Result<Features> {
        let mut by_path: HashMap<String, Vec<String>> = HashMap::new();
        for (name, dir) in installed {
            let (root, prefix) = layers.store_root(dir);
            for path in store.file_map(root)?.into_keys() {
                let Some(feature) = path
                    .strip_prefix(prefix.as_str())
                    .and_then(|p| p.strip_prefix("lib/"))
                    .and_then(|p| p.strip_suffix(".rb"))
                else {
                    continue;
                };
                by_path
                    .entry(feature.to_string())
                    .or_default()
                    .push(name.clone());
            }
        }
        for gems in by_path.values_mut() {
            gems.sort();
        }
        Ok(Features {
            by_path,
            names: installed.keys().cloned().collect(),
        })
    }

    /// The gems a required path names: the one whose `lib/` holds it, else
    /// a gem named as its first segment is (a compiled `pg_ext`, a file
    /// the index leaves out).
    pub(super) fn of(&self, required: &str) -> Vec<String> {
        let path = required.strip_suffix(".rb").unwrap_or(required);
        if let Some(gems) = self.by_path.get(path) {
            return gems.clone();
        }
        let head = path.split('/').next().unwrap_or(path);
        [head.to_string(), path.replace('/', "-")]
            .into_iter()
            .filter(|name| self.names.contains(name))
            .take(1)
            .collect()
    }
}
