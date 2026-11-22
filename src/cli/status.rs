//! `--status` and `--gc`: what the store holds, and removing what nothing
//! will ask about again.

use super::*;

/// What is indexed. By default the checkout this is run in (or `--context`
/// names), with its gems counted rather than listed — a Rails app's bundle is
/// hundreds of them — and a count of the rest; `--all` lists every checkout.
/// Outside any checkout, the repos are listed and the gems counted. A checkout
/// nobody indexed is `not_indexed`, exit 2, as a query from it would be.
pub(super) fn cmd_status(
    out: Output,
    all: bool,
    context: Option<&Path>,
) -> anyhow::Result<ExitCode> {
    let retention = configured_retention()?;
    let store = open_store()?;
    let checkouts = store.status()?;
    let totals = store.totals()?;
    // An upgrade drops the index, and an empty store must say so, as a
    // query does, rather than read as never used.
    let reason = match checkouts.is_empty() {
        true => Some(match store.upgraded_from()? {
            Some(from) => upgrade_reason(from),
            None => "nothing has been indexed yet".to_string(),
        }),
        false => None,
    };
    let asked = match (all, context) {
        (true, _) => None,
        (false, Some(dir)) => {
            if !dir.exists() {
                return Err(Failure::NotFound.error(format!("no such path: {}", dir.display())));
            }
            Some(status_checkout(&store, dir)?)
        }
        // Outside any checkout there is no "this checkout" to answer for.
        (false, None) => status_checkout(&store, Path::new(".")).ok(),
    };
    let asked = asked.map(|root| root.to_string_lossy().into_owned());
    if let Some(root) = &asked
        && !store.has_checkout(root)?
    {
        return status_not_indexed(out, Path::new(root), &store, &checkouts, &totals, retention);
    }
    let here = asked;
    let shown: Vec<&crate::store::Checkout> = checkouts
        .iter()
        .filter(|c| {
            all || here
                .as_ref()
                .map_or(c.kind == "repo", |root| c.repo == *root)
        })
        .collect();
    let indexed_files: HashMap<&str, i64> = checkouts
        .iter()
        .map(|c| (c.repo.as_str(), c.files))
        .collect();
    let mut rows: Vec<serde_json::Value> = Vec::new();
    // Each checkout's Ruby in words, for text.
    let mut rubies: HashMap<String, String> = HashMap::new();
    // Counted on a row above, so not among the others.
    let mut counted: HashSet<String> = HashSet::new();
    for checkout in &shown {
        let mut row = serde_json::to_value(checkout)?;
        // Its file count is what is in so far (DEC-320).
        if let Some(warming) = store.warming(&checkout.repo)? {
            row["warming"] = warming_note(&checkout.repo, &warming);
        }
        if !all && checkout.kind == "repo" {
            let stdlib = store.tree_roots(&checkout.repo)?.stdlib;
            let used: Vec<String> = store
                .gems_used(&checkout.repo)?
                .into_iter()
                .filter(|gem| Some(gem) != stdlib.as_ref())
                .collect();
            counted.extend(used.iter().cloned());
            if let Some(version) = crate::gems::stdlib::named_missing(Path::new(&checkout.repo)) {
                row["ruby_not_found"] = version.into();
            }
            let ruby = status_ruby(&checkout.repo, stdlib.as_deref());
            let unmet = match (&ruby, stdlib.as_deref()) {
                (Some(ruby), Some(root)) if ruby.fallback != Some(false) => {
                    crate::gems::stdlib::unmet(Path::new(&checkout.repo), Path::new(root))
                }
                _ => None,
            };
            if let Some(ruby) = &ruby {
                rubies.insert(checkout.repo.clone(), ruby_said(ruby, unmet.is_some()));
            }
            if let Some(requirements) = unmet {
                row["ruby_unmet"] = requirements.into();
            }
            row["ruby"] = serde_json::to_value(ruby)?;
            if let Some(stdlib) = stdlib.clone() {
                let rbs = store.rbs_about(&stdlib)?.map(|about| {
                    serde_json::json!({
                        "version": about.version,
                        "path": about.dir,
                        "chosen": about.chosen,
                    })
                });
                row["stdlib"] = serde_json::json!({
                    "root": stdlib,
                    "files": indexed_files.get(stdlib.as_str()).copied().unwrap_or(0),
                    "hidden": store.hidden_default_gems(&checkout.repo)?,
                    "rbs": rbs,
                });
                counted.insert(stdlib);
            }
            let indexed: Vec<i64> = used
                .iter()
                .filter_map(|gem| indexed_files.get(gem.as_str()).copied())
                .filter(|files| *files > 0)
                .collect();
            row["gems"] = serde_json::json!({
                "count": used.len(),
                "indexed": indexed.len(),
                "files": indexed.iter().sum::<i64>(),
            });
            // The store keeps only what was indexed, so the holes are read
            // again, as the next index would find them.
            let ruby_now = stdlib.as_deref().and_then(|root| {
                crate::gems::stdlib::for_checkout(Path::new(&checkout.repo), Some(Path::new(root)))
            });
            let (located, _) =
                crate::gems::for_checkout(Path::new(&checkout.repo), ruby_now.as_ref());
            let (missing, unlocated) = index::gem_holes(&located, ruby_now.as_ref());
            if !missing.is_empty() {
                row["gems"]["missing"] = serde_json::to_value(missing)?;
            }
            if !unlocated.is_empty() {
                row["gems"]["unlocated"] = serde_json::to_value(unlocated)?;
            }
        }
        rows.push(row);
    }
    let hidden = |kind: &str| {
        checkouts
            .iter()
            .filter(|c| (c.kind == "repo") == (kind == "repo"))
            .filter(|c| !shown.iter().any(|s| s.repo == c.repo) && !counted.contains(&c.repo))
            .count()
    };
    let others = serde_json::json!({ "repos": hidden("repo"), "gems": hidden("gem") });
    let kept = crate::store::kept(&crate::store::default_path()?, crate::store::VERSION);

    if out != Output::Text {
        // One object, because the totals are the point: they are what N
        // checkouts share, not the sum of what each one costs.
        let mut answer = serde_json::json!({
            "checkouts": rows,
            "others": others,
            "totals": totals,
            "storage": storage(&store, retention)?,
            "kept": kept,
        });
        if let Some(reason) = &reason {
            answer["reason"] = reason.as_str().into();
        }
        emit_json(out, &answer)?;
        return Ok(exit_on(!checkouts.is_empty()));
    }
    if let Some(reason) = reason {
        println!("{reason} — the first query in a checkout indexes it, or `trekr --index` does");
        return Ok(ExitCode::from(1));
    }
    for row in &rows {
        println!(
            "{:>7} files  {:>7} blobs  {}",
            row["files"].as_i64().unwrap_or(0),
            row["blobs"].as_i64().unwrap_or(0),
            paths::pretty(row["repo"].as_str().unwrap_or_default())
        );
        if let Some(warming) = row["warming"].as_object() {
            let (read, of) = (&warming["read"], &warming["of"]);
            match warming["interrupted"].as_bool() {
                Some(true) => println!(
                    "{:>32}! cut short: {read} of {of} files read — `{}` finishes it",
                    "",
                    warming["hint"].as_str().unwrap_or_default()
                ),
                _ => println!(
                    "{:>32}! being indexed: {read} of {of} files read so far",
                    ""
                ),
            }
        }
        if let Some(ruby) = row["repo"].as_str().and_then(|repo| rubies.get(repo)) {
            println!("{:>32}+ {ruby}", "");
        }
        if let Some(stdlib) = row["stdlib"]["root"].as_str() {
            println!(
                "{:>32}+ stdlib, {} files: {}",
                "",
                row["stdlib"]["files"].as_i64().unwrap_or(0),
                paths::pretty(stdlib)
            );

            match row["stdlib"]["rbs"]["version"].as_str() {
                Some(version) => println!(
                    "{:>32}+ signatures, rbs {version} ({}): {}",
                    "",
                    row["stdlib"]["rbs"]["chosen"].as_str().unwrap_or_default(),
                    paths::pretty(row["stdlib"]["rbs"]["path"].as_str().unwrap_or_default())
                ),
                None => println!("{:>32}+ no signatures: its Ruby carries no rbs gem", ""),
            }
        }
        if let Some(version) = row["ruby_not_found"].as_str() {
            let instead = match row["stdlib"]["root"].as_str() {
                Some(_) => "the stdlib above is another Ruby's",
                None => "no Ruby, so nothing is known of core",
            };
            println!(
                "{:>32}! the checkout names Ruby {version}, which is not installed: {instead}",
                ""
            );
        }
        if let Some(requirements) = row["ruby_unmet"].as_str() {
            println!(
                "{:>32}! no installed Ruby meets {requirements}: the stdlib above is outside it",
                ""
            );
        }
        let gems = &row["gems"];
        let (Some(count), Some(indexed)) = (gems["count"].as_u64(), gems["indexed"].as_u64())
        else {
            continue;
        };
        let state = match (count, indexed) {
            (0, _) => None,
            (n, i) if n == i => Some("all indexed".to_string()),
            (_, i) => Some(format!("{i} of {count} indexed")),
        };
        if let Some(state) = state {
            println!(
                "{:>32}+ {count} gem{}, {state} ({} files)",
                "",
                if count == 1 { "" } else { "s" },
                gems["files"].as_i64().unwrap_or(0)
            );
        }
        let names = |list: &serde_json::Value, key: &str| -> Vec<String> {
            list.as_array()
                .into_iter()
                .flatten()
                .filter_map(|item| match key {
                    "" => item.as_str(),
                    key => item[key].as_str(),
                })
                .map(str::to_string)
                .collect()
        };
        let missing = names(&gems["missing"], "");
        if !missing.is_empty() {
            println!(
                "{:>32}! {} named but not installed: {}",
                "",
                missing.len(),
                index::abridged(&missing)
            );
        }
        // One line per reason, as `--index` says them.
        let whys = names(&gems["unlocated"], "why");
        let gems_of = names(&gems["unlocated"], "gem");
        let mut reasons: Vec<&String> = Vec::new();
        for why in &whys {
            if !reasons.contains(&why) {
                reasons.push(why);
            }
        }
        for why in reasons {
            let named: Vec<String> = whys
                .iter()
                .zip(&gems_of)
                .filter(|(w, _)| *w == why)
                .map(|(_, gem)| gem.clone())
                .collect();
            println!(
                "{:>32}! {} — {why}: {}",
                "",
                named.len(),
                index::abridged(&named)
            );
        }
    }
    let (repos, gems) = (
        others["repos"].as_u64().unwrap_or(0),
        others["gems"].as_u64().unwrap_or(0),
    );
    if repos + gems > 0 {
        let plural = |n: u64, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
        println!(
            "\nalso indexed: {}, {} — `trekr --status --all` lists them",
            plural(repos, "other repo"),
            plural(gems, "gem")
        );
    }
    println!(
        "\nshared: {} blobs, {} defs, {} const refs, {} calls",
        totals.blobs, totals.defs, totals.const_refs, totals.calls
    );
    println!("{}", storage_line(&storage(&store, retention)?));
    print_kept(&kept);
    Ok(ExitCode::SUCCESS)
}

