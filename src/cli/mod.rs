//! The command line. Every command that prints anything honors `--json` and
//! `--ndjson`, because the primary consumer is an agent, not a person.
//!
//! Operations are flags rather than subcommands (rq's convention): no word is
//! reserved, and the default action stays free for the query verbs the resolve
//! layer will add.

pub(crate) mod position;
mod profile;

use crate::core::Oid;
use crate::store::Store;
use crate::tree::{Status, Tree};
use crate::{extract, scan};
use anyhow::Context;
use clap::Parser;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "trekr",
    version,
    about = "Ruby code intelligence: position→meaning, definition→references.",
    long_about = "Ruby code intelligence for agents.\n\n\
        Facts are keyed by git blob OID, so every worktree of a repo shares one \
        index and a reindex with no edits parses nothing.\n\n\
        EXIT CODES\n  \
        0  something was indexed, or a query matched\n  \
        1  nothing matched / nothing to do\n  \
        2  the request could not be served (not a repo, unreadable file)"
)]
struct Cli {
    /// Index the checkout containing this path (default: the current directory).
    #[arg(long, value_name = "PATH", num_args = 0..=1, default_missing_value = ".")]
    index: Option<PathBuf>,

    /// Report what is indexed, per checkout, with the shared blob totals.
    #[arg(long, conflicts_with_all = ["index", "symbols", "drop"])]
    status: bool,

    /// Summarize what `--serve` has been asked, from its own log: which
    /// operations, how often the answer was empty, and what they cost.
    #[arg(long, conflicts_with_all = ["index", "symbols", "drop", "refs", "def"])]
    usage: bool,

    /// Outline one file's definitions, in the order they are written.
    #[arg(long, value_name = "FILE", conflicts_with_all = ["index", "drop"])]
    symbols: Option<PathBuf>,

    /// Every mention of a name in this checkout: definitions, constant
    /// references, and call sites. Name-level — not yet resolved.
    #[arg(long, value_name = "NAME", conflicts_with_all = ["index", "drop", "symbols"])]
    refs: Option<String>,

    /// What is the name at this position, and where is it defined?
    #[arg(long, value_name = "FILE:LINE:COL", conflicts_with_all = ["index", "drop", "symbols", "refs"])]
    def: Option<String>,

    /// Answer as if asked from this checkout, instead of the one the path
    /// belongs to. Only meaningful for a position inside a **gem**, which is
    /// otherwise answered from whichever app most recently indexed it — a pick
    /// that is deterministic but moves as you work (DEC-029). Pin it when a
    /// measurement has to be reproducible.
    #[arg(long, value_name = "CHECKOUT", requires = "def")]
    context: Option<PathBuf>,

    /// The linearized ancestor chain of a class or module.
    #[arg(long, value_name = "NAME", conflicts_with_all = ["index", "drop", "symbols", "refs", "def"])]
    ancestors: Option<String>,

    /// Forget a checkout's file map (its blobs stay, for the worktrees that
    /// share them).
    #[arg(long, value_name = "PATH", num_args = 0..=1, default_missing_value = ".")]
    drop: Option<PathBuf>,

    /// Worker threads for parsing. 0 (the default) picks the machine's
    /// **physical** core count; `TREKR_JOBS` sets it too, and the flag wins.
    #[arg(long, value_name = "N", env = "TREKR_JOBS", default_value_t = 0)]
    jobs: usize,

    /// List the call sites `--refs Owner#method` ruled out, with the reason.
    ///
    /// The exclusion count is the product's central claim, so it has to be
    /// auditable rather than merely asserted.
    #[arg(long, requires = "refs")]
    include_excluded: bool,

    /// Skip the checkout's gems. They are indexed once per machine and shared
    /// by every project that resolves the same version, so the cost is paid
    /// once — but it is paid.
    #[arg(long)]
    no_gems: bool,

    /// Speak LSP over stdio. The editor owns the process: no auto-spawn, no
    /// lockfile, and it stops when stdin closes.
    #[arg(long, conflicts_with_all = ["index", "status", "symbols", "refs", "def", "ancestors", "drop"])]
    serve: bool,

    /// Report where the time went, on stderr. For `--index`, the phases of the
    /// index; for a query, the phases of the tree build behind it. With
    /// `--serve`, logs the wire-level params of every request too.
    #[arg(long)]
    profile: bool,

    /// Show why an answer came out the way it did: the rung that resolved the
    /// receiver, the confidence and what graded it, the ancestors that could
    /// not be seen, and the ranked candidates behind a residue. The same facts
    /// `--json` carries, rendered for a person.
    #[arg(long, requires = "def")]
    explain: bool,

    /// Emit results as JSON — a pretty object, or an array for row sets.
    #[arg(short = 'j', long)]
    json: bool,

