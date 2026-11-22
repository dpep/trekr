//! `--index`: parse what is new in a checkout, its bundle and its Ruby, and
//! record the map.

use super::*;

/// Worker threads for the parse phase.
///
/// **Physical** cores, not logical. Parsing is compute-bound and gains little
/// from hyperthreads; the measurement behind this is in DECISIONS.
fn worker_count(requested: usize) -> usize {
    if requested > 0 {
        return requested;
    }
    num_cpus::get_physical().max(1)
}

/// Parse whatever is new in `files` and record the map under `root`.
///
/// Shared by a checkout and a gem: the two differ only in how their file list
/// was produced, which is the whole point of `scan` owning that question.
#[allow(clippy::too_many_arguments)]
fn index_files(
    store: &mut Store,
    root: &Path,
    files: &scan::Files,
    git_state: i64,
    // Some of the checkout's files, added to its map (`Store::write_part`).
    part: bool,
    known: &mut Option<Known>,
    pool: &rayon::ThreadPool,
    profile: &mut Option<profile::Profile>,
) -> anyhow::Result<crate::store::Indexed> {
    // Under the write lock from what is known to the write: a collection
    // that deletes a blob between them would leave the write a blob it
    // neither has nor parsed (DEC-651).
    let standalone = store.autocommit();
    let (counts, received, parse_done) = if standalone {
        store.batch(|store| {
            write_files(
                store, root, files, git_state, part, true, known, pool, profile,
            )
        })?
    } else {
        write_files(
            store, root, files, git_state, part, false, known, pool, profile,
        )?
    };
    received.finish(store, profile, known, parse_done);
    Ok(counts)
}

/// Every blob the store holds, as of when it was read, for an index to skip
/// parsing what it has.
struct Known {
    oids: HashSet<Oid>,
    /// `PRAGMA data_version` then: another process has committed since —
    /// perhaps a collection — when it moves.
    version: i64,
}

/// What the store holds now, for a caller holding the write lock: loaded
/// whole once (`Store::blob_oids`), and again only if another process
/// committed since — a collection may have deleted what it held.
fn known_now<'a>(
    store: &Store,
    known: &'a mut Option<Known>,
    profile: &mut Option<profile::Profile>,
) -> anyhow::Result<&'a Known> {
    let version = store.data_version()?;
    if known.as_ref().is_none_or(|k| k.version != version) {
        let oids = profile::timed(profile, "known-diff", || store.blob_oids())?;
        *known = Some(Known { oids, version });
    }
    Ok(known.as_ref().expect("just loaded"))
}

/// `index_files`, holding the write lock.
#[allow(clippy::too_many_arguments)]
fn write_files(
    store: &mut Store,
    root: &Path,
    files: &scan::Files,
    git_state: i64,
    part: bool,
    standalone: bool,
    known: &mut Option<Known>,
    pool: &rayon::ThreadPool,
    profile: &mut Option<profile::Profile>,
) -> anyhow::Result<(crate::store::Indexed, Received, std::time::Instant)> {
    let wanted: HashSet<&Oid> = files.values().collect();

    // One path per unknown blob: identical content under two names is one
    // parse, and which name it was read from cannot matter. A map identical
    // to the stored one has nothing unknown, so the known set — every blob on
    // the machine — is loaded only when there may be.
    let mut to_parse: HashMap<&Oid, PathBuf> = HashMap::new();
    if part || !store.map_unchanged(&root.to_string_lossy(), files)? {
        let known = known_now(store, known, profile)?;
        for (rel, oid) in files {
            if !known.oids.contains(oid) {
                to_parse.entry(oid).or_insert_with(|| root.join(rel));
            }
        }
    }
    if let Some(profile) = profile.as_mut() {
        profile.blobs += wanted.len();
        profile.parsed += to_parse.len();
        profile.skipped += wanted.len() - to_parse.len();
    }

    // Never inside a batch: a gem's rows are few, and the bundle's
    // transaction is shared.
    let bulk = !part
        && standalone
        && known
            .as_ref()
            .is_some_and(|k| bulk_load(to_parse.len(), k.oids.len()));
    // Parsing fans out on the pool while this thread writes what has already
    // been parsed: the write is single-threaded and most of the cost, so the
    // parse hides behind it instead of running before it.
    let mut received = Received::new(profile.is_some());
    let (send, parsed) = std::sync::mpsc::sync_channel::<(Oid, extract::Parsed)>(256);
    let (counts, parse_done) = std::thread::scope(|scope| {
        let parsing = scope.spawn(move || {
            pool.install(|| {
                to_parse
                    .into_par_iter()
                    .for_each_with(send, |send, (oid, path)| {
                        if let Some(parsed) = parse_file(&path) {
                            let _ = send.send((oid.clone(), parsed));
                        }
                    })
            });
            std::time::Instant::now()
        });
        let facts = parsed
            .into_iter()
            .map(|(oid, parsed)| received.take(oid, parsed));
        let root = root.to_string_lossy();
        let counts = if part {
            store.write_part(&root, files, facts)
        } else if bulk {
            store.write_bulk(&root, files, facts, git_state)
        } else {
            store.write(&root, files, facts, git_state)
        };
        (counts, parsing.join().expect("the parse does not panic"))
    });
    Ok((counts?, received, parse_done))
}

/// Write the checkout and its gems: the part of an index that holds the
/// write lock, and so the part another writer can make it outwait.
fn index_all(
    store: &mut Store,
    root: &Path,
    files: &scan::Files,
    git_state: i64,
    with_gems: bool,
    jobs: usize,
    profile: &mut Option<profile::Profile>,
) -> anyhow::Result<(crate::store::Indexed, GemReport)> {
    let root_str = root.to_string_lossy().into_owned();
    store.wait_as_writer(writer_waiting)?;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(jobs).build()?;
    // What the language server or a query wants read first: this index's if
    // it fills the checkout, the one it waits for's if another does.
    let mut hints = crate::background::Hints::listen(autoindex::spawned_by_trekr());
    // The query that started this index claimed the checkout for it.
    let parent = autoindex::spawned().then(std::os::unix::process::parent_id);
    // A first index — no map yet, or one an index left unfinished — is
    // marked while it fills the store, so an answer meanwhile says it is
    // partial (DEC-320), and is written in the order someone looking at it
    // needs it (DEC-322). A reindex replaces a whole map with a whole map.
    // A first index claims the checkout as it marks it: one already filling
    // it is waited for, not run a second time (DEC-500).
    let filling = loop {
        if store.has_checkout(&root_str)? && store.warming(&root_str)?.is_none() {
            break false;
        }
        // Seen without the write lock first: another's bulk write holds it
        // for seconds, and this index has hints to hand that one meanwhile.
        let running = store.warming(&root_str)?.filter(|other| {
            !other.interrupted && other.pid != std::process::id() && Some(other.pid) != parent
        });
        let other = match running {
            Some(other) => Some(other),
            None => store.claim_warming(&root_str, files.len() as u64, !with_gems, parent)?,
        };
        match other {
            None => {
                die_after_claim_for_tests()?;
                if let Some(main) = store.path() {
                    hints.tail(crate::store::early::hints(main, std::process::id()));
                    // Handed to the claim its query made for it.
                    if let Some(parent) = parent {
                        hints.tail(crate::store::early::hints(main, parent));
                    }
                }
                break true;
            }
            Some(other) => {
                wait_for_index(store, &root_str, other, &hints)?;
                // A query started this index, and the one it waited for
                // has done what it was asked to (DEC-500).
                if autoindex::spawned()
                    && store.has_checkout(&root_str)?
                    && store.warming(&root_str)?.is_none()
                {
                    return Ok(Default::default());
                }
            }
        }
    };
    let mut known = None;
    let (counts, gems) = if filling {
        let indexed = index_first(
            store, root, files, git_state, with_gems, &hints, &mut known, &pool, profile,
        )?;
        if let (Some(main), Some(parent)) = (store.path(), parent) {
            let _ = std::fs::remove_file(crate::store::early::hints(main, parent));
        }
        indexed
    } else {
        let counts = index_files(
            store, root, files, git_state, false, &mut known, &pool, profile,
        )?;
        let plan = match with_gems {
            true => Some(plan_gems(store, root, &pool, profile)?),
            false => None,
        };
        let gems = match plan {
            Some(plan) => index_gems(store, root, plan, &mut known, &pool, profile)?,
            None => GemReport::default(),
        };
        (counts, gems)
    };

    // Only when something was actually read, and then only once the store has
    // outgrown its statistics: a full ANALYZE costs seconds whatever changed.
    if counts.parsed > 0 || gems.indexed > 0 {
        profile::timed(profile, "analyze", || {
            store.analyze_if_outgrown();
            Ok::<(), anyhow::Error>(())
        })?;
    }

    Ok((counts, gems))
}

