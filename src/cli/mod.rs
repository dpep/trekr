//! The command line. Every command that prints anything honors `--json` and
//! `--ndjson`, because the primary consumer is an agent, not a person.
//!
//! Operations are flags rather than subcommands (rq's convention): no word is
//! reserved, and the default action stays free for the query verbs the resolve
//! layer will add.

mod failure;
pub(crate) mod position;
mod profile;

use failure::{Failure, Tag};

use crate::core::Oid;
use crate::core::paths;
use crate::store::Store;
use crate::tree::{Status, Tree};
use crate::{extract, scan};
use clap::{CommandFactory, Parser};
use clap_complete::Shell;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::io::Write;
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
        0   something was indexed, or a query matched\n  \
        1   nothing found: no match, nothing to collect. `status` says whether that\n      \
        is certain (no_such_method) or a residue that names what it could not see\n  \
        2   no answer yet: this checkout is not indexed (run --index)\n  \
        64  usage: the command line is wrong\n  \
        66  not_found, not_a_repo: a path it names is missing or not in a checkout\n  \
        69  git: git could not be run\n  \
        70  internal: a bug\n  \
        74  database, io: the index or a file could not be read or written\n\n\
        Under --json/--ndjson an error is one {\"error\", \"kind\", \"code\"} object on \
        stdout — a usage error too, wherever on the line --json is — and the message \
        is on stderr either way.\n\n\
        ENVIRONMENT\n  \
        TREKR_DB     the index (default ~/.local/share/trekr/trekr.db); its tree snapshots\n               \
        and Ruby core's files are kept beside it\n  \
        TREKR_USAGE  the usage-count file (default: beside the index), or `off`\n  \
        TREKR_JOBS   parse threads, as --jobs"
)]
struct Cli {
    /// What to look up, dispatched on its shape. `FILE:LINE:COL` and
    /// `FILE:LINE` ask what is at a position, as `--def` does. `Owner#method`
    /// or `Owner.method` answers with a card: where the method is defined and
    /// how many call sites reach it, tiered — a summary; `--refs` lists the
    /// sites. A bare `Constant` is a card too: where it is defined and what it
    /// inherits. `--usage` counts these as `def` and `card`.
    ///
    /// Sugar over the flags, never a replacement: every shape it reaches is
    /// still addressable explicitly, so a script never has to depend on
    /// inference (DEC-036).
    #[arg(value_name = "INPUT")]
    input: Option<String>,

    /// Find definitions in these files or directories that nothing appears to
    /// use — candidates for deletion or inlining, graded, never asserted.
    /// Each is in one tier, from the least evidence of use to the most:
    /// `unreferenced` (no call, symbol or `super` names it), `override` (none
    /// does, but it overrides an ancestor's method, so a call of that may run
    /// it), `convention-only` (named only by a symbol handed to a macro),
    /// `super-only` (reached only by `super` from its overrides), and
    /// `single-caller` (one call: the inlining candidate). Each is `clear`, or
    /// `lower` confidence when the file sends names dynamically, the one
    /// caller's receiver is untyped, or it overrides a method. One pass: a
    /// method whose only caller is itself a candidate is `single-caller`, and
    /// its reason says so.
    #[arg(long, value_name = "PATH", num_args = 1..)]
    dead: Vec<PathBuf>,

    /// Index the checkout containing this path (default: the current directory).
    #[arg(long, value_name = "PATH", num_args = 0..=1, default_missing_value = ".")]
    index: Option<PathBuf>,

    /// Report what is indexed: the checkout here and its gems, counted, with
    /// the shared blob totals. `--all` lists every checkout
    #[arg(long, conflicts_with_all = ["index", "symbols", "drop"])]
    status: bool,

    /// With `--status`: list every checkout on the machine, gems included
    #[arg(long, requires = "status", conflicts_with = "context")]
    all: bool,

    /// Which commands and editor features have been used, by whom (an agent,
    /// a person, an editor), how often they came back empty, and how slow.
    /// Counts only — no queries, paths, or repository names are kept.
    #[arg(long, conflicts_with_all = ["index", "symbols", "drop", "refs", "def"])]
    usage: bool,

    /// With `--usage`: only the last N days (default: all kept, 90).
    #[arg(long, value_name = "N", requires = "usage", value_parser = clap::value_parser!(u32).range(1..))]
    days: Option<u32>,

    /// With `--usage`: the editor's recent definitions and hovers that came
    /// back empty or unsure — file, position, the token and trekr's reason —
    /// read from `lsp.log`, which stays on this machine.
    #[arg(long, requires = "usage")]
    misses: bool,

    /// Outline one file's definitions, in the order they are written.
    #[arg(long, value_name = "FILE", conflicts_with_all = ["index", "drop"])]
    symbols: Option<PathBuf>,

    /// Every mention of a name in this checkout: definitions, constant
    /// references, and call sites. Name-level — not yet resolved.
    #[arg(long, value_name = "NAME", conflicts_with_all = ["index", "drop", "symbols"])]
    refs: Option<String>,

    /// What is the name at this position, and where is it defined? A column
    /// on no name — whitespace, a string, punctuation — answers for the
    /// nearest name on that line and says so (`snapped_to`); a bare
    /// `FILE:LINE` takes the line's first name
    #[arg(long, value_name = "FILE:LINE:COL", conflicts_with_all = ["index", "drop", "symbols", "refs"])]
    def: Option<String>,

    /// Answer as if asked from this checkout. For a name — `Owner#method`, a
    /// constant, `--refs`, `--ancestors` — the checkout the current directory
    /// is in otherwise. For a position, the one the path belongs to, which
    /// matters inside a **gem**: it is otherwise answered from whichever app
    /// most recently indexed it, a pick that is deterministic but moves as you
    /// work (DEC-029). Pin it when a measurement has to be reproducible. For
    /// `--status`, the checkout to report on.
    #[arg(long, value_name = "CHECKOUT")]
    context: Option<PathBuf>,

    /// The linearized ancestor chain of a class or module.
    #[arg(long, value_name = "NAME", conflicts_with_all = ["index", "drop", "symbols", "refs", "def"])]
    ancestors: Option<String>,

    /// Forget a checkout's file map (its blobs stay, for the worktrees that
    /// share them).
    #[arg(long, value_name = "PATH", num_args = 0..=1, default_missing_value = ".")]
    drop: Option<PathBuf>,

    /// Remove checkouts nothing will ask about again: gem versions no
    /// surviving project's bundle names, and projects whose root is gone.
    /// Their blobs go too, unless another checkout still maps them.
    #[arg(long, conflicts_with_all = ["index", "status", "symbols", "refs", "def", "ancestors", "drop", "lsp"])]
    gc: bool,

    /// With `--gc`: report what would be removed, and the space it would free,
    /// without removing it.
    #[arg(long, requires = "gc")]
    dry_run: bool,

    /// With `--gc`: spare anything an index saw more recently than this —
    /// `36h`, `7d`, `2w`, or `0` for everything collectable now.
    #[arg(long, value_name = "AGE", requires = "gc", default_value = "7d", value_parser = parse_age)]
    older_than: u64,

    /// With `--gc`: compact the database afterwards so the file shrinks.
    /// Seconds on a large store, holding the write lock.
    #[arg(long, requires = "gc", conflicts_with = "dry_run")]
    vacuum: bool,

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
    /// `--json` carries, rendered for a person. For a position: `--def
    /// FILE:LINE:COL`, or the bare `FILE:LINE:COL`.
    #[arg(long)]
    explain: bool,

    /// Emit results as JSON — a pretty object, or an array for row sets. It
    /// carries more than the text, which is a summary: every gem picked or
    /// missing where text lists a few, each row's counts and reasons.
    #[arg(short = 'j', long)]
    json: bool,

    /// Emit newline-delimited JSON, one compact object per line. A row set
    /// (`--refs`, `--dead`, `--symbols`, `--usage`) streams each row on its
    /// own line, then ends with one `{"answer": …}` line: the rest of the
    /// `--json` answer, and `rows`, how many came before it.
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
    let started = std::time::Instant::now();
    crate::store::untracked_memory();
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => return clap_failure(error, started),
    };

    // Before any store or git work: generating a completion script must not
    // need a checkout, and every other command refuses a non-repo.
    if let Some(shell) = cli.completions {
        clap_complete::generate(shell, &mut Cli::command(), "trekr", &mut std::io::stdout());
        return ExitCode::SUCCESS;
    }

    // An index reports its own phases; the tree it prepares is one of them.
    if cli.profile && cli.index.is_none() {
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

    // Both qualify an answer about a position, which a bare `FILE:LINE:COL`
    // asks as well as `--def` does.
    let position = cli.def.is_some()
        || cli.refs.is_none()
            && cli
                .input
                .as_deref()
                .is_some_and(|i| position::Spec::parse(i).is_some());
    // `--context` also says which checkout a name is asked about in.
    let named = cli.refs.is_some() || cli.ancestors.is_some() || cli.input.is_some();
    for (on, flag, applies) in [
        (cli.explain, "--explain", position),
        (
            cli.context.is_some(),
            "--context",
            position || named || cli.status,
        ),
    ] {
        if on && !applies {
            let message = match flag {
                "--context" => "--context applies to a query: a position, a name \
                                (`trekr Widget#save --context DIR`), --refs, --ancestors \
                                or --status"
                    .to_string(),
                _ => format!(
                    "{flag} applies to a position: `trekr {flag} FILE:LINE:COL`, or with --def"
                ),
            };
            count(
                "invalid",
                String::new(),
                Outcome::Error(Failure::Usage.as_str()),
                started,
            );
            return fail(out, Failure::Usage, &message);
        }
    }

    // The feature each branch counts as; `None` for what is not a use of the
    // engine (`--usage` itself) or is counted by its own front (`--lsp`).
    let (feature, result) = if cli.lsp {
        (
            None,
            crate::serve::run(cli.profile).map(|()| ExitCode::SUCCESS),
        )
    } else if let Some(path) = &cli.index {
        (
            Some("index"),
            cmd_index(out, path, cli.jobs, cli.profile, !cli.no_gems),
        )
    } else if let Some(path) = &cli.symbols {
        (Some("symbols"), cmd_symbols(out, path))
    } else if let Some(name) = &cli.refs {
        (
            Some("refs"),
            cmd_refs(out, name, cli.include_excluded, cli.context.as_deref()),
        )
    } else if let Some(spec) = &cli.def {
        (
            Some("def"),
            cmd_def(out, spec, cli.explain, cli.context.as_deref()),
        )
    } else if !cli.dead.is_empty() {
        (Some("dead"), cmd_dead(out, &cli.dead))
    } else if let Some(name) = &cli.ancestors {
        (
            Some("ancestors"),
            cmd_ancestors(out, name, cli.context.as_deref()),
        )
    } else if let Some(path) = &cli.drop {
        (Some("drop"), cmd_drop(out, path))
    } else if cli.gc {
        (
            Some("gc"),
            cmd_gc(out, cli.older_than, cli.dry_run, cli.vacuum),
        )
    } else if cli.status {
        (
            Some("status"),
            cmd_status(out, cli.all, cli.context.as_deref()),
        )
    } else if cli.usage {
        let days = cli.days;
        (
            None,
            if cli.misses {
                cmd_misses(out, days)
            } else {
                cmd_usage(out, days)
            },
        )
    } else if let Some(input) = &cli.input {
        (
            Some("bare"),
            cmd_bare(out, input, cli.explain, cli.context.as_deref()),
        )
    } else {
        (
            Some("invalid"),
            Err(Failure::Usage.error(
                "nothing to do (try `trekr Widget#save`, `trekr app.rb:42`, \
                 or --index, --status, --usage)",
            )),
        )
    };

    let code = match &result {
        Ok(code) => *code,
        Err(e) => fail(out, Failure::of(e), &format!("{e:#}")),
    };
    // After the answer is out, never before it (rq DECISIONS D13).
    if let Some(feature) = feature {
        let note = crate::usage::take();
        let mut flags = note.flags;
        flags.extend(cli_flags(&cli, out));
        let outcome = match (note.outcome, &result) {
            (Some(outcome), _) => outcome,
            (None, Ok(code)) if *code == ExitCode::SUCCESS => Outcome::Hit,
            (None, Ok(code)) if *code == ExitCode::from(1) => Outcome::Empty,
            // Only `not_indexed` exits otherwise, and it names its own outcome.
            (None, Ok(_)) => Outcome::Error(Failure::Internal.as_str()),
            (None, Err(e)) => Outcome::Error(Failure::of(e).as_str()),
        };
        let feature = note.feature.unwrap_or(feature);
        count(feature, crate::usage::join(&flags), outcome, started);
    }
    code
}

use crate::usage::Outcome;

/// Count one CLI use. The caller has already written its answer.
fn count(feature: &str, flags: String, outcome: Outcome, started: std::time::Instant) {
    crate::usage::Recorder::open().record(&crate::usage::Tally {
        surface: "cli",
        feature,
        flags,
        origin: &crate::usage::origin::detect(),
        outcome,
        latency: Some(started.elapsed()),
        cold: false,
    });
}

