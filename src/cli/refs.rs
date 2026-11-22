//! The refs sweep: every call site of a name, tiered against the query, with
//! the files read once across the questions that share them.

use super::*;

/// Every call site named `name`, tiered against the query.
///
/// Files are reparsed rather than read from the stored call rows: the ladder
/// needs the file's assignments, which are deliberately not stored (DEC-012),
/// and reparsing means an edit since the last index is still tiered correctly.
///
/// `parsed`, when given, holds each file's facts across calls: `--dead` asks
/// about every method in scope, and the files calling them are mostly the
/// same files. A single query passes `None` and holds one file at a time.
#[allow(clippy::too_many_arguments)]
pub(super) fn gather_refs(
    tree: &Tree,
    store: &Store,
    root: &Path,
    root_str: &str,
    query: &crate::resolve::refs::Query,
    target: Option<&str>,
    // Keep the excluded sites too, so `--include-excluded` can show them.
    keep_all: bool,
    parsed: Option<&Parsed>,
    // Every call of the name as a row of its own, for the bare-name listing:
    // the index keeps which files call a name, not where (DEC-193).
    mut sites: Option<&mut Vec<crate::store::Ref>>,
) -> anyhow::Result<(
    Vec<crate::resolve::refs::Reference>,
    crate::resolve::refs::Counts,
)> {
    use crate::query::refs as query_refs;
    use crate::resolve::refs;
    let listing = sites.is_some();
    let partial = warming().is_some();
    // `--dead` asks about every `initialize`, and each would tier every
    // `X.new` in the checkout: those are worked out once instead (DEC-541).
    if let Some(parsed) = parsed
        && !keep_all
        && !listing
        && !partial
        && let Some(own) = own_initialize(tree, query, target)
    {
        return gather_constructions(tree, store, root, root_str, query, target, &own, parsed);
    }
    let files = store.files_calling_any(root_str, &refs::called_as(query))?;
    // Every worker tiers against the one tree, which is shared (DEC-250),
    // and files come back in the order they were listed.
    // Excluded sites are counted, not listed: the count is the product, and
    // the list would be the grep we are trying to beat. `keep_all` is how
    // `--include-excluded` makes the claim auditable.
    let keep = |tiered: &mut Tiered, reference: refs::Reference| {
        tiered.counts.record(&reference);
        if keep_all || reference.tier != refs::Tier::Excluded {
            tiered.found.push(reference);
        }
    };
    let asked = query_refs::target(tree, query, target, |path| match parsed {
        Some(parsed) => {
            parsed.read(root, &[path.to_string()]);
            parsed.get(path)
        }
        None => extract::read_file(root, path).map(Arc::new),
    });
    let tier = |path: &String, facts: &crate::core::Facts| {
        let mut tiered = Tiered::default();
        for call in facts.calls.iter().filter(|c| refs::names_it(c, query)) {
            if listing {
                tiered.sites.push(crate::store::Ref::call(path, call));
            }
            let reference = query_refs::tier(tree, facts, call, path, &asked, partial);
            // Its includers may answer it: asked below, one file at a time.
            match query_refs::rescuable(facts, call, &asked, &reference) {
                true => tiered.rescuable.push((call.clone(), reference)),
                false => keep(&mut tiered, reference),
            }
        }
        tiered
    };
    let read = |path: &String| extract::read_file(root, path);
    let tiered: Vec<Tiered> = match parsed {
        // Held across queries by the caller: parse what it lacks, then tier.
        Some(parsed) => {
            parsed.read(root, &files);
            files
                .par_iter()
                .filter_map(|path| Some((path, parsed.get(path)?)))
                .map(|(path, facts)| tier(path, &facts))
                .collect()
        }
        // One query: each file is parsed and tiered on the worker that takes
        // it, and only the files in flight are held.
        None => files
            .par_iter()
            .filter_map(|path| Some(tier(path, &read(path)?)))
            .collect(),
    };
    let mut merged = Tiered::default();
    let mut rescuable = Vec::new();
    for mut file in tiered {
        merged.found.append(&mut file.found);
        merged.counts.add(&file.counts);
        rescuable.append(&mut file.rescuable);
        if let Some(sites) = sites.as_deref_mut() {
            sites.append(&mut file.sites);
        }
    }
    if !rescuable.is_empty() {
        let files = crate::query::members::CheckoutFiles::new(store, root, root_str);
        for (call, mut reference) in rescuable {
            query_refs::rescue(tree, &files, &call, &asked, &mut reference);
            keep(&mut merged, reference);
        }
    }
    let Tiered {
        mut found, counts, ..
    } = merged;
    found.sort_by_key(refs::order);
    Ok((found, counts))
}

