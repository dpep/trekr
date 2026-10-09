//! A query's freshness (DEC-035): what changed in the working tree since
//! the index, read for that command alone, and how the answer says so.

use super::*;

/// What a query read from the working tree in place of the index, and why
/// the rest may still lag (DEC-035).
///
/// The model: the store's map is what `--index` read, and a query writes no
/// map — only the facts of new bytes, content-addressed. Each query compares the working tree with the store and
/// answers as if the store held what it found (`Store::overlay`), for that
/// query alone — so a reverted edit simply stops being overlaid, and an
/// untracked file is read like a tracked one.
struct Freshness {
    root: String,
    /// The file the question is about, relative to `root`, when it is one.
    queried: Option<String>,
    /// Read as they are now rather than as indexed: edited, added or deleted
    /// since `--index`.
    refreshed: Vec<String>,
    /// Changed, and left at the indexed version: another trekr is writing,
    /// so a new blob's facts could not be recorded.
    busy: Vec<String>,
    /// Why files beyond those read may still differ from the index, when
    /// they may: too many changed, or git could not say what did.
    lag: Option<String>,
    /// What the answer reads in place of the index, applied to every store
    /// connection this command opens on `root`.
    overlay: crate::store::Overlay,
}

impl Freshness {
    fn hint(&self) -> String {
        format!("trekr --index {}", paths::pretty(&self.root))
    }

    fn stale(&self) -> bool {
        self.lag.is_some() || !self.busy.is_empty()
    }

    /// Why files beyond those read may differ from the index, when they may:
    /// `index.cause`, and the text answer's caveat.
    fn cause(&self) -> Option<String> {
        self.lag.clone()
    }

    /// On stderr, so stdout stays the answer.
    fn say(&self) {
        if !self.refreshed.is_empty() {
            eprintln!(
                "trekr: {} changed since the index — read as it is now",
                self.refreshed.join(", ")
            );
        }
        if !self.busy.is_empty() {
            eprintln!(
                "trekr: {} changed since the index, which another trekr is writing — \
                 answered from the indexed version",
                self.busy.join(", ")
            );
        }
        if let Some(lag) = &self.lag {
            eprintln!("trekr: {lag}; other files may lag ({})", self.hint());
        }
    }
}

/// Each checkout this command checked, and what it found — `None`, current.
static CHECKED: std::sync::Mutex<Vec<(String, Option<Freshness>)>> =
    std::sync::Mutex::new(Vec::new());

fn checked() -> std::sync::MutexGuard<'static, Vec<(String, Option<Freshness>)>> {
    CHECKED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// `index` beside a JSON answer, from what this command's checks found:
/// absent when every checkout it asked was current. Several checkouts (a
/// `--dead` across two) are said as one.
pub(super) fn index_note() -> Option<serde_json::Value> {
    let checked = checked();
    let found: Vec<&Freshness> = checked.iter().filter_map(|(_, f)| f.as_ref()).collect();
    if found.is_empty() {
        return None;
    }
    let hint: Vec<String> = found.iter().map(|f| f.hint()).collect();
    // `refreshed` and `busy` name the file asked about, as they always have;
    // `refreshed_files` and `busy_files` list every file.
    let asked = |list: fn(&Freshness) -> &Vec<String>| {
        found.iter().find_map(|f| {
            let queried = f.queried.as_ref()?;
            list(f).contains(queried).then(|| queried.clone())
        })
    };
    let mut value = serde_json::json!({
        "stale": found.iter().any(|f| f.stale()),
        "refreshed": asked(|f| &f.refreshed),
        "refreshed_files": found.iter().flat_map(|f| f.refreshed.iter()).collect::<Vec<_>>(),
        "hint": hint.join(" && "),
    });
    let busy: Vec<&String> = found.iter().flat_map(|f| f.busy.iter()).collect();
    if !busy.is_empty() {
        value["busy"] = asked(|f| &f.busy).into();
        value["busy_files"] = serde_json::json!(busy);
    }
    let causes: Vec<String> = found.iter().filter_map(|f| f.cause()).collect();
    if !causes.is_empty() {
        value["cause"] = causes.join("; ").into();
    }
    Some(value)
}