/// The store's size on disk, what it could give back, and when the
/// background cleanup last did (DEC-650).
/// The retention `TREKR_RETENTION` gives, else the default; a value it
/// cannot read is a usage error.
fn configured_retention() -> anyhow::Result<Retention> {
    Retention::configured().map_err(|why| Failure::Usage.error(why))
}

fn storage(store: &Store, retention: Retention) -> anyhow::Result<serde_json::Value> {
    // The store first, in one snapshot, and the snapshot files after: a
    // cleanup records its run last, so a run read here has already swept.
    let storage = store.storage()?;
    let retained = store.retained(retention.before(crate::cleanup::now()))?;
    let snapshots = crate::tree::sweep_snapshots(store, &[], crate::tree::Overlays::Keep, true)?;
    Ok(serde_json::json!({
        "bytes": storage.bytes,
        "free_bytes": storage.free_bytes,
        "stale_snapshots": snapshots,
        "reclaimable_bytes": storage.free_bytes + snapshots.bytes as i64,
        "cleanup": storage.cleanup,
        "retained": {
            "blobs": retained.blobs,
            "expired": retained.expired,
            "retention": retention.seconds(),
        },
    }))
}

fn storage_line(storage: &serde_json::Value) -> String {
    let mb = |value: &serde_json::Value| value.as_i64().unwrap_or(0) as f64 / 1e6;
    let mut line = format!("index: {:.1} MB", mb(&storage["bytes"]));
    let reclaimable = storage["reclaimable_bytes"].as_i64().unwrap_or(0);
    if reclaimable > 0 {
        let snapshots = storage["stale_snapshots"]["files"].as_u64().unwrap_or(0);
        line.push_str(&format!(
            ", {:.1} MB reclaimable ({:.1} MB free inside it, {snapshots} stale tree snapshot{})",
            mb(&storage["reclaimable_bytes"]),
            mb(&storage["free_bytes"]),
            if snapshots == 1 { "" } else { "s" }
        ));
    }
    let retained = &storage["retained"];
    let versions = retained["blobs"].as_u64().unwrap_or(0);
    if versions > 0 {
        line.push_str(&format!(
            "; {versions} replaced version{} kept {}",
            if versions == 1 { "" } else { "s" },
            match retained["retention"].as_u64() {
                Some(seconds) => format!("for {}", age_words(seconds)),
                None => "forever".to_string(),
            }
        ));
    }
    match storage["cleanup"]["skipped"].as_str() {
        Some("no_room") => line.push_str(
            "; the last cleanup left it: no room on disk to rewrite the index (it needs twice \
             the live size free)",
        ),
        Some("busy") => {
            line.push_str("; the last cleanup left it: another trekr held the index, so it retries")
        }
        _ => {}
    }
    if let Some(at) = storage["cleanup"]["ran_at"].as_i64() {
        let ago = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64)
            - at;
        line.push_str(&format!(
            "; cleaned up in the background {} ago, reclaiming {:.1} MB",
            ago_words(ago),
            mb(&storage["cleanup"]["reclaimed_bytes"])
        ));
    }
    line
}

