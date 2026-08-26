//! The command line. Every command that prints anything honors `--json` and
//! `--ndjson`, because the primary consumer is an agent, not a person.
//!
//! Operations are flags rather than subcommands (rq's convention): no word is
//! reserved, and the default action stays free for the query verbs the resolve
//! layer will add.

pub(crate) mod position;
mod profile;

use crate::core::Oid;
use crate::core::paths;
use crate::store::Store;
use crate::tree::{Status, Tree};
use crate::{extract, scan};
use anyhow::Context;
use clap::{CommandFactory, Parser};
use clap_complete::Shell;
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
    /// What to look up, dispatched on its shape: `FILE:LINE:COL` and
    /// `FILE:LINE` ask what is at a position, `Owner#method` or `Owner.method`
    /// asks about a method, and a bare `Constant` asks about a class or module.
    ///
    /// Sugar over the flags, never a replacement: every shape it reaches is
    /// still addressable explicitly, so a script never has to depend on
    /// inference (DEC-036).
    #[arg(value_name = "INPUT")]
    input: Option<String>,

    /// Find definitions in these files or directories that nothing appears to
    /// use — candidates for deletion or inlining, graded, never asserted.
    #[arg(long, value_name = "PATH", num_args = 1..)]
    dead: Vec<PathBuf>,

    /// Index the checkout containing this path (default: the current directory).
    #[arg(long, value_name = "PATH", num_args = 0..=1, default_missing_value = ".")]
    index: Option<PathBuf>,

    /// Report what is indexed, per checkout, with the shared blob totals.
    #[arg(long, conflicts_with_all = ["index", "symbols", "drop"])]
    status: bool,

    /// Summarize what `--lsp` has been asked, from its own log: which
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
    lsp: bool,

    /// Report where the time went, on stderr. For `--index`, the phases of the
    /// index; for a query, the phases of the tree build behind it. With
    /// `--lsp`, logs the wire-level params of every request too.
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

    /// Print a shell completion script (bash, zsh, fish, elvish, powershell).
    #[arg(long, value_name = "SHELL")]
    completions: Option<Shell>,
}

#[derive(Clone, Copy, PartialEq)]
enum Output {
    Text,
    Json,
    Ndjson,
}