/// `TREKR_TEST_AFTER_CLAIM`: an index that dies holding its claim, as a
/// `kill -9` (`kill`) or a full disk (`fail`) leaves one.
fn die_after_claim_for_tests() -> anyhow::Result<()> {
    match std::env::var("TREKR_TEST_AFTER_CLAIM").as_deref() {
        // SAFETY: delivers SIGKILL to this process; nothing runs after it.
        Ok("kill") => unsafe {
            libc::kill(libc::getpid(), libc::SIGKILL);
        },
        Ok("fail") => {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_IOERR),
                Some("disk I/O error".into()),
            )
            .into());
        }
        _ => {}
    }
    Ok(())
}

/// Another process's first index of the checkout outlasted the writer wait.
#[derive(Debug)]
struct ClaimOutwaited;

impl std::fmt::Display for ClaimOutwaited {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("another trekr's index of this checkout outlasted the writer wait")
    }
}

impl std::error::Error for ClaimOutwaited {}

/// `TREKR_TEST_STALL_MS`: a first index that pauses once the asked file's
/// part is in, as a large checkout's does reading the rest;
/// `TREKR_TEST_STALL_BULK_MS`: and once the rest's write holds the store.
/// `TREKR_TEST_STALL_TREE_MS`: an index whose last rows are in, before it
/// prepares the tree.
fn stall_for_tests(var: &str) {
    if let Some(ms) = std::env::var(var).ok().and_then(|ms| ms.parse().ok()) {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
}

/// Wait for another process's first index of `root` to end — or to die,
/// leaving its mark for this one to take over — saying so as a writer
/// queued for the lock does (DEC-171), and for no longer than one waits.
fn wait_for_index(
    store: &Store,
    root: &str,
    other: crate::store::Warming,
    hints: &crate::background::Hints,
) -> anyhow::Result<()> {
    use std::io::IsTerminal;
    let started = std::time::Instant::now();
    let every = if std::io::stderr().is_terminal() {
        10
    } else {
        60
    };
    let (mut due, mut warming) = (1, other);
    loop {
        let waited = started.elapsed();
        if waited >= crate::store::writer_wait() {
            // As a lock outwaited: `--index` reports it incomplete, exit 2.
            return Err(ClaimOutwaited.into());
        }
        if waited.as_secs() >= due {
            match due {
                1 => eprintln!(
                    "trekr: another trekr is already indexing {} (pid {}, {}); \
                     waiting for it to finish",
                    paths::pretty(root),
                    warming.pid,
                    warming.how_far()
                ),
                secs => eprintln!("trekr: still waiting for the other index ({secs}s)"),
            }
            due = waited.as_secs() - waited.as_secs() % every + every;
        }
        // What this index was asked to read first, the other reads (DEC-512).
        if let Some(main) = store.path() {
            let _ = crate::store::early::hint(main, warming.pid, &hints.take_sent());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        match store.warming(root)? {
            Some(now) if !now.interrupted && now.pid == warming.pid => warming = now,
            _ => return Ok(()),
        }
    }
}

/// Whether a load of `new` blobs into a store of `known` rebuilds the fact
/// indexes by sorting rather than inserting into them (DEC-057): from half
/// the store up, where the rebuild costs less than the inserts (DEC-234).
fn bulk_load(new: usize, known: usize) -> bool {
    new > 0 && new * 2 >= known
}

/// A file read and parsed for the writer, timed for `--profile`.
fn parse_file(path: &Path) -> Option<extract::Parsed> {
    let started = std::time::Instant::now();
    let bytes = scan::read_source(path).ok()?;
    let facts = extract::extract_file(&path.to_string_lossy(), &bytes);
    Some(extract::Parsed {
        facts,
        bytes: bytes.len() as u64,
        elapsed: started.elapsed(),
        path: path.to_string_lossy().into_owned(),
    })
}

/// What a write took from its parse, for the profile and the known set.
struct Received {
    started: std::time::Instant,
    profiling: bool,
    slow: Vec<profile::SlowFile>,
    bytes: u64,
    fresh: Vec<Oid>,
}

impl Received {
    fn new(profiling: bool) -> Received {
        Received {
            started: std::time::Instant::now(),
            profiling,
            slow: Vec::new(),
            bytes: 0,
            fresh: Vec::new(),
        }
    }

    fn take(&mut self, oid: Oid, parsed: extract::Parsed) -> (Oid, crate::core::Facts) {
        self.bytes += parsed.bytes;
        if self.profiling {
            self.slow.push(profile::SlowFile {
                path: parsed.path,
                ms: parsed.elapsed.as_secs_f64() * 1000.0,
                bytes: parsed.bytes,
            });
        }
        self.fresh.push(oid.clone());
        (oid, parsed.facts)
    }

    /// Once the write is in: its phases named, and what it parsed known.
    fn finish(
        self,
        store: &mut Store,
        profile: &mut Option<profile::Profile>,
        known: &mut Option<Known>,
        parse_done: std::time::Instant,
    ) {
        let timing = store.take_timing();
        if let Some(profile) = profile.as_mut() {
            // The two overlap: `parse` runs until the last file is parsed, and
            // `store-write` is what the write took after that, less the parts
            // that follow the rows, which are named on their own.
            let after = timing.rebuild + timing.map + timing.commit;
            profile.phase("parse", parse_done - self.started);
            profile.phase("store-write", parse_done.elapsed().saturating_sub(after));
            profile.phase("index-rebuild", timing.rebuild);
            profile.phase("file-map", timing.map);
            profile.phase("commit", timing.commit);
            profile.bytes += self.bytes;
            profile.merge_files(self.slow);
        }
        // Written now, so a later gem holding the same bytes does not parse them.
        if let Some(known) = known.as_mut() {
            known.oids.extend(self.fresh);
        }
    }
}

/// At most this many files go ahead of the rest for the files someone has
/// open: enough for what a file's constants name, few enough to parse at once.
const NEAR: usize = 256;

/// A checkout's first index, in the order someone looking at it needs it
/// (DEC-322), each step its own commit: the files the language server says
/// are open and those their constants most likely live in; the Ruby's stdlib
/// and the gems those files name, with the bundle recorded so gems already
/// on this machine answer too; whatever was opened meanwhile; the rest of
/// the checkout; the rest of the gems. Marked as filling until the last
/// commit (DEC-320). The checkout's rest is one whole write, so the store
/// ends as one write would have left it.
#[allow(clippy::too_many_arguments)]
fn index_first(
    store: &mut Store,
    root: &Path,
    files: &scan::Files,
    git_state: i64,
    with_gems: bool,
    hints: &crate::background::Hints,
    known: &mut Option<Known>,
    pool: &rayon::ThreadPool,
    profile: &mut Option<profile::Profile>,
) -> anyhow::Result<(crate::store::Indexed, GemReport)> {
    let root_str = root.to_string_lossy().into_owned();
    if let Some(main) = store.path() {
        crate::store::early::sweep(main);
    }
    // Marked already, as the claim (`index_all`); with gems, the tree's
    // count waits for their listing (DEC-400 addendum).
    let mut written: HashSet<String> = HashSet::new();
    let (first, asked) = index_wanted(
        store,
        root,
        files,
        hints,
        &mut written,
        known,
        pool,
        profile,
    )?;
    let plan = match with_gems {
        true => Some(plan_gems(store, root, pool, profile)?),
        false => None,
    };
    let (mut counts, gems) = match plan {
        None => {
            let counts = index_files(store, root, files, git_state, false, known, pool, profile)?;
            (counts, GemReport::default())
        }
        Some(plan) => {
            let of = files.len() as u64 + plan.files();
            let GemPlan {
                mut report,
                stdlib,
                stdlib_files,
                used,
                fresh,
                known_files,
            } = plan;
            let named = scan::near::gems_named(&fresh, &asked);
            let (ahead, rest): (Vec<_>, Vec<_>) = fresh
                .into_iter()
                .enumerate()
                .partition(|(at, _)| named.contains(at));
            let count = |gems: &[(usize, (PathBuf, Vec<String>))]| -> u64 {
                gems.iter().map(|(_, (_, f))| f.len() as u64).sum()
            };
            let theirs = known_files + stdlib_files.as_ref().map_or(0, |f| f.len() as u64);
            let theirs = theirs + count(&ahead);
            let ruby = stdlib.as_ref().map(|stdlib| (stdlib, stdlib_files));
            // Only these gems are read now; the rest wait for their commit.
            let ahead = hash_gems(
                ahead.into_iter().map(|(_, gem)| gem).collect(),
                pool,
                profile,
            );
            let read = written.len() as u64 + theirs;
            gem_batch(
                store,
                root,
                &mut report,
                ruby,
                ahead,
                &used,
                known,
                pool,
                profile,
                |s, _| Ok(s.set_warming(&root_str, read, of)?),
            )?;
            // The Ruby's signatures, a commit of their own: a gem's answers
            // need none of them (DEC-330).
            if let Some(stdlib) = &stdlib {
                let rbs = store.batch(|s| signatures(s, stdlib, profile))?;
                if let Some(report) = report.stdlib.as_mut() {
                    report.rbs = rbs;
                }
            }
            stall_for_tests("TREKR_TEST_STALL_MS");
            // Opened while those were read: still ahead of the rest.
            let (more, _) = index_wanted(
                store,
                root,
                files,
                hints,
                &mut written,
                known,
                pool,
                profile,
            )?;
            let read = written.len() as u64 + theirs;
            if more.files > 0 {
                store.set_warming(&root_str, read, of)?;
            }
            // Opened while the rest is written: to an early store (DEC-332).
            let main = store
                .path()
                .filter(|_| hints.listening)
                .map(Path::to_path_buf);
            let stop = std::sync::atomic::AtomicBool::new(false);
            let mut counts = std::thread::scope(|scope| {
                let early = main.as_deref().map(|main| {
                    let (written, stop) = (&written, &stop);
                    scope.spawn(move || {
                        write_early(main, root, files, hints, written, (read, of), stop)
                    })
                });
                let counts = {
                    let _stop = Stop(&stop);
                    stall_for_tests("TREKR_TEST_STALL_BULK_MS");
                    index_files(store, root, files, git_state, false, known, pool, profile)
                };
                if let Some(early) = early {
                    let _ = early.join();
                }
                counts
            })?;
            add_parsed(&mut counts, &more);
            store.set_warming(&root_str, files.len() as u64 + theirs, of)?;
            let rest = hash_gems(
                rest.into_iter().map(|(_, gem)| gem).collect(),
                pool,
                profile,
            );
            gem_batch(
                store,
                root,
                &mut report,
                None,
                rest,
                &used,
                known,
                pool,
                profile,
                |s, _| Ok(s.clear_warming(&root_str)?),
            )?;
            (counts, report)
        }
    };
    store.clear_warming(&root_str)?;
    if let Some(main) = store.path() {
        let _ = std::fs::remove_file(crate::store::early::hints(main, std::process::id()));
    }
    add_parsed(&mut counts, &first);
    Ok((counts, gems))
}

/// `part`'s parsing, added to a whole write's report of it.
fn add_parsed(counts: &mut crate::store::Indexed, part: &crate::store::Indexed) {
    counts.parsed += part.parsed;
    counts.defs += part.defs;
    counts.refs += part.refs;
    counts.calls += part.calls;
}

/// Write the files the language server asked for since the last call, and
/// the files they most likely need, as part of the checkout's map; and hand
/// back what the asked-for files say, for the gems they name. The first call
/// writes even nothing, so the checkout exists for its gems to belong to.
#[allow(clippy::too_many_arguments)]
fn index_wanted(
    store: &mut Store,
    root: &Path,
    files: &scan::Files,
    hints: &crate::background::Hints,
    written: &mut HashSet<String>,
    known: &mut Option<Known>,
    pool: &rayon::ThreadPool,
    profile: &mut Option<profile::Profile>,
) -> anyhow::Result<(crate::store::Indexed, Vec<(String, crate::core::Facts)>)> {
    let (asked, part) = wanted(root, files, hints, written);
    if part.is_empty() && store.has_checkout(&root.to_string_lossy())? {
        return Ok((crate::store::Indexed::default(), asked));
    }
    let counts = index_files(store, root, &part, 0, true, known, pool, profile)?;
    written.extend(part.into_keys());
    Ok((counts, asked))
}

/// The files the language server asked for since the last call, with what
/// each says, and those and the files they most likely need not yet written.
/// A file written already, as another's neighbour, still brings its own.
fn wanted(
    root: &Path,
    files: &scan::Files,
    hints: &crate::background::Hints,
    written: &HashSet<String>,
) -> (Vec<(String, crate::core::Facts)>, scan::Files) {
    let asked: Vec<(String, crate::core::Facts)> = hints
        .take(root)
        .into_iter()
        .filter(|path| files.contains_key(path))
        .filter_map(|path| extract::read_file(root, &path).map(|facts| (path, facts)))
        .collect();
    // Polled while the bulk write runs: nothing asked must cost nothing.
    if asked.is_empty() {
        return (asked, scan::Files::new());
    }
    let near = scan::near::nearby(files, &asked, NEAR);
    let part: scan::Files = asked
        .iter()
        .map(|(path, _)| path)
        .chain(&near)
        .filter(|path| !written.contains(*path))
        .filter_map(|path| Some((path.clone(), files.get(path)?.clone())))
        .collect();
    (asked, part)
}

/// Sets its flag when dropped — unwinding too, or a scope would wait forever
/// for the thread watching it.
struct Stop<'a>(&'a std::sync::atomic::AtomicBool);

impl Drop for Stop<'_> {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// How often the early store's writer looks for files opened meanwhile.
const EARLY_POLL: std::time::Duration = std::time::Duration::from_millis(20);

/// While the checkout's bulk write holds the store, write the files the
/// language server opens, and their neighbours, to an early store it reads
/// until that write is in (DEC-332): a copy of the store as the write found
/// it, plus those files. Removed once `stop` is set, by which time the store
/// holds them too. The store itself is only read, so it ends as it would
/// have; an early store that cannot be written leaves its files to the bulk write.
fn write_early(
    main: &Path,
    root: &Path,
    files: &scan::Files,
    hints: &crate::background::Hints,
    written: &HashSet<String>,
    (mut read, of): (u64, u64),
    stop: &std::sync::atomic::AtomicBool,
) {
    let early = crate::store::early::dir(main, std::process::id());
    let mut written = written.clone();
    let mut store: Option<Store> = None;
    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
        let (asked, part) = wanted(root, files, hints, &written);
        if part.is_empty() {
            std::thread::sleep(EARLY_POLL);
            continue;
        }
        read += part.len() as u64;
        if early_part(main, &early, &mut store, root, &part, &asked, (read, of)).is_err() {
            break;
        }
        written.extend(part.into_keys());
    }
    drop(store);
    crate::store::early::remove(&early);
}