/// Why files beyond those this command read may differ from the index, when
/// they may.
pub(super) fn lag() -> Option<String> {
    let checked = checked();
    let causes: Vec<String> = checked
        .iter()
        .filter_map(|(_, found)| found.as_ref()?.cause())
        .collect();
    (!causes.is_empty()).then(|| causes.join("; "))
}

/// Read what changed since the index into `store`, for this command only
/// (DEC-035): every file `probe` finds edited, added or deleted, up to
/// `BULK`, and the file asked about whatever it found. Once per checkout
/// per command — a second store opened on a checkout already checked
/// (`probe` is `None`) gets the same reading. Says what it found
/// (`Freshness::say`, `index_note`); returns whether the store now answers
/// differently, so a tree built before must be built again.
pub(super) fn freshen(
    out: Output,
    store: &mut Store,
    root: &Path,
    queried: Option<&Path>,
    probe: Option<Probe>,
) -> bool {
    let root_str = root.to_string_lossy().into_owned();
    let Some(probe) = probe else {
        let overlay = checked()
            .iter()
            .find(|(known, _)| *known == root_str)
            .and_then(|(_, found)| Some(found.as_ref()?.overlay.clone()))
            .unwrap_or_default();
        // Not read twice: what it found was said when it was.
        return store.overlay(&root_str, &overlay).unwrap_or(false);
    };
    let (found, changed) = refresh_for_query(store, root, queried, probe);
    if let Some(found) = &found {
        crate::usage::flag("stale");
        if out == Output::Text {
            found.say();
        }
    }
    checked().push((root_str, found));
    changed
}

/// What changed since the index, as git found it and compared with the map
/// the store holds; `None` when the map could not be read.
pub(super) type Probe = scan::Probe<Option<Compared>>;

pub(super) struct Compared {
    /// The map as indexed, path → blob oid.
    stored: HashMap<String, String>,
    /// Every path whose blob differs, and every one gone, in path order.
    changed: std::collections::BTreeMap<String, Option<crate::core::Oid>>,
}

/// The working tree of `root`, begun reading — unless this command has
/// checked `root` already. The stored map is read on a connection of its
/// own while git runs, and compared on git's thread: none of it waits
/// behind the tree build.
pub(super) fn probe(store: &Store, root: &Path) -> Option<Probe> {
    let root_str = root.to_string_lossy().into_owned();
    if checked().iter().any(|(known, _)| *known == root_str) {
        return None;
    }
    let open = store.opener();
    let stored = std::thread::spawn(move || open?().ok()?.file_map(&root_str).ok());
    Some(scan::Probe::start(root, move |now| {
        let stored = stored.join().ok().flatten()?;
        let gone = stored
            .keys()
            .filter(|path| !now.contains_key(*path))
            .map(|path| (path.clone(), None));
        let mut changed: std::collections::BTreeMap<_, _> = gone.collect();
        changed.extend(
            now.into_iter()
                .filter(|(path, oid)| stored.get(path) != Some(&oid.0))
                .map(|(path, oid)| (path, Some(oid))),
        );
        Some(Compared { stored, changed })
    }))
}

/// Read what changed into `store` (`changes`), as its overlay on `root` —
/// replacing one a guess put there (`Store::resume`). What it found, and
/// whether the store now answers differently than it did.
fn refresh_for_query(
    store: &mut Store,
    root: &Path,
    queried: Option<&Path>,
    probe: Probe,
) -> (Option<Freshness>, bool) {
    let root_str = root.to_string_lossy();
    let mut found = changes(store, root, queried, probe);
    let none = crate::store::Overlay::default();
    let overlay = found.as_ref().map_or(&none, |f| &f.overlay);
    match store.overlay(&root_str, overlay) {
        Ok(changed) => (found, changed),
        Err(_) => {
            if let Some(found) = &mut found {
                found.lag = Some("the edits since the index could not be read".to_string());
                found.refreshed.clear();
                found.overlay = Default::default();
            }
            (found, store.overlay(&root_str, &none).unwrap_or(true))
        }
    }
}