    /// Emit newline-delimited JSON, one compact object per line.
    #[arg(short = 'J', long, conflicts_with = "json")]
    ndjson: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Output {
    Text,
    Json,
    Ndjson,
}

pub fn run() -> ExitCode {
    let cli = Cli::parse();
    if cli.profile {
        // The tree layer reads this rather than taking a parameter: it is
        // built from half a dozen call sites and the flag is a whole-process
        // decision.
        unsafe { std::env::set_var("TREKR_PROFILE", "1") };
    }
    let out = if cli.ndjson {
        Output::Ndjson
    } else if cli.json {
        Output::Json
    } else {
        Output::Text
    };

    let result = if cli.serve {
        crate::serve::run(cli.profile).map(|()| ExitCode::SUCCESS)
    } else if let Some(path) = &cli.index {
        cmd_index(out, path, cli.jobs, cli.profile, !cli.no_gems)
    } else if let Some(path) = &cli.symbols {
        cmd_symbols(out, path)
    } else if let Some(name) = &cli.refs {
        cmd_refs(out, name, cli.include_excluded)
    } else if let Some(spec) = &cli.def {
        cmd_def(out, spec, cli.explain, cli.context.as_deref())
    } else if let Some(name) = &cli.ancestors {
        cmd_ancestors(out, name)
    } else if let Some(path) = &cli.drop {
        cmd_drop(out, path)
    } else if cli.status {
        cmd_status(out)
    } else if cli.usage {
        cmd_usage(out)
    } else {
        eprintln!(
            "trekr: nothing to do (try --index, --status, --symbols FILE, \
             --refs NAME, --def FILE:LINE:COL, --usage)"
        );
        return ExitCode::from(1);
    };

    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("trekr: {e}");
            ExitCode::from(2)
        }
    }
}