/// Write `part` of the checkout at `root` to its early store in `early`,
/// copying the store into it first when there is none yet: in another
/// directory, renamed once whole, so a reader never opens half of one.
fn early_part(
    main: &Path,
    early: &Path,
    store: &mut Option<Store>,
    root: &Path,
    part: &scan::Files,
    asked: &[(String, crate::core::Facts)],
    (read, of): (u64, u64),
) -> anyhow::Result<()> {
    let write = |store: &mut Store| -> anyhow::Result<()> {
        let given: HashMap<&str, &crate::core::Facts> = asked
            .iter()
            .map(|(path, facts)| (path.as_str(), facts))
            .collect();
        let mut facts: HashMap<Oid, crate::core::Facts> = HashMap::new();
        for (path, oid) in part {
            if facts.contains_key(oid) || store.has_blob(oid)? {
                continue;
            }
            let parsed = match given.get(path.as_str()) {
                Some(facts) => (*facts).clone(),
                None => match extract::read_file(root, path) {
                    Some(facts) => facts,
                    None => continue,
                },
            };
            facts.insert(oid.clone(), parsed);
        }
        let root = root.to_string_lossy();
        store.batch(|store| {
            store.write_part(&root, part, facts)?;
            store.set_warming(&root, read, of)?;
            anyhow::Ok(())
        })
    };
    if let Some(store) = store.as_mut() {
        return write(store);
    }
    let name = main.file_name().unwrap_or_default();
    let mut fresh = early.as_os_str().to_os_string();
    fresh.push(".new");
    let fresh = PathBuf::from(fresh);
    crate::store::early::remove(&fresh);
    std::fs::create_dir_all(&fresh)?;
    let copied = (|| {
        if !crate::store::early::copy(main, &fresh.join(name))? {
            anyhow::bail!("the store could not be copied cleanly");
        }
        let mut copy = Store::open(&fresh.join(name))?;
        write(&mut copy)?;
        drop(copy);
        Ok(std::fs::rename(&fresh, early)?)
    })();
    if copied.is_err() {
        crate::store::early::remove(&fresh);
        return copied;
    }
    *store = Some(Store::open(&early.join(name))?);
    Ok(())
}