/// One file's call sites, tiered.
#[derive(Default)]
struct Tiered {
    found: Vec<crate::resolve::refs::Reference>,
    counts: crate::resolve::refs::Counts,
    sites: Vec<crate::store::Ref>,
    /// Calls ruled out that a shared group's includers may answer, counted
    /// once they have been asked.
    rescuable: Vec<(crate::core::Call, crate::resolve::refs::Reference)>,
}

/// Files held across queries, by checkout-relative path: each one's facts,
/// `None` when it could not be read; and what `--dead`'s `initialize`
/// queries share (DEC-541), each worked out the first time one needs it.
/// Shared by threads: every entry is a pure function of the checkout, so
/// which thread fills one changes no answer. No lock is held across a
/// rayon call — a worker waiting in one runs other queries, which take it.
#[derive(Default)]
pub(super) struct Parsed {
    facts: RwLock<HashMap<String, Option<Arc<crate::core::Facts>>>>,
    /// Every call named `initialize` — a subclass's `super`, mostly.
    callers: OnceLock<Vec<Caller>>,
    /// The files that call `new`, by each capitalized word their text holds.
    naming: OnceLock<HashMap<u64, Vec<u32>>>,
    /// What an `X.new` site constructs, by file and call index.
    made: RwLock<HashMap<(String, usize), crate::resolve::refs::Construct>>,
    /// The files that call `new`, by path.
    calling_new: OnceLock<Vec<String>>,
}

/// `cell`'s value, worked out by `make` if it has none. Not `get_or_init`:
/// `make` may wait in a rayon call, whose worker may come back here.
fn once<T>(cell: &OnceLock<T>, make: impl FnOnce() -> T) -> &T {
    if cell.get().is_none() {
        let _ = cell.set(make());
    }
    cell.get().expect("just set")
}

impl Parsed {
    /// Holds a file's facts, parsed elsewhere.
    pub(super) fn hold(&self, path: String, facts: Arc<crate::core::Facts>) {
        self.facts
            .write()
            .expect("unpoisoned")
            .insert(path, Some(facts));
    }

    /// A file's facts, if it has been read and could be.
    pub(super) fn get(&self, path: &str) -> Option<Arc<crate::core::Facts>> {
        self.facts.read().expect("unpoisoned").get(path)?.clone()
    }

    /// Parse the files not held yet.
    fn read(&self, root: &Path, files: &[String]) {
        let fresh: Vec<_> = files
            .par_iter()
            .filter(|path| !self.facts.read().expect("unpoisoned").contains_key(*path))
            .map(|path| {
                let facts = extract::read_file(root, path).map(|mut facts| {
                    facts.share_nestings();
                    Arc::new(facts)
                });
                (path.clone(), facts)
            })
            .collect();
        let mut facts = self.facts.write().expect("unpoisoned");
        for (path, read) in fresh {
            facts.entry(path).or_insert(read);
        }
    }

    /// What `new` call `at` of `path` constructs, once `construct` has
    /// worked it out.
    fn made(&self, path: &str, at: usize) -> crate::resolve::refs::Construct {
        self.made.read().expect("unpoisoned")[&(path.to_string(), at)].clone()
    }