/// A retention as it would be written: `14d`, `36h`, `90m`.
fn age_words(seconds: u64) -> String {
    [(86_400, "d"), (3600, "h"), (60, "m")]
        .into_iter()
        .find(|(unit, _)| seconds >= *unit && seconds.is_multiple_of(*unit))
        .map_or(format!("{seconds}s"), |(unit, name)| {
            format!("{}{name}", seconds / unit)
        })
}

/// `40 s`, `12 min`, `3 h`, `5 d`: as precise as a glance needs.
fn ago_words(seconds: i64) -> String {
    match seconds.max(0) {
        s if s < 60 => format!("{s} s"),
        s if s < 3600 => format!("{} min", s / 60),
        s if s < 86_400 => format!("{} h", s / 3600),
        s => format!("{} d", s / 86_400),
    }
}

/// The files kept beside the store, one line each.
fn print_kept(kept: &[crate::store::Kept]) {
    for k in kept {
        let what = match (k.kind, k.version, k.in_use) {
            (_, _, true) => {
                "this trekr's own store: the default one is a newer trekr's".to_string()
            }
            ("side", Some(v), _) => format!("store v{v}, another trekr's own"),
            ("early", _, _) => "an early store an index left when it stopped".to_string(),
            _ => "set aside when it couldn't be used".to_string(),
        };
        println!(
            "kept: {}  {what}, {:.1} MB, idle {}d{}",
            paths::pretty(&k.path),
            k.bytes as f64 / 1e6,
            k.idle / 86_400,
            if k.in_use {
                ""
            } else {
                " (`trekr --gc` removes it)"
            }
        );
    }
}