fn changes(
    store: &mut Store,
    root: &Path,
    queried: Option<&Path>,
    probe: Probe,
) -> Option<Freshness> {
    let root_str = root.to_string_lossy().into_owned();
    // A gem never moves; a checkout a first index is still filling already
    // says its answers are partial (DEC-320).
    if !store.is_repo(&root_str).unwrap_or(false)
        || store.warming(&root_str).ok().flatten().is_some()
    {
        return None;
    }
    let queried = queried
        .and_then(|file| std::fs::canonicalize(file).ok())
        .and_then(|file| Some(file.strip_prefix(root).ok()?.to_string_lossy().into_owned()));
    let mut lag = None;
    let (stored, mut changed) = match probe.finish() {
        Ok(compared) => {
            let Compared { stored, changed } = compared?;
            (stored, changed)
        }
        Err(why) => {
            lag = Some(why);
            (
                store.file_map(&root_str).ok()?,
                std::collections::BTreeMap::new(),
            )
        }
    };
    // More is an operation on the checkout — a branch switch, a rebase —
    // which `--index` reads at once rather than every query one by one.
    if changed.len() > scan::BULK {
        lag = Some(format!(
            "{} files changed since the index, more than a query reads",
            changed.len()
        ));
        changed.retain(|path, _| Some(path) == queried.as_ref());
    }
    // The file asked about is read whatever git said: its bytes are the
    // question.
    if let Some(path) = &queried
        && scan::is_indexed(path)
        && !changed.contains_key(path)
        && let Ok(bytes) = scan::read_source(root.join(path))
    {
        let oid = scan::hash_blob(&bytes);
        if stored.get(path) != Some(&oid.0) {
            changed.insert(path.clone(), Some(oid));
        }
    }
    let (mut refreshed, mut busy, mut overlay) = (Vec::new(), Vec::new(), Vec::new());
    for (path, oid) in changed {
        let oid = match oid {
            Some(oid) if !store.has_blob(&oid).unwrap_or(false) => {
                // New bytes: parsed, and their facts recorded — content-
                // addressed, so the next index finds them known.
                let Ok(bytes) = scan::read_source(root.join(&path)) else {
                    lag.get_or_insert_with(|| format!("{path} could not be read"));
                    continue;
                };
                // Read again, the bytes may have moved since git looked.
                let oid = scan::hash_blob(&bytes);
                if !store.has_blob(&oid).unwrap_or(false) {
                    let facts = crate::extract::extract_file(&path, &bytes);
                    // Busy: another process is writing the index. The answer
                    // comes from what it has committed (DEC-066).
                    match store.add_blob(&oid, &facts) {
                        Ok(()) => {}
                        Err(error) if crate::store::is_busy(&error) => {
                            busy.push(path);
                            continue;
                        }
                        Err(_) => {
                            lag.get_or_insert_with(|| format!("{path} could not be read"));
                            continue;
                        }
                    }
                }
                Some(oid)
            }
            known => known,
        };
        refreshed.push(path.clone());
        overlay.push((path, oid));
    }
    (lag.is_some() || !refreshed.is_empty() || !busy.is_empty()).then_some(Freshness {
        root: root_str,
        queried,
        refreshed,
        busy,
        lag,
        overlay: overlay.into_iter().collect(),
    })
}

/// The tree a query answers from, with what changed since the index read in
/// first. git's look at the working tree runs while the tree is built, which
/// is most queries' whole answer. The tree is built over what the last query
/// read (`Store::resume`) — the same edits, most often — and built again
/// when git finds otherwise.
pub(super) fn fresh_tree(
    out: Output,
    store: &mut Store,
    root: &Path,
    queried: Option<&Path>,
) -> anyhow::Result<OneShotTree> {
    let root_str = root.to_string_lossy();
    let Some(probe) = probe(store, root) else {
        freshen(out, store, root, queried, None);
        return build_tree(store, &root_str);
    };
    // A guess, and only that: `freshen` puts what git found in its place.
    let _ = store.resume(&root_str);
    let tree = build_tree(store, &root_str)?;
    Ok(match freshen(out, store, root, queried, Some(probe)) {
        true => build_tree(store, &root_str)?,
        false => tree,
    })
}