/// The Ruby (when given) and `gems` written in one commit with the bundle
/// recorded as `repo`'s, and `also` — the Ruby's signatures, or a first
/// index's progress — in it.
#[allow(clippy::too_many_arguments)]
fn gem_batch(
    store: &mut Store,
    repo: &Path,
    report: &mut GemReport,
    ruby: Option<(&crate::gems::stdlib::Stdlib, Option<scan::Files>)>,
    gems: Vec<(PathBuf, scan::Files)>,
    used: &[(String, String)],
    known: &mut Option<Known>,
    pool: &rayon::ThreadPool,
    profile: &mut Option<profile::Profile>,
    also: impl FnOnce(&mut Store, &mut Option<profile::Profile>) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    store.batch(|store| {
        if let Some((stdlib, files)) = ruby {
            report.stdlib = Some(index_stdlib(store, stdlib, files, known, pool, profile)?);
        }
        for counts in index_bundle(store, gems, known, pool, profile)? {
            report.indexed += 1;
            report.files += counts.files;
        }
        let repo = repo.to_string_lossy();
        let stdlib_root = report.stdlib.as_ref().map(|s| s.root.clone());
        store.set_gems_used(&repo, used, stdlib_root.as_deref())?;
        if let Some(stdlib) = report.stdlib.as_mut() {
            stdlib.hidden = store.hidden_default_gems(&repo)?;
        }
        also(store, profile)?;
        anyhow::Ok(())
    })?;
    if let Some(profile) = profile.as_mut() {
        // The bundle's one commit, outside every gem's own write.
        profile.phase("commit", store.take_timing().commit);
    }
    Ok(())
}

/// What an index reads beyond the checkout — its Ruby's stdlib and the
/// bundle's gems — located and walked before anything is written, so the
/// index knows what it will read before it writes any of it.
struct GemPlan {
    report: GemReport,
    stdlib: Option<crate::gems::stdlib::Stdlib>,
    /// The stdlib's files, when this machine has not read them yet.
    stdlib_files: Option<scan::Files>,
    /// Every gem root the bundle resolves, canonical, with the gem's name.
    used: Vec<(String, String)>,
    /// The gems new to the store, listed: each one's `lib/`, not yet read.
    fresh: Vec<(PathBuf, Vec<String>)>,
    /// Files the tree spans from a stdlib and gems the store already holds.
    known_files: u64,
}

impl GemPlan {
    /// Files of the checkout's tree that are not the checkout's own.
    fn files(&self) -> u64 {
        let new = self.stdlib_files.as_ref().map_or(0, |f| f.len())
            + self.fresh.iter().map(|(_, f)| f.len()).sum::<usize>();
        self.known_files + new as u64
    }
}