/// The Ruby a checkout's last index chose, and how — asked of this
/// environment, since the store keeps only which: `how` is `null` when a
/// reindex from here would choose another (DEC-292).
fn status_ruby(repo: &str, stdlib: Option<&str>) -> Option<crate::gems::stdlib::About> {
    let root = Path::new(stdlib?);
    let now = crate::gems::stdlib::for_checkout(Path::new(repo), Some(root));
    let how = now.filter(|now| now.root == root).map(|now| now.how);
    Some(crate::gems::stdlib::about(root, how))
}

/// `--status`' line for a checkout's Ruby: `Ruby 3.4.10, which the checkout
/// names`, or `Ruby 4.0.6 (fallback: …)`. `unmet`: it is outside the
/// checkout's requirements, which a line of its own says.
fn ruby_said(ruby: &crate::gems::stdlib::About, unmet: bool) -> String {
    let name = match &ruby.version {
        Some(version) => format!("Ruby {version}"),
        None => format!("the Ruby at {}", paths::pretty(&ruby.root)),
    };
    match ruby.how {
        Some(crate::gems::stdlib::How::Named) => format!("{name}, which the checkout names"),
        Some(crate::gems::stdlib::How::Highest) if unmet => {
            format!("{name} (fallback: the highest installed Ruby)")
        }
        Some(how) => format!("{name} (fallback: {})", how.said()),
        None => format!("{name}, which a reindex from here would replace"),
    }
}