/// The database: `$TREKR_DB`, else `~/.local/share/trekr/trekr.db`.
fn open_store() -> anyhow::Result<Store> {
    let path = match std::env::var("TREKR_DB") {
        Ok(p) => PathBuf::from(p),
        Err(_) => PathBuf::from(std::env::var("HOME")?).join(".local/share/trekr/trekr.db"),
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(Store::open(&path)?)
}

fn emit_json<T: serde::Serialize>(out: Output, value: &T) -> anyhow::Result<()> {
    let rendered = if out == Output::Json {
        serde_json::to_string_pretty(value)?
    } else {
        serde_json::to_string(value)?
    };
    println!("{rendered}");
    Ok(())
}

/// Print a row set. `None` means it was handled; `Some` hands text mode back
/// to the caller.
fn emit_rows<T: serde::Serialize>(out: Output, rows: &[T]) -> anyhow::Result<bool> {
    match out {
        Output::Json => println!("{}", serde_json::to_string_pretty(rows)?),
        Output::Ndjson => {
            for row in rows {
                println!("{}", serde_json::to_string(row)?);
            }
        }
        Output::Text => return Ok(false),
    }
    Ok(true)
}

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
fn index_files(
    store: &mut Store,
    root: &Path,
    root_str: &str,
    files: &scan::Files,
    pool: &rayon::ThreadPool,
    profile: &mut Option<profile::Profile>,
) -> anyhow::Result<crate::store::Indexed> {
    let wanted: HashSet<Oid> = files.values().cloned().collect();
    let known = profile::timed(profile, "known-diff", || store.known(&wanted))?;

    // One path per unknown blob: identical content under two names is one
    // parse, and which name it was read from cannot matter.
    let mut to_parse: HashMap<&Oid, PathBuf> = HashMap::new();
    for (rel, oid) in files {
        if !known.contains(oid) {
            to_parse.entry(oid).or_insert_with(|| root.join(rel));
        }
    }
    if let Some(profile) = profile.as_mut() {
        profile.blobs += wanted.len();
        profile.parsed += to_parse.len();
        profile.skipped += wanted.len() - to_parse.len();
    }

    let parsed: Vec<(Oid, extract::Parsed)> = profile::timed(profile, "parse", || {
        pool.install(|| {
            to_parse
                .into_par_iter()
                .filter_map(|(oid, path)| {
                    let started = std::time::Instant::now();
                    let bytes = std::fs::read(&path).ok()?;
                    let facts = extract::extract(&bytes);
                    Some((
                        oid.clone(),
                        extract::Parsed {
                            facts,
                            bytes: bytes.len() as u64,
                            elapsed: started.elapsed(),
                            path: path.to_string_lossy().into_owned(),
                        },
                    ))
                })
                .collect()
        })
    });

    if let Some(profile) = profile.as_mut() {
        profile.bytes += parsed.iter().map(|(_, p)| p.bytes).sum::<u64>();
        let slow = parsed
            .iter()
            .map(|(_, p)| profile::SlowFile {
                path: p.path.clone(),
                ms: p.elapsed.as_secs_f64() * 1000.0,
                bytes: p.bytes,
            })
            .collect();
        profile.merge_files(slow);
    }

    let facts: Vec<_> = parsed.into_iter().map(|(oid, p)| (oid, p.facts)).collect();
    Ok(profile::timed(profile, "store-write", || {
        store.write(root_str, files, facts)
    })?)
}

/// Index the gems this checkout resolves, skipping any already on this machine.
///
/// Returns the gems the lockfile named but disk did not have. A named-but-
/// unlocated gem is a hole in every answer that would have come from it, so it
/// is reported rather than silently absent.
fn index_gems(
    store: &mut Store,
    repo: &Path,
    pool: &rayon::ThreadPool,
    profile: &mut Option<profile::Profile>,
) -> anyhow::Result<GemReport> {
    // Reading the lockfile and stat-ing ~200 conventional paths. Small, but it
    // happens on every index including a no-op, so it is worth naming.
    let located = profile::timed(profile, "gem-scan", || crate::gems::for_checkout(repo));
    let mut report = GemReport::default();
    // Which gems this bundle resolves, whether or not they needed indexing —
    // an already-known gem still belongs to this app, and that is what makes a
    // position inside it answerable from here (DEC-029).
    let mut used: Vec<String> = Vec::new();
    for entry in located {
        let Some(gem_root) = entry.root else {
            if entry.is_hole() {
                report
                    .missing
                    .push(format!("{} {}", entry.gem.name, entry.gem.version));
            }
            continue;
        };
        report.found += 1;
        // Canonical, like every other checkout root the store keys on: a query
        // canonicalizes the path it is given, and a gem located through a
        // symlinked GEM_HOME would otherwise be stored under a name no query
        // ever asks for (DEC-024).
        let gem_root = std::fs::canonicalize(&gem_root).unwrap_or(gem_root);
        let root_str = gem_root.to_string_lossy().into_owned();
        used.push(root_str.clone());
        if store.has_checkout(&root_str)? {
            report.already_indexed += 1;
            continue;
        }
        // Only `lib/`: it is where a gem's public code lives, and a gem's
        // spec/ and test/ trees are large and never navigated to.
        let files = scan::walk(&gem_root, "lib");
        if files.is_empty() {
            continue;
        }
        let counts = index_files(store, &gem_root, &root_str, &files, pool, profile)?;
        report.indexed += 1;
        report.files += counts.files;
    }
    store.set_gems_used(&repo.to_string_lossy(), &used)?;
    Ok(report)
}

#[derive(Debug, Default, serde::Serialize)]
struct GemReport {
    /// Named by the lockfile and present on disk.
    found: usize,
    /// Read for the first time on this machine.
    indexed: usize,
    /// Already known — the shared case, and the reason this is cheap.
    already_indexed: usize,
    files: usize,
    /// Named by the lockfile and not found. A visible hole, not an absence.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    missing: Vec<String>,
}

fn cmd_index(
    out: Output,
    path: &Path,
    jobs: usize,
    want_profile: bool,
    with_gems: bool,
) -> anyhow::Result<ExitCode> {
    let mut profile = want_profile.then(profile::Profile::default);
    let jobs = worker_count(jobs);
    if let Some(profile) = profile.as_mut() {
        profile.jobs = jobs;
    }

    let root = scan::repo_root(path)?;
    let root_str = root.to_string_lossy().into_owned();
    let files = profile::timed(&mut profile, "scan", || scan::scan(&root))?;

    let mut store = open_store()?;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(jobs).build()?;
    let counts = index_files(&mut store, &root, &root_str, &files, &pool, &mut profile)?;

    let gems = if with_gems {
        index_gems(&mut store, &root, &pool, &mut profile)?
    } else {
        GemReport::default()
    };

    match out {
        Output::Text => println!(
            "indexed {} — {} files, {} blobs, {} parsed ({} defs, {} refs, {} calls)",
            root_str,
            counts.files,
            counts.blobs,
            counts.parsed,
            counts.defs,
            counts.refs,
            counts.calls
        ),
        _ => emit_json(
            out,
            &serde_json::json!({ "repo": root_str, "indexed": counts, "gems": gems }),
        )?,
    }
    if out == Output::Text && (gems.found > 0 || !gems.missing.is_empty()) {
        println!(
            "gems — {} resolved, {} newly indexed ({} files), {} already known",
            gems.found, gems.indexed, gems.files, gems.already_indexed
        );
        if !gems.missing.is_empty() {
            // A hole in the index, said out loud: every answer that would have
            // come from these gems is a residue with no reason attached. The
            // full list is in `--json`; a lockfile naming every optional
            // adapter would otherwise bury the report.
            const SHOWN: usize = 6;
            let shown = gems.missing.iter().take(SHOWN).cloned().collect::<Vec<_>>();
            let more = gems.missing.len().saturating_sub(shown.len());
            println!(
                "  {} named by Gemfile.lock but not installed: {}{}",
                gems.missing.len(),
                shown.join(", "),
                if more > 0 {
                    format!(", and {more} more")
                } else {
                    String::new()
                }
            );
        }
    }
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

fn cmd_status(out: Output) -> anyhow::Result<ExitCode> {
    let store = open_store()?;
    let checkouts = store.status()?;
    let totals = store.totals()?;

    if out != Output::Text {
        // One object, because the totals are the point: they are what N
        // checkouts share, not the sum of what each one costs.
        emit_json(
            out,
            &serde_json::json!({ "checkouts": checkouts, "totals": totals }),
        )?;
        return Ok(exit_on(!checkouts.is_empty()));
    }
    if checkouts.is_empty() {
        println!("nothing indexed yet (try `trekr --index`)");
        return Ok(ExitCode::from(1));
    }
    for c in &checkouts {
        println!("{:>7} files  {:>7} blobs  {}", c.files, c.blobs, c.repo);
    }
    println!(
        "\nshared: {} blobs, {} defs, {} const refs, {} calls",
        totals.blobs, totals.defs, totals.const_refs, totals.calls
    );
    Ok(ExitCode::SUCCESS)
}

/// Outline one file, by reading it.
///
/// Parsed rather than looked up: `--def` and `--refs` both reparse so that an
/// unindexed edit still answers correctly, and an outline that went stale — or
/// answered nothing at all until someone ran `--index` — was the odd one out.
/// Parsing also means any readable Ruby file outlines, in a repo or not, which
/// is the same rule the LSP surface follows (DEC-024).
fn cmd_symbols(out: Output, path: &Path) -> anyhow::Result<ExitCode> {
    let source = std::fs::read(path)?;
    let facts = extract::extract(&source);
    let symbols: Vec<crate::store::Symbol> = facts.defs.iter().map(Into::into).collect();

    if emit_rows(out, &symbols)? {
        return Ok(exit_on(!symbols.is_empty()));
    }
    if symbols.is_empty() {
        println!("no definitions in {}", path.display());
        return Ok(ExitCode::from(1));
    }
    for s in &symbols {
        let marker = if s.singleton { "." } else { "#" };
        let params = if s.params.is_empty() {
            String::new()
        } else {
            format!("({})", s.params.join(", "))
        };
        println!(
            "{:>5}  {:<8} {}{}{}",
            s.line, s.kind, marker, s.name, params
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Every call site named `name`, tiered against the query.
///
/// Files are reparsed rather than read from the stored call rows: the ladder
/// needs the file's assignments, which are deliberately not stored (DEC-012),
/// and reparsing means an edit since the last index is still tiered correctly.
fn gather_refs(
    tree: &Tree,
    store: &Store,
    root: &Path,
    root_str: &str,
    query: &crate::resolve::refs::Query,
    target: Option<&str>,
    // Keep the excluded sites too, so `--include-excluded` can show them.
    keep_all: bool,
) -> anyhow::Result<(
    Vec<crate::resolve::refs::Reference>,
    crate::resolve::refs::Counts,
)> {
    use crate::resolve::refs;
    let mut found = Vec::new();
    let mut counts = refs::Counts::default();
    for path in store.files_calling(root_str, &query.name)? {
        let Ok(bytes) = std::fs::read(root.join(&path)) else {
            continue;
        };
        let facts = extract::extract(&bytes);
        for call in facts.calls.iter().filter(|c| c.name == query.name) {
            let reference = refs::tier_call(tree, &facts, call, &path, query, target);
            counts.record(&reference);
            // Excluded sites are counted, not listed: the count is the product,
            // and the list would be the grep we are trying to beat. `keep_all`
            // is how `--include-excluded` makes the claim auditable.
            if keep_all || reference.tier != refs::Tier::Excluded {
                found.push(reference);
            }
        }
    }
    found.sort_by_key(refs::order);
    Ok((found, counts))
}

fn cmd_refs(out: Output, text: &str, include_excluded: bool) -> anyhow::Result<ExitCode> {
    use crate::resolve::refs;
    let query = refs::Query::parse(text);
    let root = scan::repo_root(Path::new("."))?;
    let root_str = root.to_string_lossy().into_owned();
    let store = open_store()?;

    // A bare name narrows nothing, so it keeps the whole-mention view —
    // definitions and constant references included, which a method-shaped
    // query has no use for.
    if query.owner.is_none() {
        return cmd_refs_by_name(out, &root, &root_str, &store, &query);
    }

    let tree = Tree::build(&store, &root_str)?;
    let (owner, definition) = refs::definition_of(&tree, &query);
    let (found, counts) = gather_refs(
        &tree,
        &store,
        &root,
        &root_str,
        &query,
        owner.as_deref(),
        include_excluded,
    )?;

    let answer = serde_json::json!({
        "query": text,
        "owner": owner,
        "method": query.name,
        "singleton": query.singleton,
        "definition": definition,
        "counts": counts,
        "references": found,
    });
    if out != Output::Text {
        emit_json(out, &answer)?;
        return Ok(exit_on(!found.is_empty()));
    }

    if owner.is_none() {
        println!(
            "no indexed constant named {}",
            query.owner.as_deref().unwrap_or("?")
        );
        return Ok(ExitCode::from(1));
    }
    for site in &definition {
        println!("{}:{}:{}  definition", site.path, site.line, site.col);
    }
    for reference in &found {
        println!(
            "{}:{}:{}  {:<10} {}",
            reference.path,
            reference.line,
            reference.col,
            format!("{:?}", reference.tier).to_lowercase(),
            reference.why,
        );
    }
    // The number a grep cannot produce, said out loud.
    if counts.excluded > 0 {
        println!(
            "\n{} confirmed, {} possible, {} excluded of {} same-name call sites",
            counts.confirmed,
            counts.possible,
            counts.excluded,
            counts.confirmed + counts.possible + counts.excluded,
        );
        // The three reasons are not equally strong, so they are not one number.
        println!(
            "  excluded: {} resolve to a different owner, {} define no such name, {} wrong arity",
            counts.excluded_different_owner, counts.excluded_no_such_method, counts.excluded_arity,
        );
    }
    Ok(exit_on(!found.is_empty()))
}

/// The whole-mention view for a bare name, with each call site's resolved owner
/// filled in where the ladder can reach it.
fn cmd_refs_by_name(
    out: Output,
    root: &Path,
    root_str: &str,
    store: &Store,
    query: &crate::resolve::refs::Query,
) -> anyhow::Result<ExitCode> {
    let mut rows = store.refs(root_str, &query.name)?;
    let has_calls = rows.iter().any(|row| row.role == "call");
    if has_calls {
        let tree = Tree::build(store, root_str)?;
        let (found, _) = gather_refs(&tree, store, root, root_str, query, None, false)?;
        // Match by position: one call site, one tiering.
        for row in rows.iter_mut().filter(|row| row.role == "call") {
            if let Some(reference) = found
                .iter()
                .find(|r| r.path == row.path && r.line == row.line && r.col == row.col)
            {
                row.tier = Some(format!("{:?}", reference.tier).to_lowercase());
                row.owner.clone_from(&reference.owner);
            }
        }
    }

    if emit_rows(out, &rows)? {
        return Ok(exit_on(!rows.is_empty()));
    }
    if rows.is_empty() {
        println!(
            "no mention of {} (indexed? try `trekr --index`)",
            query.name
        );
        return Ok(ExitCode::from(1));
    }
    for row in &rows {
        // The receiver shape is the disclosure: `implicit` is already resolved
        // to the enclosing class, `other` is residue. Nothing is dropped and
        // nothing is silently promoted.
        let detail = match (&row.owner, &row.kind, &row.recv, &row.recv_text) {
            (Some(owner), _, _, _) => owner.clone(),
            (_, Some(kind), _, _) => kind.clone(),
            (_, _, Some(recv), Some(text)) => format!("{recv} {text}"),
            (_, _, Some(recv), None) => recv.clone(),
            _ => String::new(),
        };
        println!(
            "{}:{}:{}  {:<11} {}",
            row.path, row.line, row.col, row.role, detail
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// The checkout containing `path`, and its assembled namespace.
///
/// The unit is the **file's own** repository, not the process's directory. A
/// question about a position is a question about that file, and an agent asks
/// it from wherever it happens to be standing — which is routinely another
/// repo, or another language's repo entirely.
///
/// The tree is rebuilt from SQL every invocation. PLAN §4 chose that over
/// incremental machinery, and the measurement in docs/ARCHITECTURE.md is why it
/// stays chosen.
fn tree_for(path: &Path, pinned: Option<&Path>) -> anyhow::Result<(PathBuf, Store, Tree)> {
    let store = open_store()?;
    let root = match pinned {
        Some(root) => std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf()),
        None => checkout_for(&store, path)?,
    };
    let tree = Tree::build(&store, &root.to_string_lossy())?;
    Ok((root, store, tree))
}

/// The checkout a path belongs to: its git repository, or failing that the
/// indexed root that contains it.
///
/// The second case is a gem. Gems are indexed per directory and are not git
/// repositories (DEC-001 governs what may be *indexed*, not what may be asked
/// about), so without this a question about a position in gem source — the
/// position an agent reaches one step after following a definition — could
/// not be answered at all.
fn checkout_for(store: &Store, path: &Path) -> anyhow::Result<PathBuf> {
    if let Ok(root) = scan::repo_root(path) {
        return Ok(root);
    }
    let absolute = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    match store.checkout_containing(&absolute.to_string_lossy())? {
        // A gem, and a gem on its own is a tree of one gem plus core — so
        // answer from an app whose bundle has the rest of it (DEC-029). With
        // no such app the gem is still its own context, and the answer says so
        // rather than quietly degrading.
        Some(gem) => Ok(PathBuf::from(store.app_for_gem(&gem)?.unwrap_or(gem))),
        // Neither: report git's own complaint, which names the real problem.
        None => scan::repo_root(path),
    }
}

/// The checkout we are standing in — for the queries that ask about a name
/// rather than a position, where "here" is the only checkout meant.
fn tree_here() -> anyhow::Result<(PathBuf, Store, Tree)> {
    tree_for(Path::new("."), None)
}

fn cmd_def(
    out: Output,
    spec: &str,
    explain: bool,
    pinned: Option<&Path>,
) -> anyhow::Result<ExitCode> {
    let spec = position::Spec::parse(spec)
        .ok_or_else(|| anyhow::anyhow!("expected FILE:LINE:COL, got `{spec}`"))?;
    let source = std::fs::read(&spec.path)?;
    let Some(under) = position::at(&source, spec.line, spec.col) else {
        return report(
            out,
            serde_json::json!({
                "query": format!("{}:{}:{}", spec.path, spec.line, spec.col),
                "status": "residue",
                "confidence": 0.0,
                "reason": "no name at this position",
            }),
            false,
            "nothing at that position",
        );
    };

    let query = format!("{}:{}:{}", spec.path, spec.line, spec.col);
    // Which checkout's assembled namespace answered. It is only ever a
    // surprise for a position inside a gem, which is answered from an app that
    // resolves it — and an answer that depends on which app must say which.
    let mut context: Option<String> = None;
    let answer = match under {
        // The cursor is on the declaration itself. Ruby has no indirection to
        // follow here, so the honest answer is "you are already there".
        position::Under::Definition(def) => serde_json::json!({
            "query": query,
            "under": "definition",
            "name": def.name,
            "status": "resolved",
            "confidence": 1.0,
            "resolved_via": "definition",
            "sites": [{
                "path": spec.path, "line": def.pos.line,
                "col": def.pos.col, "kind": def.kind.as_str(),
            }],
        }),
        position::Under::Constant(reference) => {
            let (root, _, tree) = tree_for(Path::new(&spec.path), pinned)?;
            context = Some(root.to_string_lossy().into_owned());
            let resolution = tree.resolve(&reference.name, &reference.nesting);
            let mut value = serde_json::to_value(&resolution)?;
            let object = value.as_object_mut().expect("resolution is an object");
            object.insert("query".into(), query.clone().into());
            object.insert("under".into(), "constant".into());
            object.insert("name".into(), reference.name.clone().into());
            if resolution.status == Status::Residue {
                object.insert(
                    "reason".into(),
                    "no indexed constant by that name; it may belong to a gem \
                     or be defined at runtime"
                        .into(),
                );
            }
            value
        }
        position::Under::Call(call) => {
            let (root, _, tree) = tree_for(Path::new(&spec.path), pinned)?;
            context = Some(root.to_string_lossy().into_owned());
            let relative = std::fs::canonicalize(&spec.path)
                .ok()
                .and_then(|abs| abs.strip_prefix(&root).ok().map(Path::to_path_buf))
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| spec.path.clone());
            let facts = crate::extract::extract(&source);
            let answer = crate::resolve::method_at(&tree, &facts, &call, &relative);
            let mut value = serde_json::to_value(&answer)?;
            let object = value.as_object_mut().expect("answer is an object");
            object.insert("query".into(), query.clone().into());
            object.insert("under".into(), "call".into());
            object.insert("name".into(), call.name.clone().into());
            if let Some(text) = &call.recv_text {
                object.insert("receiver_text".into(), text.clone().into());
            }
            value
        }
    };

    // Ambiguous is an answer with competitors, not a failure to answer: exit 0
    // like any other match, and let the status and confidence say the rest.
    let mut answer = answer;
    if let (Some(object), Some(context)) = (answer.as_object_mut(), context) {
        object.insert("context".into(), context.into());
    }
    let resolved = answer["status"] == "resolved" || answer["status"] == "ambiguous";
    let text = match answer["sites"].as_array().and_then(|s| s.first()) {
        Some(site) => format!(
            "{}:{}:{}  {}",
            site["path"].as_str().unwrap_or_default(),
            site["line"],
            site["col"],
            answer["fqn"]
                .as_str()
                .unwrap_or(answer["name"].as_str().unwrap_or_default()),
        ),
        // Resolved with nowhere to point is a real answer, not a failure: a
        // namespace Rails' autoloader invents from a directory exists and no
        // line of code declares it.
        None if resolved => format!(
            "{}  (namespace with no declaration)",
            answer["fqn"]
                .as_str()
                .unwrap_or(answer["name"].as_str().unwrap_or("?")),
        ),
        None => format!(
            "{}  {}",
            answer["name"].as_str().unwrap_or("?"),
            answer["reason"].as_str().unwrap_or("unresolved"),
        ),
    };
    let text = if explain && out == Output::Text {
        format!("{text}\n{}", explanation(&answer))
    } else {
        text
    };
    report(out, answer, resolved, &text)
}

/// The disclosure `--json` already carries, laid out for a reader.
///
/// Every line is a fact the answer states; nothing here is computed a second
/// time, so the two surfaces cannot drift apart.
fn explanation(answer: &serde_json::Value) -> String {
    let mut out = Vec::new();
    let field = |key: &str| answer[key].as_str().map(str::to_string);

    let mut how = format!("  status      {}", field("status").unwrap_or_default());
    if let Some(confidence) = answer["confidence"].as_f64() {
        how.push_str(&format!(" · confidence {confidence}"));
    }
    out.push(how);
    if let Some(via) = field("resolved_via") {
        out.push(format!("  via         {via}"));
    }
    if let Some(context) = field("context") {
        out.push(format!("  context     {context}"));
    }
    if let Some(receiver) = field("receiver") {
        let typed = field("receiver_type")
            .map(|t| format!(" → {t}"))
            .unwrap_or_default();
        out.push(format!("  receiver    {receiver}{typed}"));
    }
    if let Some(owner) = field("owner") {
        out.push(format!("  owner       {owner}"));
    }
    if let Some(agreement) = field("agreement") {
        out.push(format!("  agreement   {agreement}"));
    }
    if let Some(unseen) = answer["unresolved_ancestors"].as_array()
        && !unseen.is_empty()
    {
        // A "not found" is only as trustworthy as this list is short.
        let names: Vec<&str> = unseen.iter().filter_map(|a| a.as_str()).collect();
        out.push(format!(
            "  unseen      {} ancestors: {}",
            names.len(),
            names.join(", ")
        ));
    }
    if let Some(reason) = field("reason") {
        out.push(format!("  reason      {reason}"));
    }
    if let Some(candidates) = answer["candidates"].as_array()
        && !candidates.is_empty()
    {
        out.push(format!("  candidates  {} ranked:", candidates.len()));
        for (rank, candidate) in candidates.iter().enumerate() {
            let site = &candidate["site"];
            out.push(format!(
                "    {}. {}  {}:{}  — {}",
                rank + 1,
                candidate["owner"].as_str().unwrap_or("?"),
                site["path"].as_str().unwrap_or_default(),
                site["line"],
                candidate["why"].as_str().unwrap_or_default(),
            ));
        }
    }
    out.join("\n")
}

fn cmd_ancestors(out: Output, name: &str) -> anyhow::Result<ExitCode> {
    let (_, _, tree) = tree_here()?;
    let resolution = tree.resolve(name, &[]);
    let Some(fqn) = resolution.fqn.clone() else {
        return report(
            out,
            serde_json::json!({
                "name": name,
                "status": "residue",
                "confidence": 0.0,
                "scopes_tried": resolution.scopes_tried,
            }),
            false,
            &format!("no indexed constant named {name}"),
        );
    };
    let chain = tree.ancestors(&fqn);
    let text = chain.chain.join("\n");
    report(
        out,
        serde_json::json!({
            "name": name,
            "fqn": fqn,
            "status": "resolved",
            "ancestors": chain.chain,
            "unresolved": chain.unresolved,
        }),
        true,
        &text,
    )
}

/// One answer, in whichever shape the caller asked for.
fn report(
    out: Output,
    value: serde_json::Value,
    matched: bool,
    text: &str,
) -> anyhow::Result<ExitCode> {
    match out {
        Output::Text => println!("{text}"),
        _ => emit_json(out, &value)?,
    }
    Ok(exit_on(matched))
}

fn cmd_drop(out: Output, path: &Path) -> anyhow::Result<ExitCode> {
    let root = scan::repo_root(path)?;
    let root_str = root.to_string_lossy().into_owned();
    let dropped = open_store()?.drop_checkout(&root_str)?;

    match out {
        Output::Text if dropped == 0 => println!("{root_str} was not indexed"),
        Output::Text => println!("dropped {root_str}"),
        _ => emit_json(
            out,
            &serde_json::json!({ "repo": root_str, "dropped": dropped > 0 }),
        )?,
    }
    Ok(exit_on(dropped > 0))
}

/// 0 when something happened, 1 when nothing did — so a script can branch on it.
fn exit_on(happened: bool) -> ExitCode {
    if happened {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// What the resident front has actually been asked, from its own log.
///
/// The log was written in session 11 to debug a defect; this is the other half
/// of why it exists — which of the nine operations agents really call, how
/// often the answer is empty, and what it costs. A summary command rather than
/// a one-off script, so the answer regenerates itself as usage accumulates.
fn cmd_usage(out: Output) -> anyhow::Result<ExitCode> {
    let Some(path) = crate::serve::log::Log::where_to_look() else {
        anyhow::bail!("logging is off ($TREKR_LOG), so there is nothing to summarize");
    };
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;

    let mut sessions = 0usize;
    let mut retirements = 0usize;
    // The first request of a session pays for a cold page cache and a tree
    // build; every one after it does not. Blending them makes the headline a
    // measure of the disk, which no amount of work on trekr will improve —
    // measured at 450 ms first and 0.58 ms warm in the same session.
    let mut session_had_request = false;
    let mut first = String::new();
    let mut last = String::new();
    let mut per_op: HashMap<String, OpUsage> = HashMap::new();
    for line in text.lines() {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if let Some(ts) = event.get("ts").and_then(|t| t.as_str()) {
            if first.is_empty() {
                first = ts.to_string();
            }
            last = ts.to_string();
        }
        match event.get("event").and_then(|e| e.as_str()) {
            Some("start") => {
                sessions += 1;
                session_had_request = false;
            }
            Some("retire") => retirements += 1,
            Some("request") => {
                let Some(op) = event.get("op").and_then(|o| o.as_str()) else {
                    continue;
                };
                let usage = per_op.entry(op.to_string()).or_default();
                usage.calls += 1;
                let first_of_session = !session_had_request;
                session_had_request = true;
                match event.get("answered").and_then(serde_json::Value::as_u64) {
                    Some(0) => usage.empty += 1,
                    Some(_) => usage.answered += 1,
                    None => {}
                }
                if event.get("status").and_then(|s| s.as_str()) == Some("error") {
                    usage.errors += 1;
                }
                if let Some(ms) = event.get("ms").and_then(serde_json::Value::as_f64) {
                    if first_of_session {
                        usage.cold.push(ms);
                    } else {
                        usage.timings.push(ms);
                    }
                }
            }
            _ => {}
        }
    }

    let mut rows: Vec<UsageRow> = per_op
        .into_iter()
        .map(|(op, usage)| usage.finish(op))
        .collect();
    // Most-used first: the ranking is the point of the report.
    rows.sort_by(|a, b| b.calls.cmp(&a.calls).then_with(|| a.op.cmp(&b.op)));
    let total: usize = rows.iter().map(|r| r.calls).sum();

    if emit_rows(out, &rows)? {
        return Ok(exit_on(total > 0));
    }
    if rows.is_empty() {
        println!("no requests logged yet in {}", path.display());
        return Ok(ExitCode::from(1));
    }
    let retired = match retirements {
        0 => String::new(),
        n => format!(", {n} retired on a newer binary"),
    };
    println!(
        "{total} requests over {sessions} session(s){retired}, {} — {}\n",
        &first[..first.len().min(10)],
        &last[..last.len().min(10)]
    );
    println!(
        "{:<32}{:>6}{:>9}{:>9}{:>8}{:>10}",
        "operation", "calls", "answered", "median", "p90", "cold 1st"
    );
    for row in &rows {
        println!(
            "{:<32}{:>6}{:>8.0}%{:>9.1}{:>8.1}{:>10}",
            row.op,
            row.calls,
            100.0 * row.answered as f64 / row.calls.max(1) as f64,
            row.median_ms,
            row.p90_ms,
            match row.cold_first_calls {
                0 => "—".to_string(),
                n => format!("{:.0} (n={n})", row.cold_first_ms),
            },
        );
    }
    Ok(ExitCode::SUCCESS)
}

#[derive(Default)]
struct OpUsage {
    calls: usize,
    answered: usize,
    empty: usize,
    errors: usize,
    timings: Vec<f64>,
    /// Requests that opened a session, and so paid for the cold cache.
    cold: Vec<f64>,
}

impl OpUsage {
    fn finish(mut self, op: String) -> UsageRow {
        self.timings
            .sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        self.cold
            .sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        UsageRow {
            op,
            calls: self.calls,
            answered: self.answered,
            empty: self.empty,
            errors: self.errors,
            // Rounded to the precision a handful of samples actually carries.
            median_ms: round1(percentile(&self.timings, 0.5)),
            p90_ms: round1(percentile(&self.timings, 0.9)),
            cold_first_ms: round1(percentile(&self.cold, 0.5)),
            cold_first_calls: self.cold.len(),
        }
    }
}

#[derive(serde::Serialize)]
struct UsageRow {
    op: String,
    calls: usize,
    answered: usize,
    empty: usize,
    errors: usize,
    median_ms: f64,
    p90_ms: f64,
    /// Median of the requests that opened a session — the cold-cache cost,
    /// reported apart because it measures the disk rather than the engine.
    cold_first_ms: f64,
    cold_first_calls: usize,
}

fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let index = ((sorted.len() - 1) as f64 * q).round() as usize;
    sorted[index]
}

fn round1(ms: f64) -> f64 {
    (ms * 10.0).round() / 10.0
}