/// Locate the stdlib and the gems this checkout resolves, and walk those not
/// on this machine yet. Writes nothing.
///
/// A named-but-unlocated gem is a hole in every answer that would have come
/// from it, so it is reported rather than silently absent.
fn plan_gems(
    store: &Store,
    repo: &Path,
    pool: &rayon::ThreadPool,
    profile: &mut Option<profile::Profile>,
) -> anyhow::Result<GemPlan> {
    // The Ruby first: its stdlib is indexed before the gems, which reopen it
    // (DEC-180), and its gem directories are searched before any other's
    // (DEC-291). The one the last index chose stands unless the checkout
    // names another (DEC-271).
    let last = store
        .tree_roots(&repo.to_string_lossy())?
        .stdlib
        .map(PathBuf::from);
    let stdlib = crate::gems::stdlib::for_checkout(repo, last.as_deref());
    // Reading the lockfile and stat-ing ~200 conventional paths. Small, but it
    // happens on every index including a no-op, so it is worth naming.
    let (located, resolved_from) = profile::timed(profile, "gem-scan", || {
        crate::gems::for_checkout(repo, stdlib.as_ref())
    });
    let ruby = match &resolved_from {
        Some(crate::gems::Resolved::Declared { ruby }) => Some(ruby.clone()),
        _ => None,
    };
    let mut report = GemReport {
        lockfile: resolved_from == Some(crate::gems::Resolved::Lockfile),
        resolved_from: resolved_from.as_ref().map(crate::gems::Resolved::as_str),
        ruby,
        ruby_not_found: crate::gems::stdlib::named_missing(repo),
        ruby_unmet: stdlib
            .as_ref()
            .filter(|stdlib| stdlib.how != crate::gems::stdlib::How::Named)
            .and_then(|stdlib| crate::gems::stdlib::unmet(repo, &stdlib.root)),
        about: stdlib.as_ref().map(crate::gems::stdlib::Stdlib::about),
        ..GemReport::default()
    };
    let mut known: Vec<String> = Vec::new();
    let stdlib_files = match &stdlib {
        Some(stdlib) if !store.has_checkout(&stdlib.root.to_string_lossy())? => {
            Some(crate::gems::stdlib::files(&stdlib.root))
        }
        Some(stdlib) => {
            known.push(stdlib.root.to_string_lossy().into_owned());
            None
        }
        None => None,
    };
    // Which gems this bundle resolves, whether or not they needed indexing —
    // an already-known gem still belongs to this app, and that is what makes a
    // position inside it answerable from here (DEC-029).
    let mut used: Vec<(String, String)> = Vec::new();
    let mut fresh: Vec<PathBuf> = Vec::new();
    for entry in located {
        let named = match entry.gem.version.as_str() {
            "" => entry.gem.name.clone(),
            version => format!("{} {version}", entry.gem.name),
        };
        if !entry.unread.is_empty() {
            report.unread.push(Unread {
                gem: named.clone(),
                requirements: entry.unread.clone(),
            });
        }
        let gem_root = match entry.place {
            crate::gems::Place::Dir(root) => root,
            crate::gems::Place::InCheckout => {
                report.from_path += 1;
                continue;
            }
            // A default gem at the version the stdlib ships: its code is the
            // stdlib just indexed, found wherever rubygems left its directory.
            crate::gems::Place::Missing(
                crate::gems::Absence::NotInstalled | crate::gems::Absence::DefaultGem(_),
            ) if stdlib
                .as_ref()
                .is_some_and(|s| s.ships(&entry.gem.name, &entry.gem.version)) =>
            {
                report.found += 1;
                report.from_stdlib += 1;
                report.picked.push(named);
                continue;
            }
            crate::gems::Place::Missing(crate::gems::Absence::NotInstalled) => {
                report.missing.push(named);
                continue;
            }
            crate::gems::Place::Missing(absence) => {
                let why = absence.why(&entry.gem.name);
                report.unlocated.push(Unlocated { gem: named, why });
                continue;
            }
        };
        report.found += 1;
        if entry.elsewhere {
            report.other_ruby.push(named.clone());
        }
        report.picked.push(named);
        if matches!(entry.gem.source, crate::gems::Source::Git { .. }) {
            report.from_git += 1;
        }
        // Canonical, like every other checkout root the store keys on: a query
        // canonicalizes the path it is given, and a gem located through a
        // symlinked GEM_HOME would otherwise be stored under a name no query
        // ever asks for (DEC-024).
        let gem_root = std::fs::canonicalize(&gem_root).unwrap_or(gem_root);
        let root_str = gem_root.to_string_lossy().into_owned();
        used.push((root_str.clone(), entry.gem.name.clone()));
        if fresh.contains(&gem_root) {
            report.already_indexed += 1;
            continue;
        }
        if store.has_checkout(&root_str)? {
            report.already_indexed += 1;
            if !known.contains(&root_str) {
                known.push(root_str);
            }
            continue;
        }
        fresh.push(gem_root);
    }
    Ok(GemPlan {
        report,
        stdlib,
        stdlib_files,
        used,
        fresh: list_gems(&fresh, pool, profile),
        known_files: store.file_count(&known)?,
    })
}

/// Index what `plan_gems` found — the stdlib, then the gems new to this
/// machine — in one commit, and record which ones this checkout uses.
fn index_gems(
    store: &mut Store,
    repo: &Path,
    plan: GemPlan,
    known: &mut Option<Known>,
    pool: &rayon::ThreadPool,
    profile: &mut Option<profile::Profile>,
) -> anyhow::Result<GemReport> {
    let GemPlan {
        mut report,
        stdlib,
        stdlib_files,
        used,
        fresh,
        ..
    } = plan;
    let ruby = stdlib.as_ref().map(|stdlib| (stdlib, stdlib_files));
    let fresh = hash_gems(fresh, pool, profile);
    let mut rbs = None;
    gem_batch(
        store,
        repo,
        &mut report,
        ruby,
        fresh,
        &used,
        known,
        pool,
        profile,
        |s, profile| {
            if let Some(stdlib) = &stdlib {
                rbs = signatures(s, stdlib, profile)?;
            }
            Ok(())
        },
    )?;
    if let Some(report) = report.stdlib.as_mut() {
        report.rbs = rbs;
    }
    Ok(report)
}

/// Its Ruby's signatures, which core and the stdlib's compiled half are
/// served from, read once per Ruby (DEC-240).
fn signatures(
    store: &mut Store,
    stdlib: &crate::gems::stdlib::Stdlib,
    profile: &mut Option<profile::Profile>,
) -> anyhow::Result<Option<crate::rbs::Report>> {
    profile::timed(profile, "rbs", || crate::rbs::prepare(store, stdlib))
}

/// Each gem's `lib/`, listed on the pool: it is where a gem's public code
/// lives, and a gem's spec/ and test/ trees are large and never navigated to.
/// Nothing is read until `hash_gems`, so a first index reads the gems the
/// open files name before the rest (DEC-330).
fn list_gems(
    gems: &[PathBuf],
    pool: &rayon::ThreadPool,
    profile: &mut Option<profile::Profile>,
) -> Vec<(PathBuf, Vec<String>)> {
    if gems.is_empty() {
        return Vec::new();
    }
    profile::timed(profile, "gem-walk", || {
        pool.install(|| {
            gems.par_iter()
                .map(|gem| {
                    let mut paths = scan::list(gem, "lib");
                    paths.retain(|path| {
                        !path
                            .strip_prefix("lib/")
                            .is_some_and(crate::gems::stdlib::opt_in)
                    });
                    (gem.clone(), paths)
                })
                .filter(|(_, paths)| !paths.is_empty())
                .collect()
        })
    })
}

/// `list_gems`' files, read and hashed on the pool.
fn hash_gems(
    gems: Vec<(PathBuf, Vec<String>)>,
    pool: &rayon::ThreadPool,
    profile: &mut Option<profile::Profile>,
) -> Vec<(PathBuf, scan::Files)> {
    if gems.is_empty() {
        return Vec::new();
    }
    profile::timed(profile, "gem-hash", || {
        pool.install(|| {
            gems.into_par_iter()
                .map(|(gem, paths)| {
                    let files = scan::hash(&gem, paths);
                    (gem, files)
                })
                .filter(|(_, files)| !files.is_empty())
                .collect()
        })
    })
}

/// Files the bundle's stream parses at a time, and holds for the writer: two
/// chunks' facts are in memory at once.
const BUNDLE_CHUNK: usize = 128;