/// Which knobs a call reached for — names only, never their values.
fn cli_flags(cli: &Cli, out: Output) -> Vec<&'static str> {
    [
        (out == Output::Json, "json"),
        (out == Output::Ndjson, "ndjson"),
        (cli.explain, "explain"),
        (cli.context.is_some(), "context"),
        (cli.include_excluded, "include-excluded"),
        (cli.no_gems, "no-gems"),
        (cli.dry_run, "dry-run"),
        (cli.vacuum, "vacuum"),
        (cli.profile, "profile"),
    ]
    .into_iter()
    .filter_map(|(on, name)| on.then_some(name))
    .collect()
}

/// Report an error and return its exit code. The message always goes to
/// stderr; a structured caller also gets it as one object on stdout, without
/// the `trekr:` that only a terminal needs to tell whose error it is.
fn fail(out: Output, kind: Failure, message: &str) -> ExitCode {
    eprintln!("trekr: {message}");
    emit_error(out, kind, message);
    ExitCode::from(kind.exit_code())
}

/// `{"error", "kind", "code"}` on stdout — nothing in text mode. `code` is
/// the exit code the process leaves with, read from the same mapping.
fn emit_error(out: Output, kind: Failure, message: &str) {
    let error = serde_json::json!({
        "error": message,
        "kind": kind.as_str(),
        "code": kind.exit_code(),
    });
    // Printed directly: a failing `emit_json` is reported through here.
    let rendered = match out {
        Output::Text => return,
        Output::Json => serde_json::to_string_pretty(&error),
        Output::Ndjson => serde_json::to_string(&error),
    };
    if let Ok(rendered) = rendered {
        println!("{rendered}");
    }
}

/// A command line clap rejected. Not `error.exit()`: clap exits 2, which
/// trekr gives "not indexed yet" (DEC-067).
fn clap_failure(error: clap::Error, started: std::time::Instant) -> ExitCode {
    // `--help` and `--version` are questions about trekr, not uses of it.
    if !error.use_stderr() {
        let _ = error.print();
        return ExitCode::SUCCESS;
    }
    let _ = error.print();
    // A malformed call is a use that failed, and worth counting.
    count(
        "invalid",
        String::new(),
        Outcome::Error(Failure::Usage.as_str()),
        started,
    );
    let out = requested_output(std::env::args_os().skip(1));
    let text = error.to_string();
    emit_error(out, Failure::Usage, text.lines().next().unwrap_or_default());
    ExitCode::from(Failure::Usage.exit_code())
}

/// The output mode argv asks for, read before clap has parsed it — so a
/// caller that asked for JSON gets its usage error as JSON too. A cluster of
/// short flags ends at the first that takes a value, and nothing after `--`
/// is a flag. Same reading as rq's.
fn requested_output(args: impl IntoIterator<Item = std::ffi::OsString>) -> Output {
    let command = Cli::command();
    let takes_value = |c: char| {
        command
            .get_arguments()
            .any(|a| a.get_short() == Some(c) && a.get_action().takes_values())
    };
    let (mut json, mut ndjson) = (false, false);
    for arg in args {
        let arg = arg.to_string_lossy();
        match arg.as_ref() {
            "--" => break,
            "--json" => json = true,
            "--ndjson" => ndjson = true,
            a if a.starts_with('-') && !a.starts_with("--") => {
                for c in a.chars().skip(1) {
                    match c {
                        'j' => json = true,
                        'J' => ndjson = true,
                        c if takes_value(c) => break,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    // The precedence a parsed command line gets in `run`.
    if ndjson {
        Output::Ndjson
    } else if json {
        Output::Json
    } else {
        Output::Text
    }
}

/// A file the caller named. Missing is their typo, not trekr's failure, and
/// so is a directory where a file was asked for.
fn read_input(path: &Path) -> anyhow::Result<Vec<u8>> {
    if path.is_dir() {
        return Err(Failure::Usage.error(format!(
            "{} is a directory; this asks about one file",
            path.display()
        )));
    }
    std::fs::read(path).map_err(|error| {
        let kind = match error.kind() {
            std::io::ErrorKind::NotFound => Failure::NotFound,
            _ => Failure::Io,
        };
        kind.error(format!("cannot read {}: {error}", path.display()))
    })
}

/// The checkout containing a path the caller named.
/// The checkout a name is asked about in: `--context`'s, else the one the
/// current directory is in.
fn asked_from(context: Option<&Path>) -> anyhow::Result<PathBuf> {
    match context {
        Some(dir) => named_checkout(dir),
        None => scan::repo_root(Path::new(".")),
    }
}

fn named_checkout(path: &Path) -> anyhow::Result<PathBuf> {
    // Checked first: git would run in the nearest existing parent, and could
    // answer for a checkout the caller never meant.
    if !path.exists() {
        return Err(Failure::NotFound.error(format!("no such path: {}", path.display())));
    }
    scan::repo_root(path)
}

/// A writer queued behind another's lock says so on stderr — stdout stays the
/// answer alone, in every mode — after a second, then every 10 s on a
/// terminal and every minute otherwise (DEC-171).
fn writer_waiting(waited: std::time::Duration) {
    use std::io::IsTerminal;
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    // The next notice, in seconds of this wait; and how far the last call had
    // waited, since a smaller number is a new wait, for another lock.
    static DUE: AtomicU64 = AtomicU64::new(1);
    static LAST: AtomicU64 = AtomicU64::new(0);
    let (secs, millis) = (waited.as_secs(), waited.as_millis() as u64);
    if millis < LAST.swap(millis, Relaxed) {
        DUE.store(1, Relaxed);
    }
    let due = DUE.load(Relaxed);
    if secs < due {
        return;
    }
    let every = if std::io::stderr().is_terminal() {
        10
    } else {
        60
    };
    DUE.store(secs - secs % every + every, Relaxed);
    let db = store_path().map_or_else(
        |_| "the index".into(),
        |p| paths::pretty(&p.to_string_lossy()),
    );
    match due {
        1 => eprintln!(
            "trekr: waiting for another trekr writer to finish with {db} \
             (it holds the write lock; this waits up to 10 minutes)"
        ),
        _ => eprintln!("trekr: still waiting for another trekr writer ({secs}s)"),
    }
}

/// The indexed gem a path belongs to, if any: gems are indexed per
/// directory from an app's bundle, not as repositories — a git gem's
/// checkout included, though it has a `.git` (DEC-150).
fn gem_holding(store: &Store, path: &Path) -> Option<String> {
    let absolute = std::fs::canonicalize(path).ok()?;
    // The trailing `/` lets the gem's own root match, not only files in it.
    store
        .gem_containing(&format!("{}/", absolute.to_string_lossy()))
        .ok()
        .flatten()
}

/// The database: `$TREKR_DB`, else `~/.local/share/trekr/trekr.db`.
fn store_path() -> anyhow::Result<PathBuf> {
    Ok(match std::env::var("TREKR_DB") {
        Ok(p) => PathBuf::from(p),
        Err(_) => PathBuf::from(std::env::var("HOME")?).join(".local/share/trekr/trekr.db"),
    })
}

fn open_store() -> anyhow::Result<Store> {
    let open = || -> anyhow::Result<Store> {
        let path = store_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(Store::open(&path)?)
    };
    open().tag(Failure::Database)
}

/// Where an answer's paths are written from (DEC-076): the checkout the
/// question is about, which a relative path is relative to, and every root
/// the store knows, gems included. Set once a command knows its checkout.
struct Rooting {
    base: String,
    roots: Vec<String>,
}

static ROOTING: std::sync::OnceLock<Rooting> = std::sync::OnceLock::new();

/// Answer from this checkout: paths in the output are written against it.
fn answering_in(store: &Store, root: &str) {
    ROOTING.get_or_init(|| Rooting {
        base: root.to_string(),
        roots: store.roots().unwrap_or_default(),
    });
}

/// Answer about one file that needs no index (`--symbols`): paths are
/// written against its git checkout, and against the checkouts a store
/// already holds. Never creates a store just to say where a file is.
fn answering_about(file: &Path) {
    let base = scan::repo_root(file).ok();
    let mut roots: Vec<String> = crate::store::default_path()
        .ok()
        .filter(|db| db.exists())
        .and_then(|_| open_store().ok())
        .and_then(|store| store.roots().ok())
        .unwrap_or_default();
    roots.extend(base.iter().map(|b| b.to_string_lossy().into_owned()));
    ROOTING.get_or_init(|| Rooting {
        base: base.map_or_else(String::new, |b| b.to_string_lossy().into_owned()),
        roots,
    });
}

impl Rooting {
    /// A path as the checkout holding it names it, and that checkout's root.
    /// Ruby core and a file in no indexed checkout keep their path, rootless.
    fn place(&self, path: &str) -> (String, serde_json::Value) {
        if let Some((dir, file)) = core_file(path) {
            return (file, dir.into());
        }
        if path.starts_with('<') {
            return (path.to_string(), serde_json::Value::Null);
        }
        let absolute = match path.starts_with('/') {
            true => path.to_string(),
            false => format!("{}/{path}", self.base),
        };
        let holder = self
            .roots
            .iter()
            .filter(|root| paths::under(root, &absolute))
            .max_by_key(|root| root.len());
        match holder {
            Some(root) => (absolute[root.len() + 1..].to_string(), root.clone().into()),
            None => (absolute, serde_json::Value::Null),
        }
    }
}

/// A Ruby core site as a file that exists: the stubs are written beside the
/// store (DEC-078), so `<core>/rbs-3.8.0-…/String.rb` becomes `String.rb`
/// under that Ruby's directory there, as it does for the editor. `None` for
/// any other path, or when the files cannot be written, which leaves the
/// site rootless.
fn core_file(path: &str) -> Option<(String, String)> {
    let dir = crate::store::core_dir().ok()?;
    let (dir, file) = crate::tree::core_file_of(&dir, path)?;
    Some((dir.to_string_lossy().into_owned(), file))
}

/// Every `path` in an answer made relative to its checkout, with `root`
/// beside it naming that checkout.
fn rooted(value: &mut serde_json::Value) {
    let Some(rooting) = ROOTING.get() else {
        return;
    };
    match value {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(path)) = map.get("path") {
                let (path, root) = rooting.place(path);
                map.insert("path".into(), path.into());
                map.insert("root".into(), root);
            }
            map.values_mut().for_each(rooted);
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(rooted),
        _ => {}
    }
}

/// Set when an answer spans checkouts: text then writes every path whole.
static TEXT_ABSOLUTE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A path as text shows it: relative inside the checkout being asked about,
/// absolute (with `~`) anywhere else — a gem, Ruby core.
fn shown(path: &str) -> String {
    if let Some((dir, file)) = core_file(path) {
        return paths::pretty(&format!("{dir}/{file}"));
    }
    match ROOTING.get() {
        _ if TEXT_ABSOLUTE.load(std::sync::atomic::Ordering::Relaxed) => paths::pretty(path),
        Some(rooting) if paths::under(&rooting.base, path) => {
            path[rooting.base.len() + 1..].to_string()
        }
        _ => paths::pretty(path),
    }
}

fn emit_json<T: serde::Serialize>(out: Output, value: &T) -> anyhow::Result<()> {
    let mut value = serde_json::to_value(value)?;
    rooted(&mut value);
    let rendered = if out == Output::Json {
        serde_json::to_string_pretty(&value)?
    } else {
        serde_json::to_string(&value)?
    };
    println!("{rendered}");
    Ok(())
}

/// `emit_json` for an answer holding one long list: `answer[key]` is written
/// from `rows` one row at a time, where the answer would otherwise be built
/// whole as a `Value`, copied, and rendered to one string before a byte of
/// it is printed. The bytes are the same. Under `--ndjson` the rows stream,
/// one per line, and the rest of the answer follows them (`ndjson_rows`).
fn emit_listing<T: serde::Serialize>(
    out: Output,
    answer: serde_json::Value,
    key: &str,
    rows: &[T],
) -> anyhow::Result<()> {
    let mut w = std::io::BufWriter::new(std::io::stdout().lock());
    render_listing(out, answer, key, rows, &mut w)?;
    w.flush()?;
    Ok(())
}

fn render_listing<T: serde::Serialize>(
    out: Output,
    mut answer: serde_json::Value,
    key: &str,
    rows: &[T],
    w: &mut impl Write,
) -> anyhow::Result<()> {
    rooted(&mut answer);
    let Some(head) = answer.as_object_mut() else {
        anyhow::bail!("an answer with a listing is an object");
    };
    if out == Output::Ndjson {
        head.remove(key);
        return ndjson_rows(rows, head.clone(), w);
    }
    render_json(out, &Listing { head, key, rows }, w)
}

/// A row set under `--ndjson` (DEC-290): each row on its own line, as the
/// `--json` array holds it, then one `{"answer": …}` line — the rest of the
/// `--json` answer, and `rows`, how many lines came before it. Always last,
/// and always written, so a reader knows the stream ended rather than broke.
fn ndjson_rows<T: serde::Serialize>(
    rows: &[T],
    mut head: serde_json::Map<String, serde_json::Value>,
    w: &mut impl Write,
) -> anyhow::Result<()> {
    for row in rows {
        serde_json::to_writer(&mut *w, &Rows::rooted(row)?)?;
        writeln!(w)?;
    }
    head.insert("rows".into(), rows.len().into());
    serde_json::to_writer(&mut *w, &serde_json::json!({ "answer": head }))?;
    writeln!(w)?;
    Ok(())
}

/// Print a row set. `None` means it was handled; `Some` hands text mode back
/// to the caller.
fn emit_rows<T: serde::Serialize>(out: Output, rows: &[T]) -> anyhow::Result<bool> {
    match out {
        Output::Text => return Ok(false),
        Output::Json => {
            let mut w = std::io::BufWriter::new(std::io::stdout().lock());
            render_json(out, &Rows(rows), &mut w)?;
            w.flush()?;
        }
        Output::Ndjson => {
            let mut w = std::io::BufWriter::new(std::io::stdout().lock());
            ndjson_rows(rows, serde_json::Map::new(), &mut w)?;
            w.flush()?;
        }
    }
    Ok(true)
}

/// One JSON document and its newline, as `println!` of the rendered string
/// would print it, without the string.
fn render_json(
    out: Output,
    value: &impl serde::Serialize,
    w: &mut impl Write,
) -> anyhow::Result<()> {
    match out {
        Output::Json => serde_json::to_writer_pretty(&mut *w, value)?,
        _ => serde_json::to_writer(&mut *w, value)?,
    }
    writeln!(w)?;
    Ok(())
}

/// An answer's object with `key` written from `rows`, in the key order the
/// whole `Value` would have had.
struct Listing<'a, T> {
    head: &'a serde_json::Map<String, serde_json::Value>,
    key: &'a str,
    rows: &'a [T],
}

impl<T: serde::Serialize> serde::Serialize for Listing<'_, T> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = s.serialize_map(Some(self.head.len()))?;
        for (k, v) in self.head {
            match k == self.key {
                true => map.serialize_entry(k, &Rows(self.rows))?,
                false => map.serialize_entry(k, v)?,
            }
        }
        map.end()
    }
}

/// Rows written as the array a `Value` of them would be, each rooted as it
/// goes (`rooted`).
struct Rows<'a, T>(&'a [T]);