pub fn run() -> ExitCode {
    let cli = Cli::parse();

    // Before any store or git work: generating a completion script must not
    // need a checkout, and every other command refuses a non-repo with exit 2.
    if let Some(shell) = cli.completions {
        clap_complete::generate(shell, &mut Cli::command(), "trekr", &mut std::io::stdout());
        return ExitCode::SUCCESS;
    }

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

    let result = if cli.lsp {
        crate::serve::run(cli.profile).map(|()| ExitCode::SUCCESS)
    } else if let Some(path) = &cli.index {
        cmd_index(out, path, cli.jobs, cli.profile, !cli.no_gems)
    } else if let Some(path) = &cli.symbols {
        cmd_symbols(out, path)
    } else if let Some(name) = &cli.refs {
        cmd_refs(out, name, cli.include_excluded)
    } else if let Some(spec) = &cli.def {
        cmd_def(out, spec, cli.explain, cli.context.as_deref())
    } else if !cli.dead.is_empty() {
        cmd_dead(out, &cli.dead)
    } else if let Some(name) = &cli.ancestors {
        cmd_ancestors(out, name)
    } else if let Some(path) = &cli.drop {
        cmd_drop(out, path)
    } else if cli.status {
        cmd_status(out)
    } else if cli.usage {
        cmd_usage(out)
    } else if let Some(input) = &cli.input {
        cmd_bare(out, input, cli.explain, cli.context.as_deref())
    } else {
        eprintln!(
            "trekr: nothing to do (try `trekr Widget#save`, `trekr app.rb:42`, \
             or --index, --status, --usage)"
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
    git_state: i64,
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
        store.write(root_str, files, facts, git_state)
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
        let counts = index_files(store, &gem_root, &root_str, &files, 0, pool, profile)?;
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
    // Sampled *before* the scan, deliberately. A fingerprint taken afterwards
    // would cover edits this index never saw and the next query would call them
    // fresh; taken first, the worst case is a probe that reports stale when it
    // is not, which costs one re-read and never a wrong answer.
    let git_state = scan::git_fingerprint(&root).unwrap_or(0);
    let files = profile::timed(&mut profile, "scan", || scan::scan(&root))?;

    let mut store = open_store()?;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(jobs).build()?;
    let counts = index_files(
        &mut store,
        &root,
        &root_str,
        &files,
        git_state,
        &pool,
        &mut profile,
    )?;

    let gems = if with_gems {
        index_gems(&mut store, &root, &pool, &mut profile)?
    } else {
        GemReport::default()
    };

    // Only when something was actually read: statistics cost seconds, and a
    // reindex that parsed nothing has not changed the shape the planner cares
    // about.
    if counts.parsed > 0 || gems.indexed > 0 {
        profile::timed(&mut profile, "analyze", || {
            store.analyze();
            Ok::<(), anyhow::Error>(())
        })?;
    }

    match out {
        Output::Text => println!(
            "indexed {} — {} files, {} blobs, {} parsed ({} defs, {} refs, {} calls)",
            paths::pretty(&root_str),
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
        println!(
            "{:>7} files  {:>7} blobs  {}",
            c.files,
            c.blobs,
            paths::pretty(&c.repo)
        );
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
        println!(
            "no definitions in {}",
            paths::pretty(&path.to_string_lossy())
        );
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

/// What a name *is*, in one answer: where it is defined, what kind of location
/// that is, and how many call sites can actually reach it.
///
/// The reason the bare grammar earns its keep rather than aliasing a flag. Asked
/// about `Widget#save` a person wants the definition **and** whether anything
/// calls it; asked about `Widget` they want the definition **and** what it
/// inherits. Two commands' worth of answer, which is what makes this worth a
/// shape rather than a synonym.
fn cmd_card(out: Output, text: &str) -> anyhow::Result<ExitCode> {
    use crate::resolve::refs;
    let query = refs::Query::parse(text);
    let root = scan::repo_root(Path::new("."))?;
    let root_str = root.to_string_lossy().into_owned();
    let store = open_store()?;
    if !store.has_checkout(&root_str)? {
        return not_indexed(out, &root);
    }
    let tree = Tree::build(&store, &root_str)?;

    // A constant: what it is, and what it inherits.
    if query.owner.is_none() {
        let resolution = tree.resolve(&query.name, &[]);
        let Some(fqn) = resolution.fqn.clone() else {
            return report(
                out,
                serde_json::json!({
                    "query": text,
                    "status": "residue",
                    "confidence": 0.0,
                    "scopes_tried": resolution.scopes_tried,
                }),
                false,
                &format!("no indexed constant named {}", query.name),
            );
        };
        let chain = tree.ancestors(&fqn);
        let sites = tree.sites(&fqn).to_vec();
        let text_out = card_text(&fqn, &sites, &chain.chain, None);
        return report(
            out,
            serde_json::json!({
                "query": text,
                "status": "resolved",
                "fqn": fqn,
                "kind": tree.kind_of(&fqn),
                "definition": sites,
                "ancestors": chain.chain,
                "unresolved_ancestors": chain.unresolved,
            }),
            true,
            &text_out,
        );
    }

    // A method: where it is, and who can reach it.
    let (owner, definition) = refs::definition_of(&tree, &query);
    let Some(owner) = owner else {
        return report(
            out,
            serde_json::json!({ "query": text, "status": "residue", "confidence": 0.0 }),
            false,
            &format!(
                "no indexed constant named {}",
                query.owner.unwrap_or_default()
            ),
        );
    };
    let (_, counts) = gather_refs(&tree, &store, &root, &root_str, &query, Some(&owner), false)?;
    let kind = tree
        .lookup(&owner, query.singleton, &query.name)
        .map(|method| method.kind());
    let shown = match query.singleton {
        true => format!("{owner}.{}", query.name),
        false => format!("{owner}#{}", query.name),
    };
    let text_out = card_text(&shown, &definition, &[], Some(&counts));
    report(
        out,
        serde_json::json!({
            "query": text,
            "status": "resolved",
            "owner": owner,
            "method": query.name,
            "singleton": query.singleton,
            "kind": kind,
            "definition": definition,
            "counts": counts,
        }),
        !definition.is_empty(),
        &text_out,
    )
}

/// The card as a person reads it: the definition, then the one line of context
/// that shape earned — reference tiers for a method, the chain for a constant.
fn card_text(
    name: &str,
    sites: &[crate::tree::Site],
    ancestors: &[String],
    counts: Option<&crate::resolve::refs::Counts>,
) -> String {
    let mut out = vec![name.to_string()];
    for site in sites {
        out.push(format!(
            "  {}:{}:{}",
            paths::pretty(&site.path),
            site.line,
            site.col
        ));
    }
    if let Some(counts) = counts {
        out.push(format!(
            "  {} confirmed · {} possible · {} excluded",
            counts.confirmed, counts.possible, counts.excluded
        ));
    }
    // The chain contains the thing itself, and not always first: a prepended
    // module precedes it. Filter by name rather than trusting the position.
    let rest: Vec<&str> = ancestors
        .iter()
        .map(String::as_str)
        .filter(|entry| *entry != name)
        .collect();
    if !rest.is_empty() {
        let shown: Vec<&str> = rest.iter().take(5).copied().collect();
        let more = rest.len().saturating_sub(5);
        let tail = if more > 0 {
            format!(" (+{more} more)")
        } else {
            String::new()
        };
        out.push(format!("  < {}{tail}", shown.join(", ")));
    }
    out.join("\n")
}

fn cmd_refs(out: Output, text: &str, include_excluded: bool) -> anyhow::Result<ExitCode> {
    use crate::resolve::refs;
    let query = refs::Query::parse(text);
    let root = scan::repo_root(Path::new("."))?;
    let root_str = root.to_string_lossy().into_owned();
    let store = open_store()?;
    if !store.has_checkout(&root_str)? {
        return not_indexed(out, &root);
    }

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
        println!(
            "{}:{}:{}  definition",
            paths::pretty(&site.path),
            site.line,
            site.col
        );
    }
    for reference in &found {
        println!(
            "{}:{}:{}  {:<10} {}",
            paths::pretty(&reference.path),
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
            paths::pretty(&row.path),
            row.line,
            row.col,
            row.role,
            detail
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
/// The checkout a query is about, and an open store — without building the tree.
///
/// Split out because a refresh has to happen *between* those two steps: the
/// tree is assembled from the store, so refreshing after building it would
/// answer from facts one edit out of date.
fn checkout_for_query(path: &Path, pinned: Option<&Path>) -> anyhow::Result<(PathBuf, Store)> {
    let store = open_store()?;
    let root = match pinned {
        Some(root) => std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf()),
        None => checkout_for(&store, path)?,
    };
    Ok((root, store))
}

fn tree_for(path: &Path, pinned: Option<&Path>) -> anyhow::Result<(PathBuf, Store, Tree)> {
    let store = open_store()?;
    let root = match pinned {
        Some(root) => std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf()),
        None => checkout_for(&store, path)?,
    };
    let tree = Tree::build(&store, &root.to_string_lossy())?;
    Ok((root, store, tree))
}

/// Bring the file being asked about up to date, if git says anything moved.
///
/// DEC-035's policy in one function: an O(1) probe, then a bounded refresh of
/// the queried file alone. Returns what to disclose — the caller must say when
/// the rest of the index may lag, because an answer that quietly rests on stale
/// facts is the failure this whole mechanism exists to prevent.
fn refresh_for_query(store: &mut Store, root: &Path, file: &Path) -> Option<serde_json::Value> {
    let root_str = root.to_string_lossy().into_owned();
    let current = scan::git_fingerprint(root)?;
    let recorded = store.git_state(&root_str).ok().flatten()?;
    // A gem, or a checkout indexed before this column existed. Nothing to
    // compare, and claiming staleness would be as wrong as claiming freshness.
    if recorded == 0 || recorded == current {
        return None;
    }

    let absolute = std::fs::canonicalize(file).ok()?;
    let relative = absolute
        .strip_prefix(root)
        .ok()?
        .to_string_lossy()
        .into_owned();
    let bytes = std::fs::read(&absolute).ok()?;
    let oid = scan::hash_blob(&bytes);
    // Parse only when this blob is genuinely new — the common case after a
    // branch switch is bytes the store has seen before, which cost one hash.
    let known = store
        .known(&HashSet::from([oid.clone()]))
        .map(|found| found.contains(&oid))
        .unwrap_or(false);
    let facts = (!known).then(|| crate::extract::extract(&bytes));
    let changed = store
        .refresh_file(&root_str, &relative, &oid, facts.as_ref())
        .unwrap_or(false);

    Some(serde_json::json!({
        "stale": true,
        "refreshed": changed.then(|| relative.clone()),
        "hint": format!("trekr --index {}", paths::pretty(&root_str)),
    }))
}

/// `trekr <input>` — one argument, dispatched on its shape (DEC-036).
///
/// The shapes are disjoint by construction rather than by preference order: a
/// position has digits after a colon, a method has `#` or `.`, and a constant
/// begins with a capital. Anything else is refused with the shapes spelled out,
/// because guessing at a fourth meaning is how a grammar starts lying.
///
/// **Not an `rq` clone.** "Where is this name defined", across languages, is
/// rq's question. A bare constant here answers the *Ruby* question — what it
/// is, what it inherits, how many call sites can reach it — and the skill says
/// so, so an agent does not reach for the wrong tool and conclude one of them
/// is broken.
fn cmd_bare(
    out: Output,
    input: &str,
    explain: bool,
    context: Option<&Path>,
) -> anyhow::Result<ExitCode> {
    // A position: the last field is a line number, so `Spec::parse` accepts it.
    // Checked first because a Windows-ish path could contain anything else.
    if position::Spec::parse(input).is_some() {
        return cmd_def(out, input, explain, context);
    }
    // A method: `Owner#method` or `Owner.method`, which `--refs` already parses
    // and which is the one shape with a genuinely richer answer than a flag.
    if input.contains('#') || (input.contains('.') && !input.contains('/')) {
        return cmd_card(out, input);
    }
    if input.starts_with(|c: char| c.is_ascii_uppercase()) {
        return cmd_card(out, input);
    }
    eprintln!(
        "trekr: cannot tell what `{input}` is. Expected FILE:LINE[:COL], \
         Owner#method, Owner.method, or a Constant."
    );
    Ok(ExitCode::from(2))
}

/// Definitions in scope that nothing appears to use (DEC-038).
///
/// Two passes, because the cheap one settles most of it. A name with hundreds
/// of call sites is not a candidate and must not cost a receiver-narrowed
/// search to establish that; the few that survive get the expensive question
/// asked properly.
///
/// Scope is the argument, evidence is the **whole index** — a method used once
/// from outside the scope is not a candidate, and a scope-local search would
/// say it is.
fn cmd_dead(out: Output, paths: &[PathBuf]) -> anyhow::Result<ExitCode> {
    use crate::resolve::refs;

    let root = scan::repo_root(
        paths
            .first()
            .map(PathBuf::as_path)
            .unwrap_or(Path::new(".")),
    )?;
    let root_str = root.to_string_lossy().into_owned();
    let store = open_store()?;
    if !store.has_checkout(&root_str)? {
        return not_indexed(out, &root);
    }

    let files = ruby_files(paths);
    let mut defined: Vec<(String, crate::core::Def, String)> = Vec::new();
    for file in &files {
        let Ok(source) = std::fs::read(file) else {
            continue;
        };
        let facts = extract::extract(&source);
        // A dynamic-dispatch marker anywhere in the file lowers confidence for
        // everything in it: these are the shapes that make "no references" a
        // weaker statement, and they are file-wide by nature.
        let risky = dynamic_markers(&source);
        let shown = file.to_string_lossy().into_owned();
        for def in facts.defs {
            if def.kind != crate::core::Kind::Method {
                continue;
            }
            // A schema column is not dead because nothing calls it; that is a
            // fact about the database. Same for anything a macro declared —
            // deleting the method means editing the macro, which is a different
            // question than this one.
            if def.via.is_some() {
                continue;
            }
            defined.push((shown.clone(), def, risky.clone()));
        }
    }

    let names: Vec<String> = defined.iter().map(|(_, d, _)| d.name.clone()).collect();
    let mentions = store.mention_counts(&names)?;

    // The expensive pass, only for names the cheap one could not clear.
    let tree = Tree::build(&store, &root_str)?;
    let mut rows: Vec<serde_json::Value> = Vec::new();
    for (file, def, risky) in &defined {
        let (written, _) = mentions.get(&def.name).copied().unwrap_or((0, 0));
        if written > 8 {
            continue; // plainly used; not worth a narrowed search
        }
        let owner = def.nesting.first().cloned().unwrap_or_default();
        let query = refs::Query {
            owner: Some(owner.clone()),
            singleton: def.singleton,
            name: def.name.clone(),
        };
        let (found, counts) =
            gather_refs(&tree, &store, &root, &root_str, &query, Some(&owner), false)
                .unwrap_or_default();
        // A symbol reference is tiered `possible`, so it is *inside*
        // `counts.possible` — subtract it to ask what is written as a call.
        // Without this, `convention-only` can never fire and a method reached
        // only by `after_create :thing` reads as an ordinary single caller.
        let by_symbol = found
            .iter()
            .filter(|reference| reference.receiver == "symbol")
            .count();
        let written_live = (counts.confirmed + counts.possible).saturating_sub(by_symbol);
        let tier = match (written_live, by_symbol) {
            (0, 0) => "unreferenced",
            (0, _) => "convention-only",
            (1, _) => "single-caller",
            _ => continue,
        };
        rows.push(serde_json::json!({
            "name": def.name,
            "owner": owner,
            "file": file,
            "line": def.pos.line,
            "tier": tier,
            "confirmed": counts.confirmed,
            "possible": counts.possible,
            "symbol_refs": by_symbol,
            "mentions_by_name": written,
            "confidence": if risky.is_empty() { "clear" } else { "lower" },
            "caveat": risky,
        }));
    }

    let found = !rows.is_empty();
    if out != Output::Text {
        emit_json(
            out,
            &serde_json::json!({ "scope": files.len(), "candidates": rows }),
        )?;
        return Ok(exit_on(found));
    }
    for row in &rows {
        println!(
            "{:<16} {}:{}  {}{}",
            row["tier"].as_str().unwrap_or_default(),
            paths::pretty(row["file"].as_str().unwrap_or_default()),
            row["line"],
            row["name"].as_str().unwrap_or_default(),
            match row["caveat"].as_str().unwrap_or_default() {
                "" => String::new(),
                why => format!("   (lower confidence: {why})"),
            }
        );
    }
    if !found {
        println!("no candidates in {} file(s)", files.len());
    }
    Ok(exit_on(found))
}

/// Ruby files under these paths, following directories one level of recursion.
fn ruby_files(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for path in paths {
        if path.is_file() {
            found.push(path.clone());
            continue;
        }
        let Ok(walk) = std::fs::read_dir(path) else {
            continue;
        };
        for entry in walk.flatten() {
            let child = entry.path();
            if child.is_dir() {
                found.extend(ruby_files(&[child]));
            } else if child.extension().is_some_and(|e| e == "rb") {
                found.push(child);
            }
        }
    }
    found
}

/// Shapes that make "no references found" a weaker statement, named so the
/// answer can say which one it saw rather than hedging in general.
fn dynamic_markers(source: &[u8]) -> String {
    let text = String::from_utf8_lossy(source);
    let mut seen: Vec<&str> = Vec::new();
    for marker in [
        "send(",
        "public_send(",
        "method_missing",
        "define_method",
        "const_get",
    ] {
        if text.contains(marker) {
            seen.push(marker.trim_end_matches('('));
        }
    }
    seen.join(", ")
}

/// A checkout nobody has indexed, when a query needs one.
///
/// Worth its own answer because the alternative is a lie by omission: an empty
/// tree resolves nothing, so the query came back `residue` — "no indexed
/// constant by that name" — which reads as *we looked and Ruby does not have
/// it* when the truth is *nobody has looked yet*. One is a finding about the
/// code, the other is a setup step, and they call for opposite reactions.
fn not_indexed(out: Output, root: &Path) -> anyhow::Result<ExitCode> {
    let root = root.to_string_lossy().to_string();
    let hint = format!("trekr --index {}", paths::pretty(&root));
    match out {
        Output::Text => {
            eprintln!(
                "trekr: {} is not indexed — run: {hint}",
                paths::pretty(&root)
            )
        }
        _ => emit_json(
            out,
            &serde_json::json!({
                "status": "not_indexed",
                "repo": root,
                "reason": "this checkout has never been indexed, so there is nothing to answer from",
                "hint": hint,
            }),
        )?,
    }
    // Exit 2, not 1: `1` is this tool's "a definitive nothing" and would tell a
    // script the question was answered. It was not asked.
    Ok(ExitCode::from(2))
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
    let facts = crate::extract::extract(&source);
    let snapped = position::at_or_snap(&facts, spec.line, spec.col);
    let Some((under, snapped)) = snapped else {
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
    let mut freshness: Option<serde_json::Value> = None;
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
            let (root, mut store) = checkout_for_query(Path::new(&spec.path), pinned)?;
            if !store.has_checkout(&root.to_string_lossy())? {
                return not_indexed(out, &root);
            }
            // Refresh before the tree is built, so the tree sees the new facts.
            freshness = refresh_for_query(&mut store, &root, Path::new(&spec.path));
            let tree = Tree::build(&store, &root.to_string_lossy())?;
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
            let (root, mut store) = checkout_for_query(Path::new(&spec.path), pinned)?;
            if !store.has_checkout(&root.to_string_lossy())? {
                return not_indexed(out, &root);
            }
            // Refresh before the tree is built, so the tree sees the new facts.
            freshness = refresh_for_query(&mut store, &root, Path::new(&spec.path));
            let tree = Tree::build(&store, &root.to_string_lossy())?;
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
    // The index may lag the working tree, and an answer resting on stale facts
    // has to say so rather than look confident.
    if let (Some(object), Some(freshness)) = (answer.as_object_mut(), &freshness) {
        object.insert("index".into(), freshness.clone());
    }
    // An answer about a name the caller did not type has to say so.
    if let (Some(object), Some(snapped)) = (answer.as_object_mut(), &snapped) {
        object.insert(
            "snapped_to".into(),
            serde_json::json!({
                "name": snapped.name,
                "col": snapped.col,
                "alternatives": snapped
                    .alternatives
                    .iter()
                    .map(|(name, col)| serde_json::json!({ "name": name, "col": col }))
                    .collect::<Vec<_>>(),
            }),
        );
    }
    if let (Output::Text, Some(freshness)) = (out, &freshness) {
        match freshness["refreshed"].as_str() {
            Some(file) => eprintln!("trekr: {file} changed since the index — re-read it"),
            None => eprintln!("trekr: the checkout moved since the index; other files may lag"),
        }
    }
    if let (Output::Text, Some(snapped)) = (out, &snapped) {
        let others = match snapped.alternatives.len() {
            0 => String::new(),
            n => format!(
                " ({n} other name{} on that line)",
                if n == 1 { "" } else { "s" }
            ),
        };
        eprintln!(
            "trekr: answering for `{}` at column {}{others}",
            snapped.name, snapped.col
        );
    }
    let resolved = answer["status"] == "resolved" || answer["status"] == "ambiguous";
    let text = match answer["sites"].as_array().and_then(|s| s.first()) {
        Some(site) => format!(
            "{}:{}:{}  {}",
            paths::pretty(site["path"].as_str().unwrap_or_default()),
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
        out.push(format!("  context     {}", paths::pretty(&context)));
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
    // Whether the location is the code or the line that declared the name.
    // Printed with the macro, because "declaration" alone tells a reader what
    // the answer is *not* without telling them what it is.
    if let Some(kind) = field("kind") {
        let by = field("defined_via")
            .map(|via| format!(" · {via}"))
            .unwrap_or_default();
        out.push(format!("  kind        {kind}{by}"));
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
                paths::pretty(site["path"].as_str().unwrap_or_default()),
                site["line"],
                candidate["why"].as_str().unwrap_or_default(),
            ));
        }
    }
    out.join("\n")
}

fn cmd_ancestors(out: Output, name: &str) -> anyhow::Result<ExitCode> {
    let (root, store, tree) = tree_here()?;
    if !store.has_checkout(&root.to_string_lossy())? {
        return not_indexed(out, &root);
    }
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
        Output::Text if dropped == 0 => {
            println!("{} was not indexed", paths::pretty(&root_str))
        }
        Output::Text => println!("dropped {}", paths::pretty(&root_str)),
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
        println!(
            "no requests logged yet in {}",
            paths::pretty(&path.to_string_lossy())
        );
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