/// The checkout `--status` reports on from `dir`: its git repository, as a
/// query's, else the indexed gem it is in — the gem itself, not the app a
/// query would answer a gem's position from.
fn status_checkout(store: &Store, dir: &Path) -> anyhow::Result<PathBuf> {
    match scan::repo_root(dir) {
        Ok(root) => Ok(root),
        Err(error) => gem_holding(store, dir).map(PathBuf::from).ok_or(error),
    }
}

/// `--status` from a checkout nobody indexed: the same `not_indexed` a query
/// from it answers, with the store's other contents summarized beside it, so
/// no other checkout's row can be read as this one's.
fn status_not_indexed(
    out: Output,
    root: &Path,
    store: &Store,
    checkouts: &[crate::store::Checkout],
    totals: &crate::store::Totals,
    retention: Retention,
) -> anyhow::Result<ExitCode> {
    crate::usage::outcome(Outcome::NotIndexed);
    let root = root.to_string_lossy().into_owned();
    let hint = format!("trekr --index {}", paths::pretty(&root));
    let (reason, upgraded) = not_indexed_reason(store)?;
    let count = |gem: bool| {
        checkouts
            .iter()
            .filter(|c| (c.kind != "repo") == gem)
            .count()
    };
    let (repos, gems) = (count(false), count(true));
    if out != Output::Text {
        emit_json(
            out,
            &serde_json::json!({
                "status": "not_indexed",
                "repo": root,
                "reason": reason,
                "hint": hint,
                "checkouts": [],
                "others": { "repos": repos, "gems": gems },
                "totals": totals,
                "storage": storage(store, retention)?,
            }),
        )?;
        return Ok(ExitCode::from(2));
    }
    match upgraded {
        true => println!(
            "{} is not indexed — {reason}. The next query indexes it, or run: {hint}",
            paths::pretty(&root)
        ),
        false => println!(
            "{} is not indexed — the next query indexes it, or run: {hint}",
            paths::pretty(&root)
        ),
    }
    if repos + gems > 0 {
        let plural = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
        println!(
            "\nalso indexed: {}, {} — `trekr --status --all` lists them",
            plural(repos, "other repo"),
            plural(gems, "gem")
        );
    }
    println!("\n{}", storage_line(&storage(store, retention)?));
    Ok(ExitCode::from(2))
}