/// Index the gems new to the store as one stream (DEC-232): each gem's
/// `lib/` as `walk_gems` found it, every new blob parsed on the pool across gem
/// boundaries, and each gem written in turn as its files arrive — where one
/// gem at a time left the pool idle while each small gem was walked and
/// written. What each gem indexed, for those with files.
fn index_bundle(
    store: &mut Store,
    walked: Vec<(PathBuf, scan::Files)>,
    known: &mut Option<Known>,
    pool: &rayon::ThreadPool,
    profile: &mut Option<profile::Profile>,
) -> anyhow::Result<Vec<crate::store::Indexed>> {
    if walked.is_empty() {
        return Ok(Vec::new());
    }
    // One path per unknown blob, in gem order: a blob two gems share is
    // parsed for the first and known by the time the second is written.
    let mut to_parse: Vec<(usize, Oid, PathBuf)> = Vec::new();
    {
        // Inside the bundle's transaction (`gem_batch`): under the lock.
        let known = known_now(store, known, profile)?;
        let mut seen: HashSet<&Oid> = HashSet::new();
        for (at, (root, files)) in walked.iter().enumerate() {
            let wanted: HashSet<&Oid> = files.values().collect();
            let before = to_parse.len();
            for (rel, oid) in files.iter() {
                if !known.oids.contains(oid) && seen.insert(oid) {
                    to_parse.push((at, oid.clone(), root.join(rel)));
                }
            }
            if let Some(profile) = profile.as_mut() {
                let parsed = to_parse.len() - before;
                profile.blobs += wanted.len();
                profile.parsed += parsed;
                profile.skipped += wanted.len() - parsed;
            }
        }
    }

    // Parsed a chunk at a time on the pool, in order, while this thread
    // writes the chunk before: each gem's facts arrive together and in turn.
    let mut received = Received::new(profile.is_some());
    let (send, parsed) =
        std::sync::mpsc::sync_channel::<(usize, Oid, extract::Parsed)>(BUNDLE_CHUNK);
    let (indexed, parse_done) = std::thread::scope(|scope| {
        let parsing = scope.spawn(move || {
            for chunk in to_parse.chunks(BUNDLE_CHUNK) {
                let parsed: Vec<_> = pool.install(|| {
                    chunk
                        .par_iter()
                        .filter_map(|(at, oid, path)| Some((*at, oid.clone(), parse_file(path)?)))
                        .collect()
                });
                for item in parsed {
                    // The writer failed and let go; its error is the answer.
                    if send.send(item).is_err() {
                        return std::time::Instant::now();
                    }
                }
            }
            std::time::Instant::now()
        });
        let mut parsed = parsed.into_iter().peekable();
        let written: rusqlite::Result<Vec<_>> = walked
            .iter()
            .enumerate()
            .map(|(at, (root, files))| {
                let facts = std::iter::from_fn(|| parsed.next_if(|(gem, _, _)| *gem == at))
                    .map(|(_, oid, p)| received.take(oid, p));
                store.write(&root.to_string_lossy(), files, facts, 0)
            })
            .collect();
        // A write that failed lets go of the rest, and the parse stops.
        drop(parsed);
        (written, parsing.join().expect("the parse does not panic"))
    });
    let indexed = indexed?;
    received.finish(store, profile, known, parse_done);
    Ok(indexed)
}

/// Index a Ruby's stdlib once per machine, with the files its default gems
/// own (DEC-180). Its bytes are the Ruby's install, which does not change.
fn index_stdlib(
    store: &mut Store,
    stdlib: &crate::gems::stdlib::Stdlib,
    // `None` when the store holds it already (`plan_gems`).
    files: Option<scan::Files>,
    known: &mut Option<Known>,
    pool: &rayon::ThreadPool,
    profile: &mut Option<profile::Profile>,
) -> anyhow::Result<StdlibReport> {
    let root = stdlib.root.to_string_lossy().into_owned();
    let mut report = StdlibReport {
        root: root.clone(),
        ruby: stdlib.ruby.clone(),
        indexed: false,
        files: 0,
        hidden: Vec::new(),
        rbs: None,
    };
    let Some(files) = files else {
        return Ok(report);
    };
    let counts = index_files(store, &stdlib.root, &files, 0, false, known, pool, profile)?;
    let gems = crate::gems::stdlib::default_gems(&stdlib.root);
    store.set_default_gems(
        &root,
        gems.iter().flat_map(|gem| {
            gem.files
                .iter()
                .filter(|path| files.contains_key(*path))
                .map(|path| (gem.name.as_str(), gem.version.as_str(), path.as_str()))
        }),
    )?;
    store.set_compiled(&root, &crate::gems::stdlib::compiled(&stdlib.root, &files))?;
    report.indexed = true;
    report.files = counts.files;
    Ok(report)
}

/// The Ruby's stdlib, as an index saw it.
#[derive(Debug, serde::Serialize)]
struct StdlibReport {
    /// `<prefix>/lib/ruby/<abi>`.
    root: String,
    /// Which Ruby, and how it was chosen.
    ruby: String,
    /// Read for the first time on this machine by this index.
    indexed: bool,
    files: usize,
    /// Default gems this app bundles a copy of, whose stdlib files it does
    /// not see.
    hidden: Vec<String>,
    /// The rbs gem whose signatures core and the stdlib are served from;
    /// `null` when the Ruby carries none, and then nothing is (DEC-240).
    rbs: Option<crate::rbs::Report>,
}

#[derive(Debug, Default, serde::Serialize)]
struct GemReport {
    /// Whether the checkout has a `Gemfile.lock`.
    lockfile: bool,
    /// Where the gem list came from: `lockfile`, or `declared` — the
    /// gemspecs and Gemfile, each at the highest installed version that meets
    /// it (DEC-134). Absent when nothing names a gem, and then none is
    /// indexed, which is said rather than left to look like an empty bundle.
    #[serde(skip_serializing_if = "Option::is_none")]
    resolved_from: Option<&'static str>,
    /// Without a lockfile, the Ruby whose installed gems were picked from,
    /// and how it was chosen (DEC-152).
    #[serde(skip_serializing_if = "Option::is_none")]
    ruby: Option<String>,
    /// The Ruby version the checkout names when none of its installs is
    /// found, so another Ruby's stdlib and gems answer (DEC-270).
    #[serde(skip_serializing_if = "Option::is_none")]
    ruby_not_found: Option<String>,
    /// The checkout's Ruby requirements, when no Ruby found meets them, so
    /// the one run on is outside them (DEC-610).
    #[serde(skip_serializing_if = "Option::is_none")]
    ruby_unmet: Option<String>,
    /// Named by the lockfile and present on disk.
    found: usize,
    /// Of those, checked out from git (`bundler/gems/`).
    from_git: usize,
    /// Path gems whose source is inside this checkout, indexed with it.
    from_path: usize,
    /// Default gems at the version the stdlib ships, whose code is the stdlib.
    from_stdlib: usize,
    /// Read for the first time on this machine.
    indexed: usize,
    /// Already known — the shared case, and the reason this is cheap.
    already_indexed: usize,
    files: usize,
    /// Named by the lockfile and not found. A visible hole, not an absence.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    missing: Vec<String>,
    /// Git and path gems not indexed, each with the reason: a checkout
    /// that is not where bundler puts it is not the same hole as a gem
    /// nobody installed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unlocated: Vec<Unlocated>,
    /// Each gem found, `name version`: exact from a lockfile, trekr's pick
    /// without one.
    picked: Vec<String>,
    /// Of those, found only in another Ruby's gem directories: the
    /// checkout's Ruby has no copy that would do (DEC-291).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    other_ruby: Vec<String>,
    /// Picks whose requirement, as written, trekr could not read without
    /// running it; the highest installed was taken instead.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unread: Vec<Unread>,
    /// The Ruby's stdlib, absent when the checkout names no gem and no Ruby,
    /// or its Ruby's stdlib was not found.
    #[serde(skip_serializing_if = "Option::is_none")]
    stdlib: Option<StdlibReport>,
    /// The Ruby the checkout runs on, as `--index`'s top-level `ruby`
    /// reports it rather than here (DEC-292).
    #[serde(skip)]
    about: Option<crate::gems::stdlib::About>,
}