impl<T: serde::Serialize> Rows<'_, T> {
    fn rooted(row: &T) -> serde_json::Result<serde_json::Value> {
        let mut value = serde_json::to_value(row)?;
        rooted(&mut value);
        Ok(value)
    }
}

impl<T: serde::Serialize> serde::Serialize for Rows<'_, T> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{Error, SerializeSeq};
        let mut seq = s.serialize_seq(Some(self.0.len()))?;
        for row in self.0 {
            seq.serialize_element(&Rows::rooted(row).map_err(S::Error::custom)?)?;
        }
        seq.end()
    }
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
    files: &scan::Files,
    git_state: i64,
    known: &mut Option<HashSet<Oid>>,
    pool: &rayon::ThreadPool,
    profile: &mut Option<profile::Profile>,
) -> anyhow::Result<crate::store::Indexed> {
    let wanted: HashSet<&Oid> = files.values().collect();

    // One path per unknown blob: identical content under two names is one
    // parse, and which name it was read from cannot matter. A map identical
    // to the stored one has nothing unknown, so the known set — every blob on
    // the machine — is loaded only when there may be.
    let mut to_parse: HashMap<&Oid, PathBuf> = HashMap::new();
    if !store.map_unchanged(&root.to_string_lossy(), files)? {
        if known.is_none() {
            *known = Some(profile::timed(profile, "known-diff", || store.blob_oids())?);
        }
        let known = known.as_ref().expect("just loaded");
        for (rel, oid) in files {
            if !known.contains(oid) {
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
    let bulk = store.autocommit()
        && known
            .as_ref()
            .is_some_and(|k| bulk_load(to_parse.len(), k.len()));
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
        let counts = if bulk {
            store.write_bulk(&root, files, facts, git_state)
        } else {
            store.write(&root, files, facts, git_state)
        };
        (counts, parsing.join().expect("the parse does not panic"))
    });
    let counts = counts?;
    received.finish(store, profile, known, parse_done);
    Ok(counts)
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
    let bytes = std::fs::read(path).ok()?;
    let facts = extract::extract(&bytes);
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
        known: &mut Option<HashSet<Oid>>,
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
            known.extend(self.fresh);
        }
    }
}

/// Index the gems this checkout resolves, skipping any already on this machine.
///
/// Returns the gems the lockfile named but disk did not have. A named-but-
/// unlocated gem is a hole in every answer that would have come from it, so it
/// is reported rather than silently absent.
fn index_gems(
    store: &mut Store,
    repo: &Path,
    known: &mut Option<HashSet<Oid>>,
    pool: &rayon::ThreadPool,
    profile: &mut Option<profile::Profile>,
) -> anyhow::Result<GemReport> {
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
        about: stdlib.as_ref().map(crate::gems::stdlib::Stdlib::about),
        ..GemReport::default()
    };
    if let Some(stdlib) = &stdlib {
        let mut indexed = index_stdlib(store, stdlib, known, pool, profile)?;
        // Its Ruby's signatures, which core and the stdlib's compiled half
        // are served from, read once per Ruby (DEC-240).
        indexed.rbs = profile::timed(profile, "rbs", || crate::rbs::prepare(store, stdlib))?;
        report.stdlib = Some(indexed);
    }
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
        if store.has_checkout(&root_str)? || fresh.contains(&gem_root) {
            report.already_indexed += 1;
            continue;
        }
        fresh.push(gem_root);
    }
    for counts in index_bundle(store, &fresh, known, pool, profile)? {
        report.indexed += 1;
        report.files += counts.files;
    }
    let repo = repo.to_string_lossy();
    let stdlib_root = report.stdlib.as_ref().map(|s| s.root.as_str());
    store.set_gems_used(&repo, &used, stdlib_root)?;
    if let Some(stdlib) = report.stdlib.as_mut() {
        stdlib.hidden = store.hidden_default_gems(&repo)?;
    }
    Ok(report)
}

/// Files the bundle's stream parses at a time, and holds for the writer: two
/// chunks' facts are in memory at once.
const BUNDLE_CHUNK: usize = 128;