pub(super) fn cmd_gc(out: Output, older_than: u64, dry_run: bool) -> anyhow::Result<ExitCode> {
    let retention = configured_retention()?;
    let mut store = open_store()?;
    store.wait_as_writer(writer_waiting)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs() as i64;
    // An index the schema gained since this store was laid down, before the
    // deletes that cascade through it — even on a dry run, which is the real
    // collection rolled back: it changes no answer, and without it each blob
    // collected scanned a table (DEC-650).
    let indexes = store.add_missing_indexes()?;
    // What a store left unmapped before the stamps, as of now (DEC-651):
    // like the index, it removes nothing, so a dry run does it too.
    store.stamp_unmapped()?;
    let garbage = store.collect(
        now - older_than as i64,
        retention.before(now),
        |root| Path::new(root).is_dir(),
        dry_run,
    )?;
    let gone: Vec<&str> = garbage.checkouts.iter().map(|c| c.repo.as_str()).collect();
    let snapshots =
        crate::tree::sweep_snapshots(&store, &gone, crate::tree::Overlays::Sweep, dry_run)?;
    // A query's copies of the map with its edits applied (DEC-035).
    let (overlays, overlay_bytes) = store.sweep_overlays(dry_run);
    // Each Ruby's core files beside the store, and an earlier build's.
    let db = crate::store::default_path()?;
    let beside_store = crate::store::core_dir_of(&crate::store::in_use(&db));
    let mut core = crate::tree::sweep_core(&beside_store, &garbage.core_dirs, dry_run);
    if let Some(beside) = db.parent() {
        let legacy = crate::tree::sweep_legacy_core(beside, dry_run);
        core.files += legacy.files;
        core.bytes += legacy.bytes;
    }
    // Another trekr's store once idle as long as a checkout would be; a
    // set-aside copy is only evidence, and a dead index's early store is
    // read by nothing, so any age.
    let kept: Vec<crate::store::Kept> = crate::store::kept(&db, crate::store::VERSION)
        .into_iter()
        .filter(|k| !k.in_use && (k.kind != "side" || k.idle >= older_than))
        .collect();
    if !dry_run {
        kept.iter().for_each(crate::store::remove_kept);
    }
    // What this collection freed inside the index is handed back now, once
    // it is worth it, not an interval later; less is left for later writes
    // or the background cleanup (DEC-650, DEC-878).
    let before = store.db_bytes()?;
    if !dry_run {
        crate::cleanup::compact_after_gc(&store, garbage.freed_bytes)?;
    }
    let db_bytes = store.db_bytes()?;
    let free_bytes = store.free_bytes()?;
    // What left the disk: the files, and the index file's shrink.
    let reclaimed_bytes =
        (snapshots.bytes + overlay_bytes + core.bytes + kept.iter().map(|k| k.bytes).sum::<u64>())
            as i64
            + (before - db_bytes).max(0);
    let found = !garbage.checkouts.is_empty()
        || garbage.blobs > 0
        || snapshots.files > 0
        || overlays > 0
        || garbage.signatures > 0
        || core.files > 0
        || !kept.is_empty();

    if out != Output::Text {
        emit_json(
            out,
            &serde_json::json!({
                "dry_run": dry_run,
                "older_than": older_than,
                "retention": retention.seconds(),
                "indexes": indexes,
                "checkouts": garbage.checkouts,
                "files": garbage.files,
                "blobs": garbage.blobs,
                "expired": garbage.expired,
                "facts": garbage.facts,
                "freed_bytes": garbage.freed_bytes,
                "free_bytes": free_bytes,
                "reclaimed_bytes": reclaimed_bytes,
                "snapshots": snapshots,
                "overlays": { "files": overlays, "bytes": overlay_bytes },
                "signatures": garbage.signatures,
                "core_files": core,
                "kept": kept,
                "db_bytes": db_bytes,
            }),
        )?;
        return Ok(exit_on(found));
    }
    let mb = |bytes: i64| bytes as f64 / 1e6;
    let verb = if dry_run {
        "would collect"
    } else {
        "collected"
    };
    for index in &indexes {
        println!(
            "added the index {index}, which this version's schema has and this store lacked \
             (no data removed)"
        );
    }
    if !found {
        println!("nothing to collect");
    } else if !garbage.checkouts.is_empty() {
        for c in &garbage.checkouts {
            let days = (now - c.last_seen) / 86_400;
            println!(
                "{:<4} {:<9} {:>4}d  {}",
                c.kind,
                c.reason,
                days,
                paths::pretty(&c.repo)
            );
        }
        println!(
            "\n{verb} {} checkouts: {} files, {} blobs, {} facts, {:.1} MB inside the index",
            garbage.checkouts.len(),
            garbage.files,
            garbage.blobs,
            garbage.facts,
            mb(garbage.freed_bytes)
        );
    } else if garbage.blobs > 0 {
        println!(
            "{verb} {} versions of files nothing has mapped for {}: {} facts, {:.1} MB inside \
             the index",
            garbage.blobs,
            age_words(retention.seconds().unwrap_or(0)),
            garbage.facts,
            mb(garbage.freed_bytes)
        );
    }
    if snapshots.files > 0 {
        println!(
            "{verb} {} tree snapshots no checkout's index names any more: {:.1} MB",
            snapshots.files,
            mb(snapshots.bytes as i64)
        );
    }
    if overlays > 0 {
        println!(
            "{verb} {overlays} copies of the edits queries read, in trekr.overlays/: {:.1} MB",
            mb(overlay_bytes as i64)
        );
    }
    if garbage.signatures > 0 || core.files > 0 {
        println!(
            "{verb} {} Ruby signature sets no stdlib is served with, and {} core files beside \
             the store: {:.1} MB of files",
            garbage.signatures,
            core.files,
            mb(core.bytes as i64)
        );
    }
    for k in &kept {
        println!(
            "{verb} {}: {:.1} MB",
            paths::pretty(&k.path),
            mb(k.bytes as i64)
        );
    }
    if found {
        let verb = if dry_run {
            "would reclaim"
        } else {
            "reclaimed"
        };
        println!("{verb} {:.1} MB on disk", mb(reclaimed_bytes));
    }
    // Rewritten in the background once it is worth it (DEC-650).
    if free_bytes > 0 {
        println!(
            "{:.1} MB is free inside the index, for later writes or a background compaction",
            mb(free_bytes)
        );
    }
    Ok(exit_on(found))
}