    /// What each `new` call of these files constructs, worked out for the
    /// ones not known yet.
    fn construct(&self, tree: &Tree, files: &[String]) {
        use crate::resolve::refs;
        let fresh: Vec<_> = files
            .par_iter()
            .filter_map(|path| Some((path, self.get(path)?)))
            .flat_map_iter(|(path, facts)| {
                let made = self.made.read().expect("unpoisoned");
                let fresh: Vec<_> = new_calls(&facts)
                    .filter(|(at, _)| !made.contains_key(&(path.clone(), *at)))
                    .map(|(at, call)| (at, call.clone()))
                    .collect();
                drop(made);
                fresh
                    .into_iter()
                    .map(|(at, call)| {
                        (
                            (path.clone(), at),
                            refs::construct(tree, &facts, &call, path),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        let mut made = self.made.write().expect("unpoisoned");
        for (at, construct) in fresh {
            made.entry(at).or_insert(construct);
        }
    }

    /// The checkout's calls named `initialize`, read once.
    fn callers(&self, tree: &Tree, store: &Store, root: &Path, root_str: &str) -> &[Caller] {
        once(&self.callers, || {
            let files = store
                .files_calling(root_str, "initialize")
                .unwrap_or_default();
            self.read(root, &files);
            files
                .par_iter()
                .filter_map(|path| Some((path, self.get(path)?)))
                .flat_map_iter(|(path, facts)| {
                    facts
                        .calls
                        .iter()
                        .enumerate()
                        .filter(|(_, call)| call.name == "initialize")
                        .map(|(at, call)| Caller {
                            path: path.clone(),
                            at,
                            known: known_chain(tree, call),
                        })
                        .collect::<Vec<_>>()
                })
                .collect()
        })
    }

    fn calling_new(&self, store: &Store, root_str: &str) -> &[String] {
        once(&self.calling_new, || {
            let mut files = store.files_calling(root_str, "new").unwrap_or_default();
            files.sort();
            files
        })
    }

    /// The files calling `new` whose text names `word`.
    fn naming(&self, store: &Store, root: &Path, root_str: &str, word: &str) -> Vec<String> {
        let files = self.calling_new(store, root_str);
        let naming = once(&self.naming, || {
            let words: Vec<HashSet<u64>> = files
                .par_iter()
                .map(|path| {
                    capitalized_words(
                        &crate::scan::read_source(root.join(path)).unwrap_or_default(),
                    )
                })
                .collect();
            let mut naming: HashMap<u64, Vec<u32>> = HashMap::new();
            for (at, words) in words.into_iter().enumerate() {
                for word in words {
                    naming.entry(word).or_default().push(at as u32);
                }
            }
            naming
        });
        // A hash shared by two words only adds a file to check.
        naming
            .get(&word_hash(word.as_bytes()))
            .map(|at| at.iter().map(|&at| files[at as usize].clone()).collect())
            .unwrap_or_default()
    }
}

/// A call named `initialize`, and for a `super` in a class whose whole
/// chain is indexed, that chain: it lands on no `initialize` of an owner
/// outside it.
struct Caller {
    path: String,
    at: usize,
    known: Option<KnownChain>,
}

struct KnownChain {
    class: String,
    chain: HashSet<String>,
}

/// The chain of the class a `super` is written in, when the index holds all
/// of it.
fn known_chain(tree: &Tree, call: &crate::core::Call) -> Option<KnownChain> {
    if call.recv != crate::core::RecvShape::Super {
        return None;
    }
    let scope = tree.scope_fqn(&call.nesting)?;
    if tree.kind_of(&scope) != Some("class") {
        return None;
    }
    let chain = tree.ancestors(&scope);
    if !chain.unresolved.is_empty() {
        return None;
    }
    Some(KnownChain {
        class: crate::tree::public_name(&scope).to_string(),
        chain: chain
            .chain
            .iter()
            .map(|a| crate::tree::public_name(a).to_string())
            .collect(),
    })
}

/// A file's calls of `new` that may construct, by index.
fn new_calls(facts: &crate::core::Facts) -> impl Iterator<Item = (usize, &crate::core::Call)> {
    facts
        .calls
        .iter()
        .enumerate()
        .filter(|(_, call)| call.name == "new" && call.recv != crate::core::RecvShape::Super)
}

/// Every word of a source that starts with a capital, hashed: the
/// constants it may name, and some words of its comments and strings.
fn capitalized_words(text: &[u8]) -> HashSet<u64> {
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut words = HashSet::new();
    let mut at = 0;
    while at < text.len() {
        if !word(text[at]) {
            at += 1;
            continue;
        }
        let start = at;
        while at < text.len() && word(text[at]) {
            at += 1;
        }
        if text[start].is_ascii_uppercase() {
            words.insert(word_hash(&text[start..at]));
        }
    }
    words
}

/// FNV-1a: a word's key in the index, cheap to make by the million.
fn word_hash(word: &[u8]) -> u64 {
    word.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &b| {
        (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The `initialize` an instance of an `initialize` query's owner runs —
/// its own, or a module's it prepends. `None` for any other query.
fn own_initialize(
    tree: &Tree,
    query: &crate::resolve::refs::Query,
    target: Option<&str>,
) -> Option<crate::tree::MethodDef> {
    crate::resolve::refs::constructor_of(query)?;
    tree.lookup(target?, false, &query.name)
}

/// `gather_refs` for an `initialize` its owner defines, across `--dead`'s
/// queries, which needs only whether a site reaches it (DEC-541): `--dead`
/// reports one nothing reaches. The calls of its own name are tiered —
/// every subclass's `super`, read once per run — then the `X.new`s in files
/// that name a class that runs it or one it inherits from; the first site
/// that reaches it and is not any class's settles it. One nothing reaches
/// gets the first untyped `new` that could, for its caveat.
#[allow(clippy::too_many_arguments)]
fn gather_constructions(
    tree: &Tree,
    store: &Store,
    root: &Path,
    root_str: &str,
    query: &crate::resolve::refs::Query,
    target: Option<&str>,
    own: &crate::tree::MethodDef,
    parsed: &Parsed,
) -> anyhow::Result<(
    Vec<crate::resolve::refs::Reference>,
    crate::resolve::refs::Counts,
)> {
    use crate::resolve::refs;
    let owner = target.unwrap_or_default();
    // What an `initialize` is defined on is not asked: `--dead` reports none
    // defined on an object, and the calls here are `super`s and `new`s.
    let asked = refs::Target {
        query,
        owner: target,
        on: Vec::new(),
    };
    let mut found = Vec::new();
    let mut counts = refs::Counts::default();
    let keep = |counts: &mut refs::Counts,
                found: &mut Vec<refs::Reference>,
                reference: refs::Reference| {
        counts.record(&reference);
        if reference.tier != refs::Tier::Excluded {
            found.push(reference);
        }
    };
    let reaches = |found: &[refs::Reference]| found.iter().any(|r| r.unplaced().is_none());
    // An `X.new` that reaches it names its class or a class that runs it (a
    // subclass, or a class mixing it in); a `self.new` that may is in a
    // class method of a class it inherits from, written where that class is.
    let runs_own = |class: &str| {
        tree.lookup(class, false, "initialize")
            .is_some_and(|m| m.site.path == own.site.path && m.site.line == own.site.line)
    };
    // A name written in a file is its last segment, and the file names the
    // outermost namespace too — to open it, or to write the whole path.
    let mut names: Vec<(String, String)> = Vec::new();
    for class in std::iter::once(owner.to_string())
        .chain(tree.includers_of(owner).into_iter().filter(|c| runs_own(c)))
    {
        let name = crate::tree::public_name(&class);
        let last = name.rsplit("::").next().unwrap_or(name).to_string();
        let first = name.split("::").next().unwrap_or(name).to_string();
        if !last.is_empty() && !names.contains(&(last.clone(), first.clone())) {
            names.push((last, first));
        }
    }
    let mut files: Vec<String> = Vec::new();
    for (last, first) in &names {
        let naming = parsed.naming(store, root, root_str, last);
        let outer: HashSet<String> = parsed
            .naming(store, root, root_str, first)
            .into_iter()
            .collect();
        files.extend(naming.into_iter().filter(|path| outer.contains(path)));
    }
    let calling: HashSet<String> = parsed
        .calling_new(store, root_str)
        .iter()
        .cloned()
        .collect();
    for ancestor in &tree.ancestors(owner).chain {
        files.extend(
            tree.sites(ancestor)
                .iter()
                .filter_map(|site| site.path.strip_prefix(root_str)?.strip_prefix('/'))
                .filter(|path| calling.contains(*path))
                .map(str::to_string),
        );
    }
    files.sort();
    files.dedup();
    parsed.read(root, &files);
    parsed.construct(tree, &files);
    for path in &files {
        let Some(facts) = parsed.get(path) else {
            continue;
        };
        for call in facts.calls.iter().filter(|c| c.name == "initialize") {
            let reference = refs::tier_call(tree, &facts, call, path, &asked);
            if reference.tier != refs::Tier::Excluded && reference.unplaced().is_none() {
                keep(&mut counts, &mut found, reference);
                return Ok((found, counts));
            }
        }
        for (at, call) in new_calls(&facts) {
            let construct = parsed.made(path, at);
            if !refs::may_run(&construct, own) {
                continue;
            }
            let reference =
                refs::tier_call_with(tree, &facts, call, path, &asked, Some(&construct));
            if reference.tier != refs::Tier::Excluded && reference.unplaced().is_none() {
                keep(&mut counts, &mut found, reference);
                return Ok((found, counts));
            }
        }
    }
    // Then every call of its name: a subclass's `super` may be in a file
    // that calls no `new`.
    for caller in parsed.callers(tree, store, root, root_str) {
        // Every subclass's `initialize` calls `super`; one in a class whose
        // whole chain is known and does not hold the owner lands elsewhere,
        // which is all `tier_super` would find, at length.
        if let Some(known) = &caller.known
            && known.class != owner
            && !known.chain.contains(owner)
        {
            counts.excluded += 1;
            counts.excluded_different_owner += 1;
            continue;
        }
        let Some(facts) = parsed.get(&caller.path) else {
            continue;
        };
        let call = &facts.calls[caller.at];
        let reference = refs::tier_call(tree, &facts, call, &caller.path, &asked);
        keep(&mut counts, &mut found, reference);
    }
    if reaches(&found) {
        return Ok((found, counts));
    }
    // Nothing placed reaches it: the first untyped `new` that could, by
    // file and line, is all its caveat says of those.
    let calling_new = parsed.calling_new(store, root_str).to_vec();
    for path in &calling_new {
        let one = std::slice::from_ref(path);
        parsed.read(root, one);
        parsed.construct(tree, one);
        let Some(facts) = parsed.get(path) else {
            continue;
        };
        let first = new_calls(&facts)
            .filter(|(at, call)| {
                matches!(parsed.made(path, *at), refs::Construct::Untyped) && own.accepts(call.argc)
            })
            .min_by_key(|(_, call)| call.pos.line);
        if let Some((_, call)) = first {
            keep(
                &mut counts,
                &mut found,
                refs::untyped_construction(call, path, own),
            );
            break;
        }
    }
    found.sort_by_key(refs::order);
    Ok((found, counts))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_files_capitalized_words_are_its_whole_identifiers() {
        let words = capitalized_words(b"Admin::Widget.new(x_Widget, :Gadget) # Widgets");
        let want: HashSet<u64> = ["Admin", "Gadget", "Widget", "Widgets"]
            .iter()
            .map(|w| word_hash(w.as_bytes()))
            .collect();
        assert_eq!(words, want);
    }

    /// Tiering on every worker against one tree gives what one thread gives,
    /// in the same order, whether the files' facts are kept or not (DEC-250).
    #[test]
    fn workers_sharing_a_tree_tier_as_one_thread_does() {
        use crate::resolve::refs;
        let dir = std::env::temp_dir().join(format!("trekr-shared-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let repo = dir.join("repo");
        let mut sources = vec![(
            "lib/widget.rb".to_string(),
            "class Widget\n  def save(a) = a\nend\nclass Gadget\n  def save = 1\nend\n".to_string(),
        )];
        // Receivers every rung decides, in more files than a worker takes.
        for i in 0..67 {
            let body = match i % 4 {
                0 => "Widget.new.save(1)\n".to_string(),
                1 => "g = Gadget.new\ng.save\n".to_string(),
                2 => format!("class Widget\n  def go{i} = save(2)\nend\n"),
                _ => "thing.save\nWidget.new.save\n".to_string(),
            };
            sources.push((format!("app/f{i:03}.rb"), body));
        }
        let mut files = crate::scan::Files::new();
        let mut facts = Vec::new();
        for (path, source) in &sources {
            std::fs::create_dir_all(repo.join(path).parent().unwrap()).unwrap();
            std::fs::write(repo.join(path), source).unwrap();
            let oid = scan::hash_blob(source.as_bytes());
            files.insert(path.clone(), oid.clone());
            facts.push((oid, extract::extract(source.as_bytes())));
        }
        let root = repo.to_string_lossy().into_owned();
        let db = dir.join("store.db");
        Store::open(&db)
            .unwrap()
            .write(&root, &files, facts, 0)
            .unwrap();
        let query = refs::Query::parse("Widget#save");
        let answer = |threads: usize, keep: bool| {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap();
            pool.install(|| {
                // A connection stays on the thread that opened it.
                let store = Store::open(&db).unwrap();
                let tree = Tree::build(&store, &root).unwrap();
                let parsed = Parsed::default();
                let mut sites = Vec::new();
                let (found, counts) = gather_refs(
                    &tree,
                    &store,
                    &repo,
                    &root,
                    &query,
                    Some("Widget"),
                    true,
                    keep.then_some(&parsed),
                    Some(&mut sites),
                )
                .unwrap();
                let sites: Vec<_> = sites.iter().map(|s| (&s.path, s.line, s.col)).collect();
                serde_json::to_string(&(found, counts, format!("{sites:?}"))).unwrap()
            })
        };
        let alone = answer(1, false);
        assert!(alone.contains("confirmed") && alone.contains("excluded"));
        for keep in [false, true] {
            assert_eq!(answer(8, keep), alone, "keeping facts: {keep}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