#[derive(Debug, serde::Serialize)]
struct Unread {
    gem: String,
    /// The requirements as written: `version`, `"~> #{ENV['V']}"`.
    requirements: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
struct Unlocated {
    /// `name version`, as `missing` writes it.
    gem: String,
    why: String,
}

pub(super) fn cmd_index(
    out: Output,
    path: &Path,
    jobs: usize,
    want_profile: bool,
    with_gems: bool,
) -> anyhow::Result<ExitCode> {
    // First, before the scan or the pool starts a thread.
    crate::background::yield_if_background();
    incomplete::watch_signals();
    let mut profile = want_profile.then(profile::Profile::default);
    let jobs = worker_count(jobs);
    if let Some(profile) = profile.as_mut() {
        profile.jobs = jobs;
    }

    // A gem is refreshed through its app, a git gem's clone included, though
    // git would call it a checkout (DEC-150).
    let mut store = open_store()?;
    if let Some(gem) = gem_holding(&store, path) {
        let app = store.app_for_gem(&gem)?;
        let app = app.as_deref().map_or("<the app>".into(), paths::pretty);
        return Err(Failure::NotARepo.error(format!(
            "{} is a gem, indexed from an app's bundle rather than as a checkout; \
             to pick up an edit to it: trekr --drop {0} && trekr --index {app}",
            paths::pretty(&gem)
        )));
    }
    let root = named_checkout(path)?;
    let root_str = root.to_string_lossy().into_owned();
    // Sampled *before* the scan, deliberately. A fingerprint taken afterwards
    // would cover edits this index never saw and the next query would call them
    // fresh; taken first, the worst case is a probe that reports stale when it
    // is not, which costs one re-read and never a wrong answer.
    let git_state = scan::git_fingerprint(&root).unwrap_or(0);
    let files = profile::timed(&mut profile, "scan", || scan::scan(&root))?;

    incomplete::indexing(&root_str, out);
    let indexed = index_all(
        &mut store,
        &root,
        &files,
        git_state,
        with_gems,
        jobs,
        &mut profile,
    );
    let (counts, gems) = match indexed {
        Err(error) if error.is::<ClaimOutwaited>() || incomplete::outwaited(&error) => {
            drop(store);
            incomplete::report(
                out,
                &root_str,
                match error.is::<ClaimOutwaited>() {
                    true => "another trekr's index of it outlasted the writer wait",
                    false => "another trekr writer kept the write lock longer than an index waits",
                },
            );
            crate::usage::outcome(Outcome::Error("incomplete"));
            return Ok(ExitCode::from(2));
        }
        other => other?,
    };
    incomplete::finished();
    // A path inside a checkout indexes all of it, by design: say which.
    let within = std::fs::canonicalize(path)
        .ok()
        .filter(|named| *named != root)
        .map(|named| {
            format!(
                "the checkout containing {}: ",
                paths::pretty(&named.to_string_lossy())
            )
        })
        .unwrap_or_default();
    match out {
        Output::Text => println!(
            "indexed {within}{} — {} files, {} blobs{}, {} parsed ({} defs, {} refs, {} calls)",
            paths::pretty(&root_str),
            counts.files,
            counts.blobs,
            // A blob is content: files with the same bytes share one.
            match counts.files.saturating_sub(counts.blobs) {
                0 => String::new(),
                n => format!(
                    " ({n} {} another's bytes)",
                    if n == 1 {
                        "file repeats"
                    } else {
                        "files repeat"
                    }
                ),
            },
            counts.parsed,
            counts.defs,
            counts.refs,
            counts.calls
        ),
        _ => emit_json(
            out,
            &serde_json::json!({
                "repo": root_str,
                "indexed": counts,
                "gems": gems,
                "ruby": gems.about,
            }),
        )?,
    }
    // Part of the report, beside the gems line it replaces: JSON has
    // `gems.lockfile`, and stderr stays for errors and `--profile`.
    match gems.resolved_from {
        _ if out != Output::Text || !with_gems => {}
        None => println!(
            "gems — none: no Gemfile.lock, gemspec or Gemfile here, so a call into a gem \
             answers as residue"
        ),
        Some("declared") => println!(
            "gems — no Gemfile.lock: the gemspecs' and Gemfile's dependencies, each at the \
             highest installed version that meets it, in {}",
            gems.ruby.as_deref().unwrap_or("every installed Ruby")
        ),
        // The lockfile is what is read, so an edit to the Gemfile since is not.
        Some(_) if gemfile_is_newer(&root) => println!(
            "gems — the Gemfile is newer than Gemfile.lock, which is what is read: \
             an edit to the Gemfile counts once `bundle install` relocks"
        ),
        Some(_) => {}
    }
    let holes = !gems.missing.is_empty() || !gems.unlocated.is_empty() || !gems.unread.is_empty();
    if out == Output::Text && (gems.found > 0 || holes) {
        let from_git = match gems.from_git {
            0 => String::new(),
            n => format!(" ({n} from git)"),
        };
        println!(
            "gems — {} resolved{from_git}, {} newly indexed ({} files), {} already known",
            gems.found, gems.indexed, gems.files, gems.already_indexed
        );
        // A hole in the index, said out loud: every answer that would have
        // come from these gems is a residue with no reason attached.
        let named_by = match gems.resolved_from {
            Some("declared") => "the gemspec or Gemfile",
            _ => "Gemfile.lock",
        };
        if !gems.missing.is_empty() {
            println!(
                "  {} named by {named_by} but not installed: {}",
                gems.missing.len(),
                abridged(&gems.missing)
            );
        }
        // Without a lockfile, which version is trekr's choice, so it is said.
        if gems.resolved_from == Some("declared") && !gems.picked.is_empty() {
            println!("  picked: {}", abridged(&gems.picked));
        }
        if !gems.other_ruby.is_empty() {
            println!(
                "  {} found only in another Ruby's gems, not the checkout's Ruby's: {}",
                gems.other_ruby.len(),
                abridged(&gems.other_ruby)
            );
        }
        if !gems.unread.is_empty() {
            let unread: Vec<String> = gems
                .unread
                .iter()
                .map(|u| format!("{} ({})", u.gem, u.requirements.join(", ")))
                .collect();
            println!(
                "  {} with a requirement trekr cannot read, at the highest installed: {}",
                unread.len(),
                abridged(&unread)
            );
        }
        // One line per reason: a monorepo's gems share one checkout.
        let mut reasons: Vec<&str> = Vec::new();
        for unlocated in &gems.unlocated {
            if !reasons.contains(&unlocated.why.as_str()) {
                reasons.push(&unlocated.why);
            }
        }
        for why in reasons {
            let named: Vec<String> = gems
                .unlocated
                .iter()
                .filter(|u| u.why == why)
                .map(|u| u.gem.clone())
                .collect();
            println!(
                "  {} named by {named_by} — {why}: {}",
                named.len(),
                abridged(&named)
            );
        }
    }
    if out == Output::Text
        && with_gems
        && let Some(version) = &gems.ruby_not_found
    {
        println!(
            "ruby — the checkout names Ruby {version}, which is not installed \
             (looked in rvm, rbenv, asdf, chruby, mise and Homebrew); {}",
            match &gems.stdlib {
                Some(stdlib) => format!("running on {} instead", stdlib.ruby),
                None => "no other Ruby was chosen".to_string(),
            }
        );
    }
    if out == Output::Text
        && with_gems
        && let Some(requirements) = &gems.ruby_unmet
    {
        // The line above, when there is one, already said what it runs on.
        match (&gems.stdlib, &gems.ruby_not_found) {
            (Some(stdlib), None) => println!(
                "ruby — no installed Ruby meets {requirements}; running on {} instead",
                stdlib.ruby
            ),
            _ => println!("ruby — no installed Ruby meets {requirements}"),
        }
    }
    if out == Output::Text && with_gems && gems.stdlib.is_none() {
        // Core is its Ruby's: with none, nothing describes it (DEC-240).
        println!("stdlib — none: no Ruby found for this checkout, so nothing is known of core");
    }
    if out == Output::Text
        && let Some(stdlib) = &gems.stdlib
    {
        let read = match stdlib.indexed {
            true => format!("{} files newly indexed", stdlib.files),
            false => "already known".to_string(),
        };
        println!(
            "stdlib — {}, {read}: {}",
            stdlib.ruby,
            paths::pretty(&stdlib.root)
        );
        if !stdlib.hidden.is_empty() {
            println!(
                "  the bundle's own copy answers for: {}",
                abridged(&stdlib.hidden)
            );
        }
        match &stdlib.rbs {
            Some(rbs) => println!(
                "  signatures — rbs {}, {}, {}: {}",
                rbs.version,
                rbs.chosen.why(),
                match (rbs.read, rbs.kept) {
                    (true, _) => "read",
                    (false, false) => "already known",
                    (false, true) => "kept from the last index, as none as good is found now",
                },
                paths::pretty(&rbs.path)
            ),
            None => println!(
                "  signatures — none: this Ruby carries no rbs gem, so nothing is known of core"
            ),
        }
    }
    // Every index prepares the tree snapshot the next query would otherwise
    // assemble: the cost is the same either way, and a query's budget is
    // milliseconds where an index's is seconds (DEC-192).
    stall_for_tests("TREKR_TEST_STALL_TREE_MS");
    profile::timed(&mut profile, "tree", || {
        crate::tree::Tree::prepare(&store, &root_str)
    })?;
    store.truncate_wal();
    crate::cleanup::after_index(&mut store);
    if let Some(profile) = profile {
        match out {
            Output::Text => profile.report_text(),
            // Structured, but still on stderr, so `--json | jq` sees only the
            // answer on stdout.
            _ => profile.report_json(),
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Whether the checkout's Gemfile was written after its Gemfile.lock.
fn gemfile_is_newer(root: &Path) -> bool {
    let written = |name: &str| std::fs::metadata(root.join(name)).and_then(|m| m.modified());
    matches!(
        (written("Gemfile"), written("Gemfile.lock")),
        (Ok(gemfile), Ok(lock)) if gemfile > lock
    )
}

/// A list of gems, cut short. The full list is in `--json`; a lockfile
/// naming every optional adapter would otherwise bury the report.
fn abridged(gems: &[String]) -> String {
    const SHOWN: usize = 6;
    let mut out = gems
        .iter()
        .take(SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if let Some(more) = gems.len().checked_sub(SHOWN).filter(|n| *n > 0) {
        out.push_str(&format!(", and {more} more (--json lists all)"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file opened while the checkout's bulk write holds the store goes to
    /// an early store — a copy of the store with it and its neighbours added —
    /// and never to the store, and the early store is gone once the write is in
    /// (DEC-332).
    #[test]
    fn a_file_opened_during_the_bulk_write_goes_to_an_early_store_only() {
        let dir = std::env::temp_dir().join(format!("trekr-early-writer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("repo/lib")).unwrap();
        // Canonical, as an index's root is: hints are read against it.
        let repo = std::fs::canonicalize(dir.join("repo")).unwrap();
        let sources = [
            ("open.rb", "class Opened\n  include Kit\nend\n"),
            ("lib/kit.rb", "module Kit\nend\n"),
            ("other.rb", "class Other\nend\n"),
        ];
        let mut files = scan::Files::new();
        for (path, source) in sources {
            std::fs::write(repo.join(path), source).unwrap();
            files.insert(path.to_string(), scan::hash_blob(source.as_bytes()));
        }
        let main = dir.join("t.db");
        let root = repo.to_string_lossy().into_owned();
        let mut store = Store::open(&main).unwrap();
        let other: scan::Files = files
            .iter()
            .filter(|(p, _)| *p == "other.rb")
            .map(|(p, o)| (p.clone(), o.clone()))
            .collect();
        let facts = vec![(
            other["other.rb"].clone(),
            extract::extract(sources[2].1.as_bytes()),
        )];
        store.write_part(&root, &other, facts).unwrap();
        store.set_warming(&root, 1, 3).unwrap();

        let hints = crate::background::Hints::sent(&[repo.join("open.rb")]);
        let written = HashSet::from(["other.rb".to_string()]);
        let stop = std::sync::atomic::AtomicBool::new(false);
        let early = crate::store::early::path(&main, std::process::id());
        std::thread::scope(|scope| {
            let writer =
                scope.spawn(|| write_early(&main, &repo, &files, &hints, &written, (1, 3), &stop));
            let done = Stop(&stop);
            let started = std::time::Instant::now();
            while !early.exists() && started.elapsed() < std::time::Duration::from_secs(10) {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(early.exists(), "an early store is written");
            let copy = Store::open(&early).unwrap();
            assert_eq!(
                copy.file_count(std::slice::from_ref(&root)).unwrap(),
                3,
                "the opened file and what it names"
            );
            assert_eq!(copy.warming(&root).unwrap().map(|w| w.read), Some(3));
            assert_eq!(
                store.file_count(std::slice::from_ref(&root)).unwrap(),
                1,
                "the store is not written"
            );
            drop(copy);
            drop(done);
            writer.join().unwrap();
        });
        assert!(!early.exists(), "removed once the bulk write is in");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file opened after a first part read it as another's neighbour still
    /// brings its own neighbours forward (DEC-322).
    #[test]
    fn a_file_read_as_a_neighbour_still_brings_its_own_when_opened() {
        let dir = std::env::temp_dir().join(format!("trekr-wanted-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        // Canonical, as an index's root is: hints are read against it.
        let repo = std::fs::canonicalize(&dir).unwrap();
        let mut files = scan::Files::new();
        for (path, source) in [
            ("open.rb", "class Opened\n  include Kit\nend\n"),
            ("lib/kit.rb", "module Kit\nend\n"),
        ] {
            std::fs::write(repo.join(path), source).unwrap();
            files.insert(path.to_string(), scan::hash_blob(source.as_bytes()));
        }
        let hints = crate::background::Hints::sent(&[repo.join("open.rb")]);
        let written = HashSet::from(["open.rb".to_string()]);
        let (asked, part) = wanted(&repo, &files, &hints, &written);
        assert_eq!(asked.len(), 1);
        assert_eq!(part.keys().collect::<Vec<_>>(), ["lib/kit.rb"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cut_list_says_where_the_rest_is() {
        let gems: Vec<String> = (0..8).map(|i| format!("g{i} 1.0")).collect();
        assert!(abridged(&gems).ends_with("and 2 more (--json lists all)"));
        assert!(!abridged(&gems[..6]).contains("more"));
    }

    #[test]
    fn a_load_of_half_the_store_rebuilds_its_indexes() {
        assert!(bulk_load(1, 0), "a fresh store");
        assert!(bulk_load(50_000, 50_000));
        assert!(bulk_load(25_000, 50_000));
        assert!(!bulk_load(12_500, 50_000), "a quarter inserts");
        assert!(!bulk_load(0, 0), "nothing to load");
    }
}