/// Index the gems new to the store as one stream (DEC-232): every gem's
/// `lib/` walked on the pool, every new blob parsed on it across gem
/// boundaries, and each gem written in turn as its files arrive — where one
/// gem at a time left the pool idle while each small gem was walked and
/// written. What each gem indexed, for those with files.
fn index_bundle(
    store: &mut Store,
    gems: &[PathBuf],
    known: &mut Option<HashSet<Oid>>,
    pool: &rayon::ThreadPool,
    profile: &mut Option<profile::Profile>,
) -> anyhow::Result<Vec<crate::store::Indexed>> {
    if gems.is_empty() {
        return Ok(Vec::new());
    }
    // Only `lib/`: it is where a gem's public code lives, and a gem's
    // spec/ and test/ trees are large and never navigated to.
    let walked: Vec<(&PathBuf, scan::Files)> = profile::timed(profile, "gem-walk", || {
        pool.install(|| {
            gems.par_iter()
                .map(|gem| {
                    let mut files = scan::walk(gem, "lib");
                    files.retain(|path, _| {
                        !path
                            .strip_prefix("lib/")
                            .is_some_and(crate::gems::stdlib::opt_in)
                    });
                    (gem, files)
                })
                .filter(|(_, files)| !files.is_empty())
                .collect()
        })
    });
    if walked.is_empty() {
        return Ok(Vec::new());
    }
    if known.is_none() {
        *known = Some(profile::timed(profile, "known-diff", || store.blob_oids())?);
    }
    // One path per unknown blob, in gem order: a blob two gems share is
    // parsed for the first and known by the time the second is written.
    let mut to_parse: Vec<(usize, Oid, PathBuf)> = Vec::new();
    {
        let known = known.as_ref().expect("just loaded");
        let mut seen: HashSet<&Oid> = HashSet::new();
        for (at, (root, files)) in walked.iter().enumerate() {
            let wanted: HashSet<&Oid> = files.values().collect();
            let before = to_parse.len();
            for (rel, oid) in files.iter() {
                if !known.contains(oid) && seen.insert(oid) {
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
    known: &mut Option<HashSet<Oid>>,
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
    if store.has_checkout(&root)? {
        return Ok(report);
    }
    let files = crate::gems::stdlib::files(&stdlib.root);
    let counts = index_files(store, &stdlib.root, &files, 0, known, pool, profile)?;
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

fn cmd_index(
    out: Output,
    path: &Path,
    jobs: usize,
    want_profile: bool,
    with_gems: bool,
) -> anyhow::Result<ExitCode> {
    // First, before the scan or the pool starts a thread.
    crate::serve::fresh::yield_if_background();
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

    store.wait_as_writer(writer_waiting)?;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(jobs).build()?;
    let mut known = None;
    let counts = index_files(
        &mut store,
        &root,
        &files,
        git_state,
        &mut known,
        &pool,
        &mut profile,
    )?;

    let gems = if with_gems {
        let gems =
            store.batch(|store| index_gems(store, &root, &mut known, &pool, &mut profile))?;
        if let Some(profile) = profile.as_mut() {
            // The bundle's one commit, outside every gem's own write.
            profile.phase("commit", store.take_timing().commit);
        }
        gems
    } else {
        GemReport::default()
    };

    // Only when something was actually read, and then only once the store has
    // outgrown its statistics: a full ANALYZE costs seconds whatever changed.
    if counts.parsed > 0 || gems.indexed > 0 {
        profile::timed(&mut profile, "analyze", || {
            store.analyze_if_outgrown();
            Ok::<(), anyhow::Error>(())
        })?;
    }

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
    profile::timed(&mut profile, "tree", || {
        crate::tree::Tree::prepare(&store, &root_str)
    })?;
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

/// What is indexed. By default the checkout this is run in (or `--context`
/// names), with its gems counted rather than listed — a Rails app's bundle is
/// hundreds of them — and a count of the rest; `--all` lists every checkout.
/// Outside any checkout, the repos are listed and the gems counted. A checkout
/// nobody indexed is `not_indexed`, exit 2, as a query from it would be.
fn cmd_status(out: Output, all: bool, context: Option<&Path>) -> anyhow::Result<ExitCode> {
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
        return status_not_indexed(out, Path::new(root), &store, &checkouts, &totals);
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
    // Counted on a row above, so not among the others.
    let mut counted: HashSet<String> = HashSet::new();
    for checkout in &shown {
        let mut row = serde_json::to_value(checkout)?;
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
            row["ruby"] = serde_json::to_value(status_ruby(&checkout.repo, stdlib.as_deref()))?;
            if let Some(stdlib) = stdlib {
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

    if out != Output::Text {
        // One object, because the totals are the point: they are what N
        // checkouts share, not the sum of what each one costs.
        let mut answer = serde_json::json!({
            "checkouts": rows,
            "others": others,
            "totals": totals,
        });
        if let Some(reason) = &reason {
            answer["reason"] = reason.as_str().into();
        }
        emit_json(out, &answer)?;
        return Ok(exit_on(!checkouts.is_empty()));
    }
    if let Some(reason) = reason {
        println!("{reason} (try `trekr --index`)");
        return Ok(ExitCode::from(1));
    }
    for row in &rows {
        println!(
            "{:>7} files  {:>7} blobs  {}",
            row["files"].as_i64().unwrap_or(0),
            row["blobs"].as_i64().unwrap_or(0),
            paths::pretty(row["repo"].as_str().unwrap_or_default())
        );
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
        let gems = &row["gems"];
        let (Some(count), Some(indexed)) = (gems["count"].as_u64(), gems["indexed"].as_u64())
        else {
            continue;
        };
        let state = match (count, indexed) {
            (0, _) => continue,
            (n, i) if n == i => "all indexed".to_string(),
            (_, i) => format!("{i} of {count} indexed"),
        };
        println!(
            "{:>32}+ {count} gem{}, {state} ({} files)",
            "",
            if count == 1 { "" } else { "s" },
            gems["files"].as_i64().unwrap_or(0)
        );
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
    Ok(ExitCode::SUCCESS)
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
            }),
        )?;
        return Ok(ExitCode::from(2));
    }
    match upgraded {
        true => println!(
            "{} is not indexed — {reason}. Run: {hint}",
            paths::pretty(&root)
        ),
        false => println!("{} is not indexed — run: {hint}", paths::pretty(&root)),
    }
    if repos + gems > 0 {
        let plural = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
        println!(
            "\nalso indexed: {}, {} — `trekr --status --all` lists them",
            plural(repos, "other repo"),
            plural(gems, "gem")
        );
    }
    Ok(ExitCode::from(2))
}

/// Outline one file, by reading it.
///
/// Parsed rather than looked up: `--def` and `--refs` both reparse so that an
/// unindexed edit still answers correctly, and an outline that went stale — or
/// answered nothing at all until someone ran `--index` — was the odd one out.
/// Parsing also means any readable Ruby file outlines, in a repo or not, which
/// is the same rule the LSP surface follows (DEC-024).
fn cmd_symbols(out: Output, path: &Path) -> anyhow::Result<ExitCode> {
    let source = read_input(path)?;
    let facts = extract::extract(&source);
    let file = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut symbols: Vec<crate::store::Symbol> = facts.defs.iter().map(Into::into).collect();
    // Every row carries its location, like every other answer's (DEC-080).
    if out != Output::Text {
        answering_about(&file);
        let file = file.to_string_lossy().into_owned();
        for symbol in &mut symbols {
            symbol.path.clone_from(&file);
        }
    }

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
        let marker = match (s.kind.as_str(), s.singleton) {
            ("method", true) => ".",
            ("method", false) => "#",
            _ => "",
        };
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
///
/// `parsed`, when given, holds each file's facts across calls: `--dead` asks
/// about every method in scope, and the files calling them are mostly the
/// same files. A single query passes `None` and holds one file at a time.
#[allow(clippy::too_many_arguments)]
fn gather_refs(
    tree: &Tree,
    store: &Store,
    root: &Path,
    root_str: &str,
    query: &crate::resolve::refs::Query,
    target: Option<&str>,
    // Keep the excluded sites too, so `--include-excluded` can show them.
    keep_all: bool,
    parsed: Option<&mut Parsed>,
    // Every call of the name as a row of its own, for the bare-name listing:
    // the index keeps which files call a name, not where (DEC-193).
    mut sites: Option<&mut Vec<crate::store::Ref>>,
) -> anyhow::Result<(
    Vec<crate::resolve::refs::Reference>,
    crate::resolve::refs::Counts,
)> {
    use crate::resolve::refs;
    let files = store.files_calling(root_str, &query.name)?;
    let listing = sites.is_some();
    // Every worker tiers against the one tree, which is shared (DEC-250),
    // and files come back in the order they were listed.
    let tier = |path: &String, facts: &crate::core::Facts| {
        let mut tiered = Tiered::default();
        for call in facts.calls.iter().filter(|c| c.name == query.name) {
            if listing {
                tiered.sites.push(crate::store::Ref::call(path, call));
            }
            let reference = refs::tier_call(tree, facts, call, path, query, target);
            tiered.counts.record(&reference);
            // Excluded sites are counted, not listed: the count is the
            // product, and the list would be the grep we are trying to
            // beat. `keep_all` is how `--include-excluded` makes the
            // claim auditable.
            if keep_all || reference.tier != refs::Tier::Excluded {
                tiered.found.push(reference);
            }
        }
        tiered
    };
    let read = |path: &String| {
        std::fs::read(root.join(path))
            .ok()
            .map(|bytes| extract::extract(&bytes))
    };
    let tiered: Vec<Tiered> = match parsed {
        // Held across queries by the caller: parse what it lacks, then tier.
        Some(parsed) => {
            let fresh: Vec<_> = files
                .par_iter()
                .filter(|path| !parsed.contains_key(*path))
                .map(|path| (path.clone(), read(path)))
                .collect();
            parsed.extend(fresh);
            let parsed = &*parsed;
            files
                .par_iter()
                .filter_map(|path| Some((path, parsed.get(path)?.as_ref()?)))
                .map(|(path, facts)| tier(path, facts))
                .collect()
        }
        // One query: each file is parsed and tiered on the worker that takes
        // it, and only the files in flight are held.
        None => files
            .par_iter()
            .filter_map(|path| Some(tier(path, &read(path)?)))
            .collect(),
    };
    let mut found = Vec::new();
    let mut counts = refs::Counts::default();
    for mut file in tiered {
        found.append(&mut file.found);
        counts.add(&file.counts);
        if let Some(sites) = sites.as_deref_mut() {
            sites.append(&mut file.sites);
        }
    }
    found.sort_by_key(refs::order);
    Ok((found, counts))
}

/// One file's call sites, tiered.
#[derive(Default)]
struct Tiered {
    found: Vec<crate::resolve::refs::Reference>,
    counts: crate::resolve::refs::Counts,
    sites: Vec<crate::store::Ref>,
}

/// A file's facts by checkout-relative path, `None` when it could not be read.
type Parsed = HashMap<String, Option<crate::core::Facts>>;

/// What a name *is*, in one answer: where it is defined, what kind of location
/// that is, and how many call sites can actually reach it.
///
/// The reason the bare grammar earns its keep rather than aliasing a flag. Asked
/// about `Widget#save` a person wants the definition **and** whether anything
/// calls it; asked about `Widget` they want the definition **and** what it
/// inherits. Two commands' worth of answer, which is what makes this worth a
/// shape rather than a synonym.
fn cmd_card(out: Output, text: &str, context: Option<&Path>) -> anyhow::Result<ExitCode> {
    use crate::resolve::refs;
    let query = refs::Query::parse(text);
    check_method_shape(&query, text)?;
    let root = asked_from(context)?;
    let root_str = root.to_string_lossy().into_owned();
    let store = open_store()?;
    if !store.has_checkout(&root_str)? {
        return not_indexed(out, &root, &store);
    }
    answering_in(&store, &root_str);
    let tree = build_tree(&store, &root_str)?;

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
        let variants = tree.variants_of(&fqn);
        if !variants.is_empty() {
            let split = Split::of(&tree, &fqn, &variants);
            return report(
                out,
                serde_json::json!({
                    "query": text,
                    "status": "ambiguous",
                    "fqn": fqn,
                    "kind": tree.kind_of(&fqn),
                    "definition": tree.sites(&fqn),
                    "ancestors": [fqn],
                    "unresolved_ancestors": split.unresolved,
                    "variants": split.listed,
                }),
                true,
                &split.text.join("\n"),
            );
        }
        let chain = tree.ancestors(&fqn);
        let ancestors = public_chain(&chain.chain);
        let sites = tree.sites(&fqn).to_vec();
        let text_out = card_text(&fqn, &sites, &ancestors, None);
        return report(
            out,
            serde_json::json!({
                "query": text,
                "status": "resolved",
                "fqn": fqn,
                "kind": tree.kind_of(&fqn),
                "definition": sites,
                "ancestors": ancestors,
                "unresolved_ancestors": chain.unresolved,
            }),
            true,
            &text_out,
        );
    }

    // A method: where it is, and who can reach it.
    let (owner, definition) = refs::definition_of(&tree, &query);
    let (status, reason) = method_verdict(&tree, &query, owner.as_deref(), !definition.is_empty());
    let Some(owner) = owner else {
        let reason = reason.unwrap_or_default();
        return report(
            out,
            serde_json::json!({
                "query": text,
                "status": status,
                "confidence": 0.0,
                "reason": reason,
            }),
            false,
            &reason,
        );
    };
    let (_, counts) = gather_refs(
        &tree,
        &store,
        &root,
        &root_str,
        &query,
        Some(&owner),
        false,
        None,
        None,
    )?;
    let kind = tree
        .lookup(&owner, query.singleton, &query.name)
        .map(|method| method.kind());
    let shown = match query.singleton {
        true => format!("{owner}.{}", query.name),
        false => format!("{owner}#{}", query.name),
    };
    let resolved = refs::resolves_to(&tree, &owner, &query);
    let mut text_out = card_text(&shown, &definition, &[], Some(&counts));
    if let Some(line) = inherited_line(resolved.as_ref()) {
        text_out.push_str(&format!("\n  {line}"));
    }
    let (resolves_to, inherited) = resolved.unzip();
    let mut answer = serde_json::json!({
        "query": text,
        "status": status,
        "owner": owner,
        "method": query.name,
        "singleton": query.singleton,
        "kind": kind,
        "definition": definition,
        "resolves_to": resolves_to,
        "inherited": inherited.unwrap_or(false),
        "counts": counts,
    });
    if let Some(reason) = reason {
        text_out.push_str(&format!("\n  {reason}"));
        answer["reason"] = reason.into();
    }
    report(out, answer, !definition.is_empty(), &text_out)
}

/// "resolves to Base#save, inherited", for a method the owner does not define.
fn inherited_line(resolved: Option<&(String, bool)>) -> Option<String> {
    match resolved {
        Some((method, true)) => Some(format!("resolves to {method}, inherited")),
        _ => None,
    }
}

/// Refuse a method query no Ruby could mean: the owner is a constant path
/// (`Foo::Bar`), and the method one name after one `#` or `.`. The last
/// separator splits, so `Foo#a#b` would otherwise ask `Foo#a` for `b` and
/// answer "no indexed constant", which reads as a finding about the code.
fn check_method_shape(query: &crate::resolve::refs::Query, text: &str) -> anyhow::Result<()> {
    let Some(owner) = &query.owner else {
        return Ok(());
    };
    let constant = |segment: &str| {
        segment.starts_with(char::is_uppercase)
            && segment.chars().all(|c| c.is_alphanumeric() || c == '_')
    };
    let owner_ok = owner
        .strip_prefix("::")
        .unwrap_or(owner)
        .split("::")
        .all(constant);
    let name_ok = !query.name.is_empty()
        && !query
            .name
            .contains(|c: char| c.is_whitespace() || c == ':' || c == '#');
    if owner_ok && name_ok {
        return Ok(());
    }
    Err(Failure::Usage.error(format!(
        "`{text}` is not a method: expected Owner#method or Owner.method, \
         with the owner a constant like Foo::Bar"
    )))
}

/// Whether a method query found its method: the `status`, and the `reason`
/// when it did not.
///
/// An owner that resolves is not an answer about the method. "Nothing
/// defines it" is only said when the owner's whole chain was seen — the same
/// line `--refs` draws before it excludes a call site as `no_such_method`.
fn method_verdict(
    tree: &Tree,
    query: &crate::resolve::refs::Query,
    owner: Option<&str>,
    found: bool,
) -> (&'static str, Option<String>) {
    let Some(owner) = owner else {
        let written = query.owner.as_deref().unwrap_or("?");
        return (
            "residue",
            Some(format!("no indexed constant named {written}")),
        );
    };
    if found {
        return ("resolved", None);
    }
    let what = if query.singleton {
        "class method"
    } else {
        "method"
    };
    let name = &query.name;
    // A `method_missing` in the chain answers any name, so "has no such
    // method" would be a claim the code does not support. Core's own is
    // BasicObject's NoMethodError, and says nothing.
    if let Some(catcher) = tree.lookup(owner, query.singleton, "method_missing")
        && !crate::tree::is_core(&catcher.site.path)
    {
        return (
            "residue",
            Some(format!(
                "nothing in {owner}'s ancestors defines {what} {name}, but {} defines \
                 method_missing, which may answer it",
                crate::tree::public_name(&catcher.owner)
            )),
        );
    }
    // A method made from a name the source does not state may be this one.
    if let Some((maker, how)) = tree.dynamic_in_chain(owner, query.singleton, name) {
        return (
            "residue",
            Some(format!(
                "nothing in {owner}'s ancestors defines {what} {name}, but {maker} \
                 defines methods its source does not name ({}), which may \
                 include it",
                tree.dynamic_note(&how)
            )),
        );
    }
    let unseen = &tree.ancestors(owner).unresolved;
    if unseen.is_empty() {
        return (
            "no_such_method",
            Some(format!("{owner} has no {what} {name} in its ancestors")),
        );
    }
    const SHOWN: usize = 3;
    let named: Vec<&str> = unseen.iter().take(SHOWN).map(String::as_str).collect();
    let more = unseen.len().saturating_sub(SHOWN);
    let more = if more > 0 {
        format!(", and {more} more")
    } else {
        String::new()
    };
    (
        "residue",
        Some(format!(
            "nothing indexed in {owner}'s ancestors defines {what} {name}, but some of \
             them are not indexed ({}{more}) and it may come from one",
            named.join(", ")
        )),
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
            shown(&site.path),
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

fn cmd_refs(
    out: Output,
    text: &str,
    include_excluded: bool,
    context: Option<&Path>,
) -> anyhow::Result<ExitCode> {
    use crate::resolve::refs;
    let query = refs::Query::parse(text);
    check_method_shape(&query, text)?;
    let root = asked_from(context)?;
    let root_str = root.to_string_lossy().into_owned();
    let store = open_store()?;
    if !store.has_checkout(&root_str)? {
        return not_indexed(out, &root, &store);
    }
    answering_in(&store, &root_str);

    // A bare name narrows nothing, so it keeps the whole-mention view —
    // definitions and constant references included, which a method-shaped
    // query has no use for.
    if query.owner.is_none() {
        crate::usage::flag("by-name");
        return cmd_refs_by_name(out, &root, &root_str, &store, &query);
    }

    let tree = build_tree(&store, &root_str)?;
    let (owner, definition) = refs::definition_of(&tree, &query);
    let (status, reason) = method_verdict(&tree, &query, owner.as_deref(), !definition.is_empty());

    // An owner that does not exist, or a method its whole chain shows it does
    // not have, has no references: every same-name site belongs to another
    // owner, and the bare name is the question that lists them.
    if owner.is_none() || status == "no_such_method" {
        let reason = reason.unwrap_or_default();
        let hint = format!("trekr --refs {}", query.name);
        if out != Output::Text {
            let answer = serde_json::json!({
                "query": text,
                "status": status,
                "owner": owner,
                "method": query.name,
                "singleton": query.singleton,
                "definition": definition,
                "resolves_to": null,
                "inherited": false,
                "counts": refs::Counts::default(),
                "references": null,
                "reason": reason,
                "hint": hint,
            });
            emit_listing(out, answer, "references", &[] as &[refs::Reference])?;
        } else {
            println!(
                "{reason}\n  every call site of {} by name: {hint}",
                query.name
            );
        }
        return Ok(ExitCode::from(1));
    }

    let (found, counts) = gather_refs(
        &tree,
        &store,
        &root,
        &root_str,
        &query,
        owner.as_deref(),
        include_excluded,
        None,
        None,
    )?;
    let resolved = owner
        .as_deref()
        .and_then(|owner| refs::resolves_to(&tree, owner, &query));
    let inherited = inherited_line(resolved.as_ref());
    let (resolves_to, is_inherited) = resolved.unzip();
    let mut answer = serde_json::json!({
        "query": text,
        "status": status,
        "owner": owner,
        "method": query.name,
        "singleton": query.singleton,
        "definition": definition,
        "resolves_to": resolves_to,
        "inherited": is_inherited.unwrap_or(false),
        "counts": counts,
        // Written row by row from `found` (`emit_listing`).
        "references": null,
    });
    if let Some(reason) = &reason {
        answer["reason"] = reason.as_str().into();
    }
    // Sites that might call it and none that certainly do: an answer, but not
    // the narrowing the command exists for.
    if !found.is_empty() && counts.confirmed == 0 {
        crate::usage::outcome(Outcome::Uncertain);
    }
    if out != Output::Text {
        emit_listing(out, answer, "references", &found)?;
        return Ok(exit_on(!found.is_empty()));
    }

    if let Some(reason) = &reason {
        println!("{reason}");
    }
    if let Some(line) = &inherited {
        println!("{line}");
    }
    for site in &definition {
        println!(
            "{}:{}:{}  definition",
            shown(&site.path),
            site.line,
            site.col
        );
    }
    for reference in &found {
        println!(
            "{}:{}:{}  {:<10} {}",
            shown(&reference.path),
            reference.line,
            reference.col,
            format!("{:?}", reference.tier).to_lowercase(),
            reference.why,
        );
    }
    // The number a grep cannot produce, said out loud — a zero included,
    // since "nothing ruled out" is a finding too. An answer that lists no
    // site has already said why.
    if !found.is_empty() || reason.is_none() || include_excluded {
        println!(
            "\n{} confirmed, {} possible, {} excluded of {} same-name call sites",
            counts.confirmed,
            counts.possible,
            counts.excluded,
            counts.confirmed + counts.possible + counts.excluded,
        );
    }
    if counts.excluded > 0 {
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
    if store.calls_name(root_str, &query.name)? {
        let tree = build_tree(store, root_str)?;
        let mut calls = Vec::new();
        let (found, _) = gather_refs(
            &tree,
            store,
            root,
            root_str,
            query,
            None,
            false,
            None,
            Some(&mut calls),
        )?;
        rows.extend(calls);
        // In the order the index's one query gave: by place, and at one place
        // a definition, then a constant, then a call.
        let rank = |role: &str| match role {
            "definition" => 0,
            "constant" => 1,
            _ => 2,
        };
        rows.sort_by(|a, b| {
            (&a.path, a.line, a.col, rank(&a.role)).cmp(&(&b.path, b.line, b.col, rank(&b.role)))
        });
        // Match by position: one call site, one tiering. Keyed, because a
        // common name has as many sites as rows and a scan per row was
        // quadratic in them. First one wins, as the scan's `find` did.
        let mut at: HashMap<(&str, u32, u32), &crate::resolve::refs::Reference> = HashMap::new();
        for r in &found {
            at.entry((r.path.as_str(), r.line, r.col)).or_insert(r);
        }
        for row in rows.iter_mut().filter(|row| row.role == "call") {
            if let Some(reference) = at.get(&(row.path.as_str(), row.line, row.col)) {
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
        let line = format!(
            "{}:{}:{}  {:<11} {}",
            shown(&row.path),
            row.line,
            row.col,
            row.role,
            detail
        );
        println!("{}", line.trim_end());
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
/// A tree a command builds, uses, and exits holding.
///
/// Never freed: the process is about to end, and freeing a checkout's
/// namespace string by string costs time that grows with the repo — the OS
/// takes the pages back in one go.
type OneShotTree = std::mem::ManuallyDrop<Tree>;

fn build_tree(store: &Store, root: &str) -> anyhow::Result<OneShotTree> {
    Tree::build(store, root).map(std::mem::ManuallyDrop::new)
}

fn checkout_for_query(path: &Path, pinned: Option<&Path>) -> anyhow::Result<(PathBuf, Store)> {
    let store = open_store()?;
    let root = match pinned {
        Some(root) => std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf()),
        None => checkout_for(&store, path)?,
    };
    Ok((root, store))
}

fn tree_for(path: &Path, pinned: Option<&Path>) -> anyhow::Result<(PathBuf, Store, OneShotTree)> {
    let store = open_store()?;
    let root = match pinned {
        Some(root) => std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf()),
        None => checkout_for(&store, path)?,
    };
    let tree = build_tree(&store, &root.to_string_lossy())?;
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
    let known = store.has_blob(&oid).unwrap_or(false);
    let facts = (!known).then(|| crate::extract::extract(&bytes));
    // Busy means another process is writing the index. The answer comes from
    // what it has committed, and says this file may lag (DEC-066).
    let (changed, busy) = match store.refresh_file(&root_str, &relative, &oid, facts.as_ref()) {
        Ok(changed) => (changed, false),
        Err(error) => (false, crate::store::is_busy(&error)),
    };

    let mut freshness = serde_json::json!({
        "stale": true,
        "refreshed": changed.then(|| relative.clone()),
        "hint": format!("trekr --index {}", paths::pretty(&root_str)),
    });
    if busy {
        freshness["busy"] = relative.into();
    }
    Some(freshness)
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
    crate::usage::flag("bare");
    if position::Spec::parse(input).is_some() {
        crate::usage::feature("def");
        return cmd_def(out, input, explain, context);
    }
    crate::usage::feature("card");
    // A method: `Owner#method` or `Owner.method`, which `--refs` already parses
    // and which is the one shape with a genuinely richer answer than a flag.
    if input.contains('#') || (input.contains('.') && !input.contains('/')) {
        return cmd_card(out, input, context);
    }
    if input.starts_with(|c: char| c.is_ascii_uppercase()) {
        return cmd_card(out, input, context);
    }
    Err(Failure::Usage.error(format!(
        "cannot tell what `{input}` is. Expected FILE:LINE[:COL], \
         Owner#method, Owner.method, or a Constant."
    )))
}

/// Definitions in scope that nothing appears to use (DEC-038).
///
/// Two passes, because the cheap one settles most of it. A name with hundreds
/// of call sites is not a candidate and must not cost a receiver-narrowed
/// search to establish that; the few that survive get the expensive question
/// asked properly.
///
/// Scope is the argument, evidence is the **whole checkout** — a method used
/// once from outside the scope is not a candidate, and a scope-local search
/// would say it is. Not the whole store: what else is indexed must not change
/// the answer (DEC-074). Scopes in two checkouts are each weighed against
/// their own.
fn cmd_dead(out: Output, paths: &[PathBuf]) -> anyhow::Result<ExitCode> {
    let mut checkouts: Vec<(PathBuf, Vec<PathBuf>)> = Vec::new();
    for path in paths {
        let root = named_checkout(path)?;
        match checkouts.iter_mut().find(|(known, _)| *known == root) {
            Some((_, scoped)) => scoped.push(path.clone()),
            None => checkouts.push((root, vec![path.clone()])),
        }
    }
    let store = open_store()?;
    for (root, _) in &checkouts {
        if !store.has_checkout(&root.to_string_lossy())? {
            return not_indexed(out, root, &store);
        }
    }
    // Across checkouts no one root is "here", so text writes every path
    // whole rather than relative to whichever scope came first.
    if let Some((first, _)) = checkouts.first() {
        answering_in(&store, &first.to_string_lossy());
    }
    if checkouts.len() > 1 {
        TEXT_ABSOLUTE.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    let mut rows: Vec<serde_json::Value> = Vec::new();
    let mut scope = 0;
    for (root, scoped) in &checkouts {
        scope += dead_in(&store, root, scoped, &mut rows)?;
    }
    note_candidate_callers(&mut rows);

    let found = !rows.is_empty();
    let summary = dead_summary(&rows);
    if out != Output::Text {
        let answer = serde_json::json!({ "scope": scope, "summary": summary, "candidates": null });
        emit_listing(out, answer, "candidates", &rows)?;
        return Ok(exit_on(found));
    }
    for row in &rows {
        let visibility = match row["visibility"].as_str() {
            Some("public") | None => String::new(),
            Some(other) => format!(" ({other})"),
        };
        println!(
            "{:<16} {}  {}{visibility}  — {}{}",
            row["tier"].as_str().unwrap_or_default(),
            at_line(row),
            dead_name(row),
            row["reason"].as_str().unwrap_or_default(),
            match row["caveat"].as_str().unwrap_or_default() {
                "" => String::new(),
                why => format!("   (lower confidence: {why})"),
            }
        );
    }
    if !found {
        println!("no candidates in {scope} file(s)");
        return Ok(exit_on(found));
    }
    let tiers: Vec<String> = DEAD_TIERS
        .iter()
        .map(|tier| (tier, summary["tiers"][tier].as_u64().unwrap_or(0)))
        .filter(|(_, n)| *n > 0)
        .map(|(tier, n)| format!("{n} {tier}"))
        .collect();
    println!(
        "\n{} candidates in {scope} file(s): {} ({} clear, {} lower)",
        rows.len(),
        tiers.join(", "),
        summary["confidence"]["clear"],
        summary["confidence"]["lower"],
    );
    Ok(exit_on(found))
}

/// `--dead`'s tiers, from the least evidence of use to the most.
const DEAD_TIERS: [&str; 5] = [
    "unreferenced",
    "override",
    "convention-only",
    "super-only",
    "single-caller",
];

/// How many candidates in each tier, and at each confidence. Every tier is
/// present, so a script reads a zero rather than a missing key.
fn dead_summary(rows: &[serde_json::Value]) -> serde_json::Value {
    let count = |key: &str, value: &str| rows.iter().filter(|row| row[key] == value).count();
    let tiers: serde_json::Map<String, serde_json::Value> = DEAD_TIERS
        .iter()
        .map(|tier| (tier.to_string(), count("tier", tier).into()))
        .collect();
    serde_json::json!({
        "candidates": rows.len(),
        "tiers": tiers,
        "confidence": { "clear": count("confidence", "clear"), "lower": count("confidence", "lower") },
    })
}

/// A candidate as Ruby's documentation names it: `Widget#save`, or
/// `Widget.build` for a method on the singleton.
fn dead_name(row: &serde_json::Value) -> String {
    let name = row["name"].as_str().unwrap_or_default();
    match row["owner"].as_str().unwrap_or_default() {
        "" => name.to_string(),
        owner if row["singleton"] == true => format!("{owner}.{name}"),
        owner => format!("{owner}#{name}"),
    }
}

/// `--dead` over the scopes in one checkout, weighed against that checkout:
/// pushes a row per candidate and returns how many files were in scope.
fn dead_in(
    store: &Store,
    root: &Path,
    paths: &[PathBuf],
    rows: &mut Vec<serde_json::Value>,
) -> anyhow::Result<usize> {
    use crate::resolve::refs;

    let root_str = root.to_string_lossy().into_owned();
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
        let mut risky = dynamic_markers(&source);
        // A string of code not read: its calls are not in the index (DEC-132).
        let unread = facts.ancestry.iter().any(|edge| {
            edge.relation == crate::core::Relation::Dynamic
                && crate::core::Maker::parse(&edge.target)
                    .by
                    .ends_with(" string")
        });
        if unread {
            if !risky.is_empty() {
                risky.push_str(", ");
            }
            risky.push_str("class_eval string");
        }
        let at = file.to_string_lossy().into_owned();
        let unread_calls = facts.unread_calls;
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
            // A string of code calls a name of its shape that it spells only
            // in part, which no call site records (DEC-163).
            let mut caveat = risky.clone();
            if let Some(shape) = unread_calls
                .iter()
                .find(|shape| crate::core::shape_matches(shape, &def.name))
            {
                if !caveat.is_empty() {
                    caveat.push_str(", ");
                }
                caveat.push_str(&format!("a string of code calls `{shape}`"));
            }
            defined.push((at.clone(), def, caveat));
        }
    }

    let names: Vec<String> = defined.iter().map(|(_, d, _)| d.name.clone()).collect();
    // More written calls than this and a name is plainly used.
    const PLAINLY_USED: i64 = 8;
    let written_calls = store.written_calls(&root_str, &names, PLAINLY_USED + 1)?;

    // The expensive pass, only for names the cheap one could not clear.
    let tree = build_tree(store, &root_str)?;
    let mut parsed = Parsed::new();
    for (file, def, risky) in &defined {
        let written = written_calls.get(&def.name).copied().unwrap_or(0);
        if written > PLAINLY_USED {
            continue; // not worth a narrowed search
        }
        // The class it is, not the name as written: `Helpers` inside `module
        // Alpha` is `Alpha::Helpers`, and that is what a resolved call names.
        let owner = tree
            .scope_fqn(&def.nesting)
            .or_else(|| def.nesting.first().cloned())
            .unwrap_or_default();
        let query = refs::Query {
            owner: Some(owner.clone()),
            singleton: def.singleton,
            name: def.name.clone(),
        };
        let (found, counts) = gather_refs(
            &tree,
            store,
            root,
            &root_str,
            &query,
            Some(&owner),
            false,
            Some(&mut parsed),
            None,
        )
        .unwrap_or_default();
        let live = refs::liveness(&found, &counts);
        let Some(tier) = live.tier else { continue };
        // Whoever calls the method this overrides may run it instead, and
        // that is often a framework the checkout never names (DEC-121).
        let overrides = crate::resolve::overridden(&tree, def, file);
        let tier = match tier {
            "unreferenced" if !overrides.is_empty() => "override",
            tier => tier,
        };
        // The one written call a single caller has: whether it certainly
        // reaches this method is the difference between inlining it and
        // checking an untyped receiver first.
        let caller = (tier == "single-caller")
            .then(|| found.iter().find(|r| refs::is_written_call(r)))
            .flatten()
            .map(|r| {
                serde_json::json!({
                    "path": format!("{root_str}/{}", r.path),
                    "line": r.line,
                    "col": r.col,
                    "tier": r.tier,
                })
            });
        // Its only evidence of use is a call that may be another method's:
        // that is weaker than a clear single caller, and says why.
        let mut risky = risky.clone();
        if caller.as_ref().is_some_and(|c| c["tier"] == "possible") {
            if !risky.is_empty() {
                risky.push_str(", ");
            }
            risky.push_str("untyped caller");
        }
        if tier != "override" && !overrides.is_empty() {
            if !risky.is_empty() {
                risky.push_str(", ");
            }
            risky.push_str(&format!("overrides {}", overrides.join(", ")));
        }
        let reason = match (tier, &caller) {
            ("unreferenced", _) => "no call, symbol or `super` names it".to_string(),
            ("override", _) => format!(
                "no call names it, but it overrides {}, so a call of that may run it",
                overrides.join(", ")
            ),
            ("convention-only", _) => format!(
                "named only by a symbol handed to a macro ({})",
                live.by_symbol
            ),
            ("super-only", _) => format!(
                "reached only by `super` from {}",
                live.super_from.join(", ")
            ),
            (_, Some(caller)) if caller["tier"] == "confirmed" => {
                format!("one call, at {}", at_line(caller))
            }
            (_, Some(caller)) => format!(
                "one possible call, at {}: its receiver is untyped",
                at_line(caller)
            ),
            _ => String::new(),
        };
        let mut row = serde_json::json!({
            "name": def.name,
            "owner": owner,
            "singleton": def.singleton,
            // Whether deleting it could break a caller outside the checkout.
            "visibility": def.visibility.as_str(),
            "path": file,
            "line": def.pos.line,
            "col": def.pos.col,
            "end_line": def.end_line,
            "tier": tier,
            "confirmed": counts.confirmed,
            "possible": counts.possible,
            "symbol_refs": live.by_symbol,
            "super_refs": live.by_super,
            "super_from": live.super_from,
            "mentions_by_name": written,
            "overrides": overrides,
            "confidence": if risky.is_empty() && tier != "override" { "clear" } else { "lower" },
            "caveat": risky,
            "reason": reason,
        });
        if let Some(caller) = caller {
            row["caller"] = caller;
        }
        rows.push(row);
    }
    Ok(files.len())
}

/// One pass does not cascade: a method whose only caller is itself a
/// candidate is `single-caller`, not `unreferenced`. Say so on the row,
/// where the next question is asked.
fn note_candidate_callers(rows: &mut [serde_json::Value]) {
    let spans: Vec<(String, u64, u64, String)> = rows
        .iter()
        .map(|row| {
            (
                row["path"].as_str().unwrap_or_default().to_string(),
                row["line"].as_u64().unwrap_or(0),
                row["end_line"].as_u64().unwrap_or(0),
                row["name"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    for row in rows.iter_mut() {
        let caller = &row["caller"];
        let (Some(path), Some(line)) = (caller["path"].as_str(), caller["line"].as_u64()) else {
            continue;
        };
        let within = spans
            .iter()
            .find(|(p, start, end, _)| p == path && (*start..=*end).contains(&line));
        if let Some((_, _, _, name)) = within {
            let reason = format!(
                "{}; its caller, {name}, is itself a candidate",
                row["reason"].as_str().unwrap_or_default()
            );
            row["reason"] = reason.into();
        }
    }
}

/// `path:line` of a located JSON object, as text shows a path.
fn at_line(site: &serde_json::Value) -> String {
    format!(
        "{}:{}",
        shown(site["path"].as_str().unwrap_or_default()),
        site["line"]
    )
}

/// Ruby files under these paths, each once: the same file named twice — a
/// path repeated, a directory and a file in it, a symlink and its target —
/// is one file in scope, and would otherwise be two candidates.
fn ruby_files(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    ruby_files_under(paths)
        .into_iter()
        .map(|file| std::fs::canonicalize(&file).unwrap_or(file))
        .filter(|file| seen.insert(file.clone()))
        .collect()
}

/// Ruby files under these paths, following directories one level of recursion.
fn ruby_files_under(paths: &[PathBuf]) -> Vec<PathBuf> {
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
                found.extend(ruby_files_under(&[child]));
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

/// Why the store holds nothing, when an upgrade emptied it.
fn upgrade_reason(from: i64) -> String {
    format!(
        "trekr's index format changed (store v{from} to v{}), which dropped any \
         earlier index; nothing has been indexed since",
        crate::store::VERSION
    )
}

/// Why a checkout is not indexed, and whether an upgrade is the reason.
///
/// A store rebuilt for a new schema looks exactly like one never used, and
/// "never indexed" to someone who indexed yesterday reads as a bug. Only until
/// the first index after it: from then on "not indexed" is about this
/// checkout, not the upgrade, and a checkout nobody ever indexed would be told
/// an index of it was dropped.
fn not_indexed_reason(store: &Store) -> anyhow::Result<(String, bool)> {
    let upgraded = match store.roots()?.is_empty() {
        true => store.upgraded_from()?,
        false => None,
    };
    Ok(match upgraded {
        Some(from) => (upgrade_reason(from), true),
        None => (
            "this checkout has never been indexed, so there is nothing to answer from".into(),
            false,
        ),
    })
}

/// A checkout nobody has indexed, when a query needs one.
///
/// Worth its own answer because the alternative is a lie by omission: an empty
/// tree resolves nothing, so the query came back `residue` — "no indexed
/// constant by that name" — which reads as *we looked and Ruby does not have
/// it* when the truth is *nobody has looked yet*. One is a finding about the
/// code, the other is a setup step, and they call for opposite reactions.
fn not_indexed(out: Output, root: &Path, store: &Store) -> anyhow::Result<ExitCode> {
    crate::usage::outcome(Outcome::NotIndexed);
    let root = root.to_string_lossy().to_string();
    let hint = format!("trekr --index {}", paths::pretty(&root));
    let (reason, upgraded) = not_indexed_reason(store)?;
    match out {
        Output::Text if upgraded => eprintln!(
            "trekr: {} is not indexed — {reason}. Run: {hint}",
            paths::pretty(&root)
        ),
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
                "reason": reason,
                "hint": hint,
            }),
        )?,
    }
    // Exit 2, not 1: `1` is this tool's "looked, found nothing" and would tell a
    // script the question was answered. It was not asked (DEC-067).
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
    let absolute = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    // Before git: a git gem's checkout is a clone of its own (DEC-150).
    if let Some(gem) = store.gem_containing(&absolute.to_string_lossy())? {
        return Ok(PathBuf::from(store.app_for_gem(&gem)?.unwrap_or(gem)));
    }
    if let Ok(root) = scan::repo_root(path) {
        return Ok(root);
    }
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

/// The checkout we are standing in, or the one `--context` names — for the
/// queries that ask about a name rather than a position.
fn tree_here(context: Option<&Path>) -> anyhow::Result<(PathBuf, Store, OneShotTree)> {
    let dir = context.unwrap_or(Path::new("."));
    if !dir.exists() {
        return Err(Failure::NotFound.error(format!("no such path: {}", dir.display())));
    }
    tree_for(dir, None)
}

fn cmd_def(
    out: Output,
    spec: &str,
    explain: bool,
    pinned: Option<&Path>,
) -> anyhow::Result<ExitCode> {
    let written = spec;
    let spec = position::Spec::parse(spec)
        .ok_or_else(|| Failure::Usage.error(format!("expected FILE:LINE:COL, got `{spec}`")))?;
    if let Some(why) = spec.out_of_range(written) {
        return Err(Failure::Usage.error(why));
    }
    let source = read_input(Path::new(&spec.path))?;
    let facts = crate::extract::extract(&source);
    // The file as a site names it, whatever directory the question came from.
    let file = std::fs::canonicalize(&spec.path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| spec.path.clone());
    // Branches that never build a tree still write their paths against the
    // file's checkout.
    if let Ok((root, store)) = checkout_for_query(Path::new(&spec.path), pinned)
        && store.has_checkout(&root.to_string_lossy()).unwrap_or(false)
    {
        answering_in(&store, &root.to_string_lossy());
    }
    // A `super` with no fact behind it is one whose method has no owner the
    // source names. Snapping would answer for another name on the line.
    if position::at_facts(&facts, spec.line, spec.col).is_none()
        && position::word_at(&source, spec.line, spec.col).as_deref() == Some("super")
    {
        return report(
            out,
            serde_json::json!({
                "query": written,
                "under": "call",
                "name": "super",
                "receiver": "super",
                "status": "residue",
                "confidence": 0.0,
                "definition": [],
                "reason": "`super` in a method whose owner the source does not name — \
                           outside a method, `def obj.x`, or a `def` inside a block",
            }),
            false,
            "super  its method's owner is decided at runtime",
        );
    }
    // A variable is not a call, and snapping from one answered for whatever
    // name was nearest on the line.
    if spec.col > 0
        && position::at_facts(&facts, spec.line, spec.col).is_none()
        && let Some(answer) = position::variable_at(&source, &file, spec.line, spec.col)
    {
        crate::usage::flag("variable");
        let mut answer = answer;
        answer["query"] = written.into();
        let resolved = answer["status"] == "resolved";
        let text = match answer["definition"].get(0) {
            Some(site) => format!(
                "{}:{}:{}  {} `{}`",
                shown(site["path"].as_str().unwrap_or_default()),
                site["line"],
                site["col"],
                answer["variable"].as_str().unwrap_or_default(),
                answer["name"].as_str().unwrap_or_default(),
            ),
            None => format!(
                "{}  {}",
                answer["name"].as_str().unwrap_or_default(),
                answer["reason"].as_str().unwrap_or_default(),
            ),
        };
        return report(out, answer, resolved, &text);
    }
    let snapped = position::at_or_snap(&facts, spec.line, spec.col);
    let Some((under, snapped)) = snapped else {
        return report(
            out,
            serde_json::json!({
                "query": written,
                "status": "residue",
                "confidence": 0.0,
                "definition": [],
                "reason": "no name at this position",
            }),
            false,
            "nothing at that position",
        );
    };

    let query = written.to_string();
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
            "definition": [{
                "path": file, "line": def.pos.line,
                "col": def.pos.col, "kind": def.kind.as_str(),
            }],
        }),
        position::Under::Constant(reference) => {
            let (root, mut store) = checkout_for_query(Path::new(&spec.path), pinned)?;
            if !store.has_checkout(&root.to_string_lossy())? {
                return not_indexed(out, &root, &store);
            }
            answering_in(&store, &root.to_string_lossy());
            // Refresh before the tree is built, so the tree sees the new facts.
            freshness = refresh_for_query(&mut store, &root, Path::new(&spec.path));
            let tree = build_tree(&store, &root.to_string_lossy())?;
            context = Some(root.to_string_lossy().into_owned());
            let relative = std::fs::canonicalize(&spec.path)
                .ok()
                .and_then(|abs| abs.strip_prefix(&root).ok().map(Path::to_path_buf))
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| spec.path.clone());
            let resolution = tree.resolve_at(&reference.name, &reference.nesting, &relative);
            let mut value = serde_json::to_value(&resolution)?;
            let object = value.as_object_mut().expect("resolution is an object");
            object.insert("query".into(), query.clone().into());
            object.insert("under".into(), "constant".into());
            object.insert("name".into(), reference.name.clone().into());
            if resolution.status == Status::Residue {
                let reason = if crate::core::rspec::is_shared_module(&reference.name) {
                    "no top-level `shared_examples` or `shared_context` by this name is \
                     indexed; one written inside a group is not followed"
                } else {
                    "no indexed constant by that name; it may belong to a gem \
                     or be defined at runtime"
                };
                object.insert("reason".into(), reason.into());
            }
            value
        }
        position::Under::Call(call) => {
            let (root, mut store) = checkout_for_query(Path::new(&spec.path), pinned)?;
            if !store.has_checkout(&root.to_string_lossy())? {
                return not_indexed(out, &root, &store);
            }
            answering_in(&store, &root.to_string_lossy());
            // Refresh before the tree is built, so the tree sees the new facts.
            freshness = refresh_for_query(&mut store, &root, Path::new(&spec.path));
            let tree = build_tree(&store, &root.to_string_lossy())?;
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
    if freshness.is_some() {
        crate::usage::flag("stale");
    }
    if snapped.is_some() {
        crate::usage::flag("snapped");
    }
    if let (Output::Text, Some(freshness)) = (out, &freshness) {
        match (freshness["refreshed"].as_str(), freshness["busy"].as_str()) {
            (Some(file), _) => eprintln!("trekr: {file} changed since the index — re-read it"),
            (None, Some(file)) => eprintln!(
                "trekr: {file} changed since the index, which another trekr is writing — \
                 answered from its indexed version"
            ),
            (None, None) => {
                eprintln!("trekr: the checkout moved since the index; other files may lag")
            }
        }
    }
    let resolved = answer["status"] == "resolved" || answer["status"] == "ambiguous";
    let text = match answer["definition"].as_array().and_then(|s| s.first()) {
        Some(site) => format!(
            "{}:{}:{}  {}",
            shown(site["path"].as_str().unwrap_or_default()),
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
    // Beside the answer, not on stderr: an answer about a name the caller
    // did not point at has to say so where it is read.
    let text = match &snapped {
        Some(snapped) => {
            let others = match snapped.alternatives.len() {
                0 => String::new(),
                n => format!(
                    " ({n} other name{} on that line)",
                    if n == 1 { "" } else { "s" }
                ),
            };
            let why = match spec.col {
                0 => "no column given".to_string(),
                col => format!("no name at column {col}"),
            };
            format!(
                "{text}\n  snapped_to  `{}` at column {}: {why}{others}",
                snapped.name, snapped.col
            )
        }
        None => text,
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
    // A receiver that is itself a call was typed by what that call returns;
    // its syntactic shape ("other") says nothing a reader can use.
    let chained = field("resolved_via").filter(|via| via.starts_with("chain"));
    match (
        field("receiver"),
        field("receiver_type"),
        chained.as_deref(),
    ) {
        (_, Some(typed), Some("chain:name")) => {
            let share = field("agreement")
                .map(|a| format!(" ({a} of them declare one)"))
                .unwrap_or_default();
            out.push(format!(
                "  receiver    a call → {typed}: its receiver is untyped, and every indexed \
                 method of that name that declares a return type returns {typed}{share}"
            ));
        }
        (_, Some(typed), Some(_)) => out.push(format!(
            "  receiver    a call → {typed}, by the return type that method declares"
        )),
        (Some(receiver), typed, _) => {
            let typed = typed.map(|t| format!(" → {t}")).unwrap_or_default();
            out.push(format!("  receiver    {receiver}{typed}"));
        }
        _ => {}
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
                shown(site["path"].as_str().unwrap_or_default()),
                site["line"],
                candidate["why"].as_str().unwrap_or_default(),
            ));
        }
    }
    out.join("\n")
}

fn cmd_ancestors(out: Output, name: &str, context: Option<&Path>) -> anyhow::Result<ExitCode> {
    let (root, store, tree) = tree_here(context)?;
    if !store.has_checkout(&root.to_string_lossy())? {
        return not_indexed(out, &root, &store);
    }
    answering_in(&store, &root.to_string_lossy());
    let resolution = tree.resolve(name, &[]);
    let Some(fqn) = resolution.fqn.clone() else {
        return report(
            out,
            serde_json::json!({
                "query": name,
                "status": "residue",
                "confidence": 0.0,
                "scopes_tried": resolution.scopes_tried,
            }),
            false,
            &format!("no indexed constant named {name}"),
        );
    };
    let chain = tree.ancestors(&fqn);
    let variants = tree.variants_of(&fqn);
    if !variants.is_empty() {
        return split_ancestors(out, &tree, name, &fqn, &variants);
    }
    let ancestors = public_chain(&chain.chain);
    let text = ancestors.join("\n");
    report(
        out,
        serde_json::json!({
            "query": name,
            "status": "resolved",
            "fqn": fqn,
            "ancestors": ancestors,
            "unresolved_ancestors": chain.unresolved,
        }),
        true,
        &text,
    )
}

/// `--ancestors` on a name two programs declare with different superclasses:
/// one chain per declaration, and none of them promoted (DEC-072).
fn split_ancestors(
    out: Output,
    tree: &Tree,
    name: &str,
    fqn: &str,
    variants: &[String],
) -> anyhow::Result<ExitCode> {
    let split = Split::of(tree, fqn, variants);
    report(
        out,
        serde_json::json!({
            "query": name,
            "status": "ambiguous",
            "fqn": fqn,
            "ancestors": [fqn],
            "unresolved_ancestors": split.unresolved,
            "variants": split.listed,
        }),
        true,
        &split.text.join("\n"),
    )
}

/// A split name's declarations, one chain each (DEC-072).
struct Split {
    listed: Vec<serde_json::Value>,
    /// What no variant's chain could resolve. The name's own chain lists the
    /// competing superclasses as unresolved, which is internal bookkeeping:
    /// each one resolves in its variant.
    unresolved: Vec<String>,
    text: Vec<String>,
}

impl Split {
    fn of(tree: &Tree, fqn: &str, variants: &[String]) -> Split {
        let mut split = Split {
            listed: Vec::new(),
            unresolved: Vec::new(),
            text: vec![format!(
                "{fqn} is {} different classes, declared in separate files:",
                variants.len()
            )],
        };
        for variant in variants {
            let chain = tree.ancestors(variant);
            for missing in &chain.unresolved {
                if !split.unresolved.contains(missing) {
                    split.unresolved.push(missing.clone());
                }
            }
            let ancestors = public_chain(&chain.chain);
            let sites = tree.sites(variant);
            split.text.push(String::new());
            for site in &sites {
                split.text.push(format!(
                    "  {}:{}:{}",
                    shown(&site.path),
                    site.line,
                    site.col
                ));
            }
            split.text.push(format!("  {}", ancestors.join(" < ")));
            split.listed.push(serde_json::json!({
                "definition": sites,
                "ancestors": ancestors,
                "unresolved_ancestors": chain.unresolved,
            }));
        }
        split
    }
}

/// An ancestor chain as a person reads it: a split name's variant is its name.
fn public_chain(chain: &[String]) -> Vec<String> {
    chain
        .iter()
        .map(|name| crate::tree::public_name(name).to_string())
        .collect()
}

/// One answer, in whichever shape the caller asked for.
fn report(
    out: Output,
    value: serde_json::Value,
    matched: bool,
    text: &str,
) -> anyhow::Result<ExitCode> {
    // Ambiguous is an answer, so it exits 0 — which is exactly why the count
    // has to look past the exit code to see it.
    let confidence = value["confidence"].as_f64().unwrap_or(1.0);
    let guesses = value["candidates"]
        .as_array()
        .is_some_and(|c| !c.is_empty());
    crate::usage::outcome(match value["status"].as_str() {
        // Residue with ranked guesses is not nothing: it is the least certain
        // answer there is, and the LSP counts it the same way.
        _ if !matched && guesses => Outcome::Uncertain,
        _ if !matched => Outcome::Empty,
        Some("ambiguous") => Outcome::Uncertain,
        _ if confidence < crate::usage::LOW_CONFIDENCE => Outcome::Uncertain,
        _ => Outcome::Hit,
    });
    match out {
        Output::Text => println!("{text}"),
        _ => emit_json(out, &value)?,
    }
    Ok(exit_on(matched))
}

fn cmd_drop(out: Output, path: &Path) -> anyhow::Result<ExitCode> {
    let store = open_store()?;
    store.wait_as_writer(writer_waiting)?;
    // A gem is no git checkout, and is dropped by the directory it was
    // indexed under; the next index of an app that uses it reads it again.
    let root = match named_checkout(path) {
        Err(e) if Failure::of(&e) == Failure::NotARepo => match gem_holding(&store, path) {
            Some(gem) => PathBuf::from(gem),
            None => return Err(e),
        },
        root => root?,
    };
    let root_str = root.to_string_lossy().into_owned();
    let dropped = store.drop_checkout(&root_str)?;
    crate::tree::forget_snapshots(&store, &root_str);

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

/// `36h`, `7d`, `2w` — or `0`. A bare number is refused: minutes and days
/// are both plausible readings, and guessing wrong deletes the wrong thing.
fn parse_age(text: &str) -> Result<u64, String> {
    if text == "0" {
        return Ok(0);
    }
    let split = text.len() - text.chars().last().map_or(0, char::len_utf8);
    let (count, unit) = text.split_at(split);
    let count: u64 = count
        .parse()
        .map_err(|_| format!("`{text}` is not an age like 36h, 7d or 2w"))?;
    let unit = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        "w" => 7 * 86_400,
        _ => return Err(format!("`{text}`: the unit is one of s, m, h, d, w")),
    };
    Ok(count * unit)
}

fn cmd_gc(out: Output, older_than: u64, dry_run: bool, vacuum: bool) -> anyhow::Result<ExitCode> {
    let mut store = open_store()?;
    store.wait_as_writer(writer_waiting)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs() as i64;
    let garbage = store.collect(
        now - older_than as i64,
        |root| Path::new(root).is_dir(),
        dry_run,
    )?;
    let gone: Vec<&str> = garbage.checkouts.iter().map(|c| c.repo.as_str()).collect();
    let snapshots = crate::tree::sweep_snapshots(&store, &gone, dry_run)?;
    // Each Ruby's core files beside the store, and an earlier build's.
    let db = crate::store::default_path()?;
    let mut core =
        crate::tree::sweep_core(&crate::store::core_dir_of(&db), &garbage.core_dirs, dry_run);
    if let Some(beside) = db.parent() {
        let legacy = crate::tree::sweep_legacy_core(beside, dry_run);
        core.files += legacy.files;
        core.bytes += legacy.bytes;
    }
    if vacuum {
        store.vacuum()?;
    }
    let db_bytes = store.db_bytes()?;
    let found = !garbage.checkouts.is_empty()
        || snapshots.files > 0
        || garbage.signatures > 0
        || core.files > 0;

    if out != Output::Text {
        emit_json(
            out,
            &serde_json::json!({
                "dry_run": dry_run,
                "older_than": older_than,
                "checkouts": garbage.checkouts,
                "files": garbage.files,
                "blobs": garbage.blobs,
                "facts": garbage.facts,
                "reclaimed_bytes": garbage.reclaimed_bytes,
                "snapshots": snapshots,
                "signatures": garbage.signatures,
                "core_files": core,
                "vacuumed": vacuum,
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
            "\n{verb} {} checkouts: {} files, {} blobs, {} facts, {:.1} MB",
            garbage.checkouts.len(),
            garbage.files,
            garbage.blobs,
            garbage.facts,
            mb(garbage.reclaimed_bytes)
        );
    }
    if snapshots.files > 0 {
        println!(
            "{verb} {} tree snapshots no checkout's index names any more: {:.1} MB",
            snapshots.files,
            mb(snapshots.bytes as i64)
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
    if vacuum {
        println!("vacuumed: database is {:.1} MB", mb(db_bytes));
    }
    Ok(exit_on(found))
}

/// 0 when something happened, 1 when nothing did — so a script can branch on it.
fn exit_on(happened: bool) -> ExitCode {
    if happened {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// Which features get used, by whom, how often they come back empty, and how
/// slow — the CLI's commands and the LSP's operations in one view (DEC-063).
///
/// Text is the summary; `--json`/`--ndjson` are the stored daily rows, the
/// evidence the summary is folded from.
fn cmd_usage(out: Output, days: Option<u32>) -> anyhow::Result<ExitCode> {
    // Off is not a mistake in the command line: nothing was recorded, which
    // is the same nothing as a counter that has not run yet.
    let Some(path) = crate::usage::path() else {
        let why = "usage counting is off (TREKR_USAGE=off), so nothing is recorded";
        match out {
            Output::Text => println!("{why}"),
            _ => {
                eprintln!("trekr: {why}");
                emit_rows(out, &[] as &[crate::usage::Row])?;
            }
        }
        return Ok(ExitCode::from(1));
    };
    let rows = crate::usage::read(&path, days)?;
    if emit_rows(out, &rows)? {
        return Ok(exit_on(!rows.is_empty()));
    }
    if rows.is_empty() {
        println!(
            "no usage recorded yet in {}",
            paths::pretty(&path.to_string_lossy())
        );
        return Ok(ExitCode::from(1));
    }
    print!("{}", usage_text(&rows, days));
    Ok(ExitCode::SUCCESS)
}

/// The editor's recent misses, oldest first — the positions behind `--usage`'s
/// `empty` and `unsure` columns (DEC-083).
fn cmd_misses(out: Output, days: Option<u32>) -> anyhow::Result<ExitCode> {
    let Some(log) = crate::serve::log::Log::where_to_look() else {
        let why = "the LSP log is off or on stderr (TREKR_LOG), so no misses are recorded";
        match out {
            Output::Text => println!("{why}"),
            _ => {
                eprintln!("trekr: {why}");
                emit_rows(out, &[] as &[crate::serve::miss::Recorded])?;
            }
        }
        return Ok(ExitCode::from(1));
    };
    let since = days.map(|n| crate::serve::log::days_ago(n.saturating_sub(1)));
    let misses = crate::serve::miss::read(&log, since.as_deref())?;
    if emit_rows(out, &misses)? {
        return Ok(exit_on(!misses.is_empty()));
    }
    if misses.is_empty() {
        println!(
            "no editor misses recorded in {}",
            paths::pretty(&log.to_string_lossy())
        );
        return Ok(ExitCode::from(1));
    }
    for miss in &misses {
        println!(
            "{}  {:<10} {:<9} {}:{}:{}  {}{}",
            &miss.ts[..miss.ts.len().min(16)],
            miss.op,
            miss.outcome,
            paths::pretty(&miss.file),
            miss.line,
            miss.col,
            if miss.token.is_empty() {
                "(no token)"
            } else {
                &miss.token
            },
            miss.why
                .as_deref()
                .map(|w| format!(" — {w}"))
                .unwrap_or_default(),
        );
    }
    println!(
        "\n{} misses, from {}",
        misses.len(),
        paths::pretty(&log.to_string_lossy())
    );
    Ok(ExitCode::SUCCESS)
}

/// LSP events that are the session's life rather than something asked of it.
const LIFECYCLE: [&str; 6] = [
    "session",
    "resume",
    "reload",
    "reload-failed",
    "retire",
    "index",
];

/// Fold daily rows into one line per feature, most used first.
fn usage_text(rows: &[crate::usage::Row], days: Option<u32>) -> String {
    use std::collections::BTreeMap;
    use std::fmt::Write;

    #[derive(Default)]
    struct Feature {
        uses: i64,
        outcomes: BTreeMap<String, i64>,
        /// Warm latency buckets; an LSP session's first request is kept apart
        /// because it measures the disk and a tree build, not the engine.
        latency: BTreeMap<String, i64>,
        cold: BTreeMap<String, i64>,
        callers: BTreeMap<String, i64>,
    }
    let mut features: BTreeMap<(String, String), Feature> = BTreeMap::new();
    let mut flags: BTreeMap<String, i64> = BTreeMap::new();
    for row in rows {
        let f = features
            .entry((row.surface.clone(), row.feature.clone()))
            .or_default();
        f.uses += row.count;
        *f.outcomes.entry(row.outcome.clone()).or_default() += row.count;
        *f.callers.entry(row.origin.clone()).or_default() += row.count;
        if !row.latency.is_empty() {
            let into = if row.cold {
                &mut f.cold
            } else {
                &mut f.latency
            };
            *into.entry(row.latency.clone()).or_default() += row.count;
        }
        for flag in row.flags.split(',').filter(|f| !f.is_empty()) {
            let key = format!("{} {flag}", row.feature);
            *flags.entry(key).or_default() += row.count;
        }
    }

    let first = rows
        .iter()
        .map(|r| r.day.as_str())
        .min()
        .unwrap_or_default();
    let last = rows
        .iter()
        .map(|r| r.day.as_str())
        .max()
        .unwrap_or_default();
    let window = match days {
        Some(n) => format!("last {n} day{}", if n == 1 { "" } else { "s" }),
        None => format!("up to {} days kept", crate::usage::RETENTION_DAYS),
    };
    let mut text = format!("trekr usage, {window}: {first} — {last}\n");

    let count = |f: &Feature, outcome: &str| f.outcomes.get(outcome).copied().unwrap_or(0);
    for (surface, title) in [("cli", "command line"), ("lsp", "editor (--lsp)")] {
        let mut shown: Vec<(&String, &Feature)> = features
            .iter()
            .filter(|((s, name), _)| {
                s == surface && !(s == "lsp" && LIFECYCLE.contains(&name.as_str()))
            })
            .map(|((_, name), f)| (name, f))
            .collect();
        // An editor that only opened and closed still has sessions to report;
        // skipping it printed a bare heading, a summary of nothing.
        let lifecycle = surface == "lsp"
            && features
                .keys()
                .any(|(s, name)| s == "lsp" && LIFECYCLE.contains(&name.as_str()));
        if shown.is_empty() && !lifecycle {
            continue;
        }
        shown.sort_by(|a, b| b.1.uses.cmp(&a.1.uses).then_with(|| a.0.cmp(b.0)));
        let _ = writeln!(
            text,
            "\n{:<22}{:>6}{:>6}{:>8}{:>7}{:>7}  {:<8}{:<8}callers",
            title, "uses", "hit", "unsure", "empty", "error", "median", "p90"
        );
        for (name, f) in &shown {
            let hit = count(f, "hit");
            let unsure = count(f, "uncertain");
            let empty = count(f, "empty");
            let _ = writeln!(
                text,
                "{:<22}{:>6}{:>6}{:>8}{:>7}{:>7}  {:<8}{:<8}{}",
                name,
                f.uses,
                hit,
                unsure,
                empty,
                f.uses - hit - unsure - empty,
                bucket_at(&f.latency, 0.5),
                bucket_at(&f.latency, 0.9),
                ranked(&f.callers),
            );
        }
        // What the error column holds, so "error" is never a dead end.
        let mut failures: BTreeMap<&str, i64> = BTreeMap::new();
        for (_, f) in &shown {
            for (outcome, n) in &f.outcomes {
                if !matches!(outcome.as_str(), "hit" | "uncertain" | "empty") {
                    *failures.entry(outcome.as_str()).or_default() += n;
                }
            }
        }
        if !failures.is_empty() {
            let mut failures: Vec<(String, i64)> = failures
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect();
            failures.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            let list: Vec<String> = failures.iter().map(|(k, v)| format!("{k} {v}")).collect();
            let _ = writeln!(text, "  errors: {}", list.join(" · "));
        }
        if surface == "lsp" {
            let mut cold: BTreeMap<String, i64> = BTreeMap::new();
            for (_, f) in &shown {
                for (bucket, n) in &f.cold {
                    *cold.entry(bucket.clone()).or_default() += n;
                }
            }
            let n: i64 = cold.values().sum();
            if n > 0 {
                let _ = writeln!(
                    text,
                    "  a session's first request: median {} (n={n}), kept out of the columns above",
                    bucket_at(&cold, 0.5)
                );
            }
            let life = |name: &str| {
                features
                    .get(&("lsp".to_string(), name.to_string()))
                    .map(|f| (f.uses, f.uses - count(f, "hit")))
                    .unwrap_or((0, 0))
            };
            let (sessions, _) = life("session");
            let (resumed, _) = life("resume");
            let (reloads, _) = life("reload");
            let (reload_failed, _) = life("reload-failed");
            let (retired, _) = life("retire");
            let (indexes, index_failed) = life("index");
            let _ = writeln!(
                text,
                "  sessions {sessions} · resumed after a hot reload {resumed} · reloads {reloads} \
                 (failed {reload_failed}) · retired {retired} · background indexes {indexes} \
                 (failed {index_failed})"
            );
        }
    }

    if !flags.is_empty() {
        let mut flags: Vec<(String, i64)> = flags.into_iter().collect();
        flags.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let list: Vec<String> = flags.iter().map(|(k, v)| format!("{k} {v}")).collect();
        let _ = writeln!(text, "\nflags and variants: {}", list.join(" · "));
    }
    text
}

/// The latency bucket holding the `q` quantile of these counts.
fn bucket_at(counts: &std::collections::BTreeMap<String, i64>, q: f64) -> &'static str {
    let total: i64 = counts.values().sum();
    if total == 0 {
        return "—";
    }
    let wanted = (total as f64 * q).ceil().max(1.0) as i64;
    let mut seen = 0;
    for bucket in crate::usage::BUCKETS {
        seen += counts.get(bucket).copied().unwrap_or(0);
        if seen >= wanted {
            return bucket;
        }
    }
    "—"
}

/// `claude-code 205 · human 7`, most first.
fn ranked(counts: &std::collections::BTreeMap<String, i64>) -> String {
    let mut sorted: Vec<(&String, &i64)> = counts.iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    sorted
        .iter()
        .map(|(k, v)| format!("{k} {v}"))
        .collect::<Vec<_>>()
        .join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_output_mode_is_read_off_argv_before_clap_parses_it() {
        let mode = |args: &[&str]| requested_output(args.iter().map(std::ffi::OsString::from));
        for (args, want) in [
            (&["x", "--json"][..], Output::Json),
            (&["x", "--ndjson"], Output::Ndjson),
            (&["-j", "--bogus"], Output::Json),
            (&["-jJ"], Output::Ndjson),
            (&["x"], Output::Text),
            (&["--", "-j"], Output::Text),
            (&["--refs", "Widget#save", "--jobs", "2"], Output::Text),
        ] {
            assert!(mode(args) == want, "{args:?}");
        }
    }

    #[test]
    fn a_chained_receiver_is_explained_by_its_return_type_not_its_shape() {
        let by_name = serde_json::json!({
            "status": "resolved", "resolved_via": "chain:name",
            "receiver": "other", "receiver_type": "String",
        });
        let text = explanation(&by_name);
        assert!(text.contains("every indexed method of that name"), "{text}");
        assert!(!text.contains("other →"), "{text}");
        let by_sig = serde_json::json!({
            "status": "resolved", "resolved_via": "chain",
            "receiver": "local", "receiver_type": "Item",
        });
        assert!(explanation(&by_sig).contains("a call → Item, by the return type"));
    }

    /// A listing written row by row is the answer `emit_json` would print
    /// whole: its key in the sorted place, every row as its `Value`.
    #[test]
    fn a_listing_prints_the_bytes_of_the_whole_answer() {
        #[derive(serde::Serialize)]
        struct Row {
            path: String,
            line: u32,
            why: Option<&'static str>,
            nested: Vec<(u8, &'static str)>,
        }
        let rows: Vec<Row> = (0..3)
            .map(|i| Row {
                path: format!("lib/f{i}.rb"),
                line: i,
                why: (i == 1).then_some("because"),
                nested: vec![(i as u8, "x")],
            })
            .collect();
        for rows in [&rows[..], &[]] {
            let answer = serde_json::json!({
                "zeta": 1, "alpha": {"path": "a.rb"}, "method": "save", "counts": {"b": 1, "a": 2},
            });
            let mut whole = answer.clone();
            whole["references"] = serde_json::to_value(rows).unwrap();
            rooted(&mut whole);
            // Under `--ndjson`, each row of it and then the rest (DEC-290).
            let mut streamed = String::new();
            for row in whole["references"].as_array().unwrap() {
                streamed.push_str(&format!("{}\n", serde_json::to_string(row).unwrap()));
            }
            let mut head = whole.clone();
            head.as_object_mut().unwrap().remove("references");
            head["rows"] = rows.len().into();
            let answer_line = serde_json::json!({ "answer": head });
            streamed.push_str(&format!(
                "{}\n",
                serde_json::to_string(&answer_line).unwrap()
            ));
            for (out, want) in [
                (
                    Output::Json,
                    format!("{}\n", serde_json::to_string_pretty(&whole).unwrap()),
                ),
                (Output::Ndjson, streamed),
            ] {
                let mut listed = answer.clone();
                listed["references"] = serde_json::Value::Null;
                let mut got = Vec::new();
                render_listing(out, listed, "references", rows, &mut got).unwrap();
                assert_eq!(String::from_utf8(got).unwrap(), want);
            }
            let mut got = Vec::new();
            render_json(Output::Json, &Rows(rows), &mut got).unwrap();
            let want = serde_json::to_string_pretty(&serde_json::to_value(rows).unwrap()).unwrap();
            assert_eq!(String::from_utf8(got).unwrap(), format!("{want}\n"));
        }
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
                let mut parsed = Parsed::new();
                let mut sites = Vec::new();
                let (found, counts) = gather_refs(
                    &tree,
                    &store,
                    &repo,
                    &root,
                    &query,
                    Some("Widget"),
                    true,
                    keep.then_some(&mut parsed),
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

    #[test]
    fn an_editor_that_only_opened_still_has_sessions_to_report() {
        let row = |feature: &str| crate::usage::Row {
            day: "2026-01-01".into(),
            surface: "lsp".into(),
            feature: feature.into(),
            flags: String::new(),
            origin: "vscode".into(),
            outcome: "hit".into(),
            latency: String::new(),
            cold: false,
            count: 2,
        };
        let text = usage_text(&[row("session")], None);
        assert!(text.contains("editor (--lsp)"), "{text}");
        assert!(text.contains("sessions 2"), "{text}");
    }
}
