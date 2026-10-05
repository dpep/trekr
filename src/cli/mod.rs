//! The command line. Every command that prints anything honors `--json` and
//! `--ndjson`, because the primary consumer is an agent, not a person.
//!
//! Operations are flags rather than subcommands (rq's convention): no word is
//! reserved, and the default action stays free for the query verbs the resolve
//! layer will add.

pub(crate) mod autoindex;
mod built;
mod config;
mod conventions;
mod dead;
mod dead_consts;
mod generated;
mod incomplete;
mod members;
mod position;
mod profile;
mod routes;
mod views;

use crate::failure::{Failure, Tag};
use autoindex::{Need, Then};

use crate::core::Oid;
use crate::core::paths;
use crate::resolve::DefinedOn;
use crate::store::Store;
use crate::tree::{Status, Tree, public_name};
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
        2   no answer yet: a miss while this checkout's first index is still running\n      \
        (`warming`: ask again), an index that could not finish (`incomplete`), or\n      \
        with --no-index a checkout nobody indexed (`not_indexed`)\n  \
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
        TREKR_JOBS   parse threads, as --jobs\n  \
        TREKR_NO_INDEX  never index from a query, as --no-index"
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
    /// use — candidates for deletion or inlining, graded, never asserted:
    /// methods, then an example group's `let`s, `subject`s and `def`s (`kind`
    /// `let`, `subject`, `method`, with `group`), which no example, hook,
    /// included shared group or helper reads as RSpec runs it (`let!` never),
    /// then classes, modules and constants, which no
    /// constant reference resolves to. Each is in one tier, from the least
    /// evidence of use to the most: `unreferenced` (no call, symbol or
    /// `super` names it), `shadowed` (every call of its name lands on an
    /// override, a subclass's or a nested group's, `overridden_by`),
    /// `test-only` (a class only tests name), `override` (none
    /// does, but it overrides an ancestor's method, so a call of that may run
    /// it), `convention-only` (named only by a symbol handed to a macro, a
    /// route, or a name Rails or a library looks a class up by),
    /// `super-only` (reached only by `super` from its overrides), and
    /// `single-caller` (one call: the inlining candidate). Each is `clear`, or
    /// `lower` confidence — a class, module or constant always, unless a
    /// convention names it, as one is not reliably unused — or when its
    /// `caveat` names a caller trekr cannot see —
    /// the file sends names dynamically, the one caller's receiver is
    /// untyped, it overrides a method, a view or a gem names it, the checkout
    /// builds a name of its shape, an ancestor is not indexed. One pass: a
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
    /// references, and call sites; `Owner#method` narrows them to the call
    /// sites that may reach that method, tiered. `FILE:LINE[:COL]` asks about
    /// what is at a position: a method's definition or a call of it, as
    /// `Owner#method`; a variable, with its mentions (a local's in its scope,
    /// an `@ivar`'s across its class's files); an example group's `let`,
    /// `subject` or `def`, with every read as RSpec runs it and where it was
    /// found (`from`). A column on none of these is a usage error.
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
    /// share them), and the copy of its edits queries keep in
    /// `trekr.overlays/` beside the index.
    #[arg(long, value_name = "PATH", num_args = 0..=1, default_missing_value = ".")]
    drop: Option<PathBuf>,

    /// Remove checkouts nothing will ask about again: gem versions no
    /// surviving project's bundle names, and projects whose root is gone.
    /// Their blobs go too, unless another checkout still maps them. Also
    /// removes an index set aside as unusable, an early copy a stopped index
    /// left, another trekr's own index once it is idle, and every copy of
    /// the working tree's edits queries keep in `trekr.overlays/` (the next
    /// query over edits writes its own again).
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

    /// Never index from a query. Without it, a query in a checkout no index
    /// has filled — never indexed, dropped by an upgrade, cut short —
    /// indexes it first: a position waits for its file and what that file
    /// names, and the rest is read in the background; any other question
    /// waits for the whole index. With it, the query answers from what is
    /// indexed, and a checkout nobody indexed is `not_indexed`, exit 2.
    /// `TREKR_NO_INDEX=1` sets it too; other commands ignore it, so it can
    /// stay exported around a `trekr --index`.
    #[arg(
        long,
        env = "TREKR_NO_INDEX",
        value_parser = clap::builder::FalseyValueParser::new()
    )]
    no_index: bool,

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

/// The CLI, handing `--lsp` to `lsp`: the crate root wires in the language
/// server, so neither front imports the other.
pub(crate) fn run(lsp: fn(bool) -> anyhow::Result<()>) -> ExitCode {
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
        crate::tree::profile();
    }
    if cli.no_index {
        autoindex::turn_off();
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
        (None, lsp(cli.profile).map(|()| ExitCode::SUCCESS))
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
        (Some("dead"), dead::cmd_dead(out, &cli.dead))
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
    // JSON carries it as `warming`; text says it once, after the answer.
    if out == Output::Text
        && result.is_ok()
        && let Some((root, warming)) = warming()
    {
        let root = paths::pretty(&root);
        match warming.interrupted {
            false => {
                eprintln!(
                    "trekr: {root} is still being indexed — {} of {} files read, so this answer may change",
                    warming.read, warming.of
                );
                if autoindex::left_running().is_some() {
                    eprintln!(
                        "trekr: the rest of {root} is being indexed in the background; later queries use it"
                    );
                }
            }
            true => eprintln!(
                "trekr: the index of {root} was cut short at {} of {} files, so answers are partial until: trekr --index {root}",
                warming.read, warming.of
            ),
        }
    }
    // After the answer is out, never before it (rq DECISIONS D13).
    if let Some(feature) = feature {
        let note = crate::usage::take();
        let mut flags = note.flags;
        flags.extend(cli_flags(&cli, out));
        let outcome = match (note.outcome, &result) {
            (Some(outcome), _) => outcome,
            (None, Ok(code)) if *code == ExitCode::SUCCESS => Outcome::Hit,
            (None, Ok(code)) if *code == ExitCode::from(1) => Outcome::Empty,
            (None, Ok(_)) if warming().is_some() => Outcome::NotIndexed,
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
        (cli.no_index, "no-index"),
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
/// so is a directory, a device or a file too large to be source where a
/// file was asked for.
fn read_input(path: &Path) -> anyhow::Result<Vec<u8>> {
    if path.is_dir() {
        return Err(Failure::Usage.error(format!(
            "{} is a directory; this asks about one file",
            path.display()
        )));
    }
    scan::read_source(path).map_err(|error| {
        let kind = match error.kind() {
            std::io::ErrorKind::NotFound => Failure::NotFound,
            std::io::ErrorKind::InvalidInput | std::io::ErrorKind::FileTooLarge => Failure::Usage,
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
    let db = crate::store::default_path().map_or_else(
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

fn open_store() -> anyhow::Result<Store> {
    // A query whose file is only in a first index's early store reads that
    // (DEC-512), until it is gone.
    if let Some(early) = autoindex::early_store() {
        match Store::open_existing(&early) {
            Ok(store) => return Ok(store),
            Err(_) => drop(autoindex::forget_early()),
        }
    }
    crate::store::open_default().tag(Failure::Database)
}

/// Where an answer's paths are written from (DEC-076): the checkout the
/// question is about, which a relative path is relative to, and every root
/// the store knows, gems included. Set once a command knows its checkout.
struct Rooting {
    base: String,
    roots: Vec<String>,
    /// Read while an index filled the store: a gem it committed since may
    /// hold the answer, so the store's roots are read again at the answer.
    late: Option<std::sync::OnceLock<Vec<String>>>,
}

static ROOTING: std::sync::OnceLock<Rooting> = std::sync::OnceLock::new();

/// Answer from this checkout: paths in the output are written against it,
/// and an index of it still filling the store is said (DEC-320).
fn answering_in(store: &Store, root: &str) {
    ROOTING.get_or_init(|| Rooting {
        base: root.to_string(),
        roots: store.roots().unwrap_or_default(),
        late: read_warming(store, root).map(|_| std::sync::OnceLock::new()),
    });
    let mut held = WARMING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if held.is_none() {
        *held = Some(read_warming(store, root));
    }
}

/// An answer read from the file alone — nothing under the cursor, a
/// variable, a symbol: no index can change it, so it says nothing of one
/// still filling the store, and a miss is final (exit 1, not 2).
fn index_free() {
    *WARMING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(None);
}

/// An answer from the file alone, given before any index is waited on:
/// paths are written against the file's checkout, held by the store yet or not.
fn answered_alone(store: &Store, root: &Path) {
    let base = root.to_string_lossy().into_owned();
    let mut roots = store.roots().unwrap_or_default();
    if !roots.contains(&base) {
        roots.push(base.clone());
    }
    ROOTING.get_or_init(|| Rooting {
        base,
        roots,
        late: None,
    });
}

/// Read the asked checkout's mark again: an index this query waited for has
/// moved it since `answering_in` (DEC-500).
fn rewarm(store: &Store, root: &str) {
    *WARMING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(read_warming(store, root));
}

fn read_warming(store: &Store, root: &str) -> Option<(String, crate::store::Warming)> {
    store
        .warming(root)
        .ok()
        .flatten()
        .map(|warming| (root.to_string(), warming))
}

/// Unset until a command knows its checkout; then that checkout's mark, if any.
static WARMING: std::sync::Mutex<Option<Option<(String, crate::store::Warming)>>> =
    std::sync::Mutex::new(None);

/// The asked checkout's index, while it is not whole yet.
fn warming() -> Option<(String, crate::store::Warming)> {
    WARMING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .flatten()
}

/// What a partial index says beside an answer: how much is in, and what
/// makes the rest certain.
fn warming_note(root: &str, warming: &crate::store::Warming) -> serde_json::Value {
    serde_json::json!({
        "read": warming.read,
        "of": warming.of,
        "interrupted": warming.interrupted,
        "hint": if warming.interrupted {
            format!("trekr --index {}", paths::pretty(root))
        } else {
            "an index is filling this checkout; ask again when it ends".to_string()
        },
    })
}

/// An answer from a partial index says so, and claims no more than the part
/// it read (DEC-320): `warming` beside it, its confidence scaled by the share
/// of the tree read, and a certain absence demoted to a residue — the method
/// may be in a file not read yet.
fn disclose(value: &mut serde_json::Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    // What the query's freshness check found (DEC-035).
    if let Some(index) = index_note() {
        object.entry("index").or_insert(index);
    }
    let Some((root, warming)) = warming() else {
        return;
    };
    object.insert("warming".into(), warming_note(&root, &warming));
    if let Some(confidence) = object.get("confidence").and_then(serde_json::Value::as_f64) {
        let scaled = (confidence * warming.coverage() * 100.0).floor() / 100.0;
        object.insert("confidence".into(), scaled.into());
    }
    if object.get("status").and_then(serde_json::Value::as_str) == Some("no_such_method") {
        object.insert("status".into(), "residue".into());
        let reason = object
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let reason = format!(
            "{reason}{}so far: the index has read {} of {} files",
            if reason.is_empty() { "" } else { " — " },
            warming.read,
            warming.of
        );
        object.insert("reason".into(), reason.into());
    }
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
        late: None,
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
        let holder = |roots: &[String]| {
            roots
                .iter()
                .filter(|root| paths::under(root, &absolute))
                .max_by_key(|root| root.len())
                .cloned()
        };
        let holder = holder(&self.roots).or_else(|| {
            let late = self.late.as_ref()?.get_or_init(|| {
                open_store()
                    .and_then(|store| Ok(store.roots()?))
                    .unwrap_or_default()
            });
            holder(late)
        });
        match holder {
            Some(root) => (absolute[root.len() + 1..].to_string(), root.into()),
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
    println!("{}", json_text(out, value)?);
    Ok(())
}

/// What `emit_json` prints, for a caller that must not panic on a closed
/// pipe.
fn json_text<T: serde::Serialize>(out: Output, value: &T) -> anyhow::Result<String> {
    let mut value = serde_json::to_value(value)?;
    rooted(&mut value);
    disclose(&mut value);
    Ok(if out == Output::Json {
        serde_json::to_string_pretty(&value)?
    } else {
        serde_json::to_string(&value)?
    })
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
    disclose(&mut answer);
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
            let mut head = serde_json::Map::new();
            if let Some(index) = index_note() {
                head.insert("index".into(), index);
            }
            ndjson_rows(rows, head, &mut w)?;
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
#[allow(clippy::too_many_arguments)]
fn index_files(
    store: &mut Store,
    root: &Path,
    files: &scan::Files,
    git_state: i64,
    // Some of the checkout's files, added to its map (`Store::write_part`).
    part: bool,
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
    if part || !store.map_unchanged(&root.to_string_lossy(), files)? {
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
    let bulk = !part
        && store.autocommit()
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
        let counts = if part {
            store.write_part(&root, files, facts)
        } else if bulk {
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
    let mut hints = crate::background::Hints::listen(
        crate::background::in_background() || autoindex::spawned(),
    );
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
    known: &mut Option<HashSet<Oid>>,
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
    known: &mut Option<HashSet<Oid>>,
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
    known: &mut Option<HashSet<Oid>>,
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
    known: &mut Option<HashSet<Oid>>,
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
    known: &mut Option<HashSet<Oid>>,
    pool: &rayon::ThreadPool,
    profile: &mut Option<profile::Profile>,
) -> anyhow::Result<Vec<crate::store::Indexed>> {
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
    // `None` when the store holds it already (`plan_gems`).
    files: Option<scan::Files>,
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

fn cmd_index(
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
    profile::timed(&mut profile, "tree", || {
        crate::tree::Tree::prepare(&store, &root_str)
    })?;
    store.truncate_wal();
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
    let kept = crate::store::kept(&crate::store::default_path()?, crate::store::VERSION);

    if out != Output::Text {
        // One object, because the totals are the point: they are what N
        // checkouts share, not the sum of what each one costs.
        let mut answer = serde_json::json!({
            "checkouts": rows,
            "others": others,
            "totals": totals,
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
    print_kept(&kept);
    Ok(ExitCode::SUCCESS)
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
    let facts = extract::extract_file(&path.to_string_lossy(), &source);
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
    mut parsed: Option<&mut Parsed>,
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
    if let Some(parsed) = parsed.as_deref_mut()
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
    let tier = |path: &String, facts: &crate::core::Facts| {
        let mut tiered = Tiered::default();
        for call in facts.calls.iter().filter(|c| refs::names_it(c, query)) {
            if listing {
                tiered.sites.push(crate::store::Ref::call(path, call));
            }
            let reference = query_refs::tier(tree, facts, call, path, query, target, partial);
            // Its includers may answer it: asked below, one file at a time.
            match query_refs::rescuable(facts, call, target, &reference) {
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
            let fresh: Vec<_> = files
                .par_iter()
                .filter(|path| !parsed.facts.contains_key(*path))
                .map(|path| (path.clone(), read(path)))
                .collect();
            parsed.facts.extend(fresh);
            let parsed = &*parsed;
            files
                .par_iter()
                .filter_map(|path| Some((path, parsed.facts.get(path)?.as_ref()?)))
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
    if let (Some(target), false) = (target, rescuable.is_empty()) {
        let files = crate::query::members::CheckoutFiles::new(store, root, root_str);
        for (call, mut reference) in rescuable {
            query_refs::rescue(tree, &files, &call, target, &mut reference);
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
#[derive(Default)]
struct Parsed {
    facts: HashMap<String, Option<crate::core::Facts>>,
    /// Every call named `initialize` — a subclass's `super`, mostly.
    callers: Option<Vec<Caller>>,
    /// The files that call `new`, by each capitalized word their text holds.
    naming: Option<HashMap<u64, Vec<u32>>>,
    /// What an `X.new` site constructs, by file and call index.
    made: HashMap<(String, usize), crate::resolve::refs::Construct>,
    /// The files that call `new`, by path.
    calling_new: Option<Vec<String>>,
}

impl Parsed {
    /// Parse the files not held yet.
    fn read(&mut self, root: &Path, files: &[String]) {
        let fresh: Vec<_> = files
            .par_iter()
            .filter(|path| !self.facts.contains_key(*path))
            .map(|path| (path.clone(), extract::read_file(root, path)))
            .collect();
        self.facts.extend(fresh);
    }

    /// What each `new` call of these files constructs, worked out for the
    /// ones not known yet.
    fn construct(&mut self, tree: &Tree, files: &[String]) {
        use crate::resolve::refs;
        let made = &self.made;
        let fresh: Vec<_> = files
            .par_iter()
            .filter_map(|path| Some((path, self.facts.get(path)?.as_ref()?)))
            .flat_map_iter(|(path, facts)| {
                new_calls(facts)
                    .filter(|(at, _)| !made.contains_key(&(path.clone(), *at)))
                    .map(|(at, call)| {
                        ((path.clone(), at), refs::construct(tree, facts, call, path))
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        self.made.extend(fresh);
    }

    /// Read the checkout's calls named `initialize`, once.
    fn read_callers(&mut self, tree: &Tree, store: &Store, root: &Path, root_str: &str) {
        if self.callers.is_none() {
            let files = store
                .files_calling(root_str, "initialize")
                .unwrap_or_default();
            self.read(root, &files);
            let held = &self.facts;
            let callers = files
                .par_iter()
                .filter_map(|path| Some((path, held.get(path)?.as_ref()?)))
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
                .collect();
            self.callers = Some(callers);
        }
    }

    fn calling_new(&mut self, store: &Store, root_str: &str) -> &[String] {
        self.calling_new.get_or_insert_with(|| {
            let mut files = store.files_calling(root_str, "new").unwrap_or_default();
            files.sort();
            files
        })
    }

    /// The files calling `new` whose text names `word`.
    fn naming(&mut self, store: &Store, root: &Path, root_str: &str, word: &str) -> Vec<String> {
        let files = self.calling_new(store, root_str).to_vec();
        let naming = self.naming.get_or_insert_with(|| {
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
    parsed: &mut Parsed,
) -> anyhow::Result<(
    Vec<crate::resolve::refs::Reference>,
    crate::resolve::refs::Counts,
)> {
    use crate::resolve::refs;
    let owner = target.unwrap_or_default();
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
        let Some(facts) = parsed.facts.get(path).and_then(Option::as_ref) else {
            continue;
        };
        for call in facts.calls.iter().filter(|c| c.name == "initialize") {
            let reference = refs::tier_call(tree, facts, call, path, query, target);
            if reference.tier != refs::Tier::Excluded && reference.unplaced().is_none() {
                keep(&mut counts, &mut found, reference);
                return Ok((found, counts));
            }
        }
        for (at, call) in new_calls(facts) {
            let construct = &parsed.made[&(path.clone(), at)];
            if !refs::may_run(construct, own) {
                continue;
            }
            let reference =
                refs::tier_call_with(tree, facts, call, path, query, target, Some(construct));
            if reference.tier != refs::Tier::Excluded && reference.unplaced().is_none() {
                keep(&mut counts, &mut found, reference);
                return Ok((found, counts));
            }
        }
    }
    // Then every call of its name: a subclass's `super` may be in a file
    // that calls no `new`.
    parsed.read_callers(tree, store, root, root_str);
    for caller in parsed.callers.as_deref().unwrap_or_default() {
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
        let Some(facts) = parsed.facts.get(&caller.path).and_then(Option::as_ref) else {
            continue;
        };
        let call = &facts.calls[caller.at];
        let reference = refs::tier_call(tree, facts, call, &caller.path, query, target);
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
        let Some(facts) = parsed.facts.get(path).and_then(Option::as_ref) else {
            continue;
        };
        let first = new_calls(facts)
            .filter(|(at, call)| {
                matches!(parsed.made[&(path.clone(), *at)], refs::Construct::Untyped)
                    && own.accepts(call.argc)
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
    // A card counts the call sites, which may be in any file.
    if let Some(code) = autoindex::ensure(out, &store, &root, Need::Whole)? {
        return Ok(code);
    }
    if !store.has_checkout(&root_str)? {
        return not_indexed(out, &root, &store);
    }
    answering_in(&store, &root_str);
    let tree = build_tree(&store, &root_str)?;
    let query = refs::constructing(&tree, query);

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
        let mut text_out = card_text(&fqn, &sites, &ancestors, None);
        let mut answer = serde_json::json!({
            "query": text,
            "status": "resolved",
            "fqn": fqn,
            "kind": tree.kind_of(&fqn),
            "definition": sites,
            "ancestors": ancestors,
            "unresolved_ancestors": chain.unresolved,
        });
        if let Some((table, line)) = model_table(&tree, &root, &fqn) {
            text_out.push_str(&format!("\n  {line}"));
            answer["table"] = table;
        }
        return report(out, answer, true, &text_out);
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

/// A model's table, from the app's schema dump (DEC-481): the card's `table`
/// object and its one line of text. `None` for a class that is not a model.
fn model_table(tree: &Tree, root: &Path, fqn: &str) -> Option<(serde_json::Value, String)> {
    use crate::schema::model::Model;
    let read = |path: &str| crate::scan::read_text(path).ok();
    let (name, base) = match crate::schema::model::of(tree, fqn, &read)? {
        Model::Abstract => {
            let table = serde_json::json!({"name": null, "abstract": true});
            return Some((table, "abstract: no table of its own".to_string()));
        }
        Model::Table { name, base } => (name, base),
    };
    let root_text = root.to_string_lossy();
    let site = tree
        .sites(fqn)
        .into_iter()
        .map(|s| s.path)
        .find(|p| crate::core::paths::under(&root_text, p));
    let found = crate::schema::dumps_near(root, site.as_deref())
        .into_iter()
        .find_map(|dump| {
            let bytes = crate::scan::read_source(root.join(&dump)).ok()?;
            let table = crate::schema::tables_in(&dump, &bytes)
                .into_iter()
                .find(|t| t.name == name)?;
            Some((dump, table))
        });
    let mut table = serde_json::json!({
        "name": name,
        "abstract": false,
        "inherited_from": base,
    });
    let shared = base
        .as_deref()
        .map(|base| format!(", shared with {base}"))
        .unwrap_or_default();
    let Some((dump, found)) = found else {
        let line = format!("table {name}{shared}, not in the schema");
        return Some((table, line));
    };
    let columns: Vec<serde_json::Value> = found
        .columns
        .iter()
        .map(|c| {
            serde_json::json!({
                "name": c.name,
                "type": c.sql_type,
                "class": c.class,
                "null": c.null,
                "default": c.default,
                "line": c.pos.line,
            })
        })
        .collect();
    let indexes: Vec<serde_json::Value> = found
        .indexes
        .iter()
        .map(|i| serde_json::json!({"columns": i.columns, "unique": i.unique}))
        .collect();
    table["path"] = dump.clone().into();
    table["line"] = found.line.into();
    table["primary_key"] = found.primary_key.names().into();
    table["columns"] = columns.into();
    table["indexes"] = indexes.into();
    table["view"] = found.view.is_some().into();
    if let Some(view) = found.view {
        table["unread_columns"] = view.unread.into();
    }
    let implicit = found
        .primary_key
        .names()
        .iter()
        .filter(|key| found.column(key).is_none())
        .count();
    let (kind, unread) = match found.view {
        Some(view) if view.unread > 0 => ("view", format!(", {} not read", view.unread)),
        Some(_) => ("view", String::new()),
        None => ("table", String::new()),
    };
    let line = format!(
        "{kind} {name}{shared} · {dump}:{} · {} columns{unread}",
        found.line,
        found.columns.len() + implicit
    );
    Some((table, line))
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
    if position::Spec::parse(text).is_some() {
        return cmd_refs_at(out, text, include_excluded, context);
    }
    let query = refs::Query::parse(text);
    check_method_shape(&query, text)?;
    let root = asked_from(context)?;
    let root_str = root.to_string_lossy().into_owned();
    let mut store = open_store()?;
    if let Some(code) = autoindex::ensure(out, &store, &root, Need::Whole)? {
        return Ok(code);
    }
    if !store.has_checkout(&root_str)? {
        return not_indexed(out, &root, &store);
    }
    answering_in(&store, &root_str);

    // A bare name narrows nothing, so it keeps the whole-mention view —
    // definitions and constant references included, which a method-shaped
    // query has no use for.
    if query.owner.is_none() {
        crate::usage::flag("by-name");
        // `--json` is a bare array, with no room for `index`: it is said on
        // stderr, as text says it. NDJSON's closing line carries it.
        let say = match out {
            Output::Json => Output::Text,
            other => other,
        };
        let probe = probe(&store, &root);
        freshen(say, &mut store, &root, None, probe);
        return cmd_refs_by_name(out, &root, &root_str, &store, &query);
    }

    let tree = fresh_tree(out, &mut store, &root, None)?;
    let query = refs::constructing(&tree, query);
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
        return Ok(exit_on(false));
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
    // An untyped `x.new` may build any class, and an `initialize` with many
    // callers has hundreds: one line says so, and `--json` lists them.
    let (untyped_new, listed): (Vec<_>, Vec<_>) = found
        .iter()
        .partition(|r| r.unplaced() == Some(refs::Unplaced::New));
    for reference in listed {
        println!(
            "{}:{}:{}  {:<10} {}",
            shown(&reference.path),
            reference.line,
            reference.col,
            format!("{:?}", reference.tier).to_lowercase(),
            reference.why,
        );
    }
    if !untyped_new.is_empty() {
        println!(
            "… {} untyped x.new (possible) — --json lists them",
            untyped_new.len()
        );
    }
    // The number a grep cannot produce, said out loud — a zero included,
    // since "nothing ruled out" is a finding too. An answer that lists no
    // site has already said why.
    if !found.is_empty() || reason.is_none() || include_excluded {
        // An `initialize` is called as `new` too (DEC-541).
        let sites = match refs::constructor_of(&query) {
            Some(new) => format!("call sites of `{}` or `{new}`", query.name),
            None => "same-name call sites".to_string(),
        };
        println!(
            "\n{} confirmed, {} possible, {} excluded of {} {sites}",
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

/// `--refs FILE:LINE:COL`: the references of what is at a position. An
/// example group's `let`, `subject` or `def` is read by Ruby's lookup at
/// runtime, which `resolve::members` follows (DEC-490); a method is asked by
/// its owner, as `--refs Owner#method` asks it.
fn cmd_refs_at(
    out: Output,
    written: &str,
    include_excluded: bool,
    pinned: Option<&Path>,
) -> anyhow::Result<ExitCode> {
    let spec = position::Spec::parse(written).expect("checked by the caller");
    if let Some(why) = spec.out_of_range(written) {
        return Err(Failure::Usage.error(why));
    }
    let file = Path::new(&spec.path);
    if !file.exists() {
        return Err(Failure::NotFound.error(format!("no such path: {}", spec.path)));
    }
    // Before anything reads it unbounded: a pipe or a device is refused here.
    let source = read_input(file)?;
    let (root, mut store) = checkout_for_query(file, pinned)?;
    let root_str = root.to_string_lossy().into_owned();
    // References are anywhere: a position's own part is not enough.
    if let Some(code) = autoindex::ensure(out, &store, &root, Need::Whole)? {
        return Ok(code);
    }
    if !store.has_checkout(&root_str)? {
        return not_indexed(out, &root, &store);
    }
    answering_in(&store, &root_str);
    let absolute = std::fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
    let relative = absolute
        .strip_prefix(&root)
        .map_or_else(|_| spec.path.clone(), |p| p.to_string_lossy().into_owned());
    let tree = fresh_tree(out, &mut store, &root, Some(file))?;
    let files = crate::query::members::CheckoutFiles::new(&store, &root, &root_str);
    if let Some((path, def)) =
        crate::query::members::member_at_position(&tree, &files, &relative, spec.line, spec.col)
    {
        let context = crate::resolve::members::Context::new(&tree, &files);
        let (answer, reads) =
            members::refs_answer(&context, written, &path, &def, include_excluded);
        let found = reads.counts.confirmed + reads.counts.possible > 0;
        if out != Output::Text {
            emit_listing(out, answer, "references", &reads.found)?;
        } else {
            for line in members::refs_text(&answer, &def, &reads) {
                println!("{line}");
            }
        }
        return Ok(exit_on(found));
    }
    // A method: the one defined there, or the one a call there runs.
    let facts = crate::extract::extract_file(&file.to_string_lossy(), &source);
    // A variable is not a call: its mentions, as the editor lists them, and
    // not whichever method is nearest on the line.
    if spec.col > 0
        && crate::query::position::at_facts(&facts, spec.line, spec.col).is_none()
        && let Some((head, rows)) =
            variable_mentions(&tree, &root_str, &absolute, &source, spec.line, spec.col)
    {
        let found = !rows.is_empty();
        let mut head = head;
        head["query"] = written.into();
        if out != Output::Text {
            emit_listing(out, head, "references", &rows)?;
        } else {
            for row in &rows {
                println!("{}:{}:{}  {}", row.path, row.line, row.col, row.kind);
            }
            println!(
                "{} mention(s) of {} `{}`",
                rows.len(),
                head["variable"].as_str().unwrap_or_default(),
                head["name"].as_str().unwrap_or_default()
            );
        }
        return Ok(exit_on(found));
    }
    // Exactly what is at the column: a snap to the nearest name would list
    // another name's references, with nothing to say so. A bare `FILE:LINE`
    // takes the line's first name.
    let under = match spec.col {
        0 => crate::query::position::at_or_snap(&facts, spec.line, 0).map(|(under, _)| under),
        col => crate::query::position::at_facts(&facts, spec.line, col),
    };
    // A class, module or constant: its references, as `--refs Name` lists them.
    let constant = match &under {
        Some(crate::query::position::Under::Definition(def))
            if matches!(
                def.kind,
                crate::core::Kind::Class | crate::core::Kind::Module
            ) =>
        {
            let mut nesting = def.nesting.clone();
            nesting.insert(0, def.name.clone());
            tree.scope_fqn(&nesting)
        }
        Some(crate::query::position::Under::Constant(reference)) => tree
            .resolve_at(&reference.name, &reference.nesting, &relative)
            .fqn
            .or_else(|| Some(reference.name.clone())),
        _ => None,
    };
    if let Some(fqn) = constant {
        return cmd_refs(out, &fqn, include_excluded, Some(&root));
    }
    let owner_and_name = match under {
        Some(crate::query::position::Under::Definition(def))
            if def.kind == crate::core::Kind::Method =>
        {
            tree.scope_fqn(&def.nesting)
                .map(|owner| (owner, def.singleton, def.name.clone()))
        }
        Some(crate::query::position::Under::Call(call)) => {
            let answer = crate::resolve::method_at(&tree, &facts, &call, &relative);
            let (name, singleton) = crate::resolve::asked_at(&tree, &call, &answer);
            // A call whose method trekr cannot place has no references to
            // narrow: say so, and point at the name's.
            let Some(owner) = answer.owner.clone() else {
                return refs_of_unplaced_call(out, written, &name, &answer);
            };
            Some((owner, singleton, name))
        }
        _ => None,
    };
    let Some((owner, singleton, name)) = owner_and_name else {
        return Err(Failure::Usage.error(format!(
            "no method at {written}: --refs takes a method's definition or a call of it, \
             a class, module or constant, a variable, or an example group's let, subject or def"
        )));
    };
    let query = format!("{owner}{}{name}", if singleton { "." } else { "#" });
    cmd_refs(out, &query, include_excluded, Some(&root))
}

/// One mention of a variable, for `--refs` at it.
#[derive(serde::Serialize)]
struct Mention {
    path: String,
    line: u32,
    col: u32,
    /// `read` or `write`.
    kind: &'static str,
}

/// The variable at a position and every mention of it: a local's in its
/// file, an instance or class variable's across its class's files
/// (`query::variables`). `None` when no variable is there.
fn variable_mentions(
    tree: &crate::tree::Tree,
    root_str: &str,
    file: &Path,
    raw: &[u8],
    line: u32,
    col: u32,
) -> Option<(serde_json::Value, Vec<Mention>)> {
    use crate::resolve::vars::{self, Sigil};
    let path = file.to_string_lossy();
    let source = crate::extract::ruby_source(&path, raw);
    let head = position::variable_at(&source, &path, line, col)?;
    crate::usage::flag("variable");
    let offset = position::offset_of(&source, line, col)?;
    let here = vars::analyze(&source);
    let want = here.at(offset)?.clone();
    let relative = |p: &str| {
        p.strip_prefix(root_str)
            .and_then(|r| r.strip_prefix('/'))
            .unwrap_or(p)
            .to_string()
    };
    let mention = |path: &str, text: &[u8], o: &vars::Occurrence| {
        let at = crate::extract::LineIndex::new(text).pos(o.span.start);
        Mention {
            path: relative(path),
            line: at.line,
            col: at.col,
            kind: if o.is_write() { "write" } else { "read" },
        }
    };
    let scope = match want.sigil {
        Sigil::Local => None,
        Sigil::Instance | Sigil::Class => {
            let owner = here.owner(&want)?;
            crate::query::variables::ClassScope::of(tree, &path, owner, want.sigil)
        }
    };
    let mut rows: Vec<Mention> = match scope {
        None => here
            .same(&want)
            .into_iter()
            .map(|o| mention(&path, &source, o))
            .collect(),
        Some(scope) => scope
            .files
            .iter()
            .filter_map(|other| {
                let raw = crate::scan::read_source(other).ok()?;
                let text = crate::extract::ruby_source(other, &raw).into_owned();
                let found = vars::analyze(&text);
                let rows: Vec<Mention> = scope
                    .mentions(&found, &want, |nesting| scope.holds(tree, nesting))
                    .into_iter()
                    .map(|o| mention(other, &text, o))
                    .collect();
                Some(rows)
            })
            .flatten()
            .collect(),
    };
    rows.sort_by(|a, b| (&a.path, a.line, a.col).cmp(&(&b.path, b.line, b.col)));
    rows.dedup_by(|a, b| (&a.path, a.line, a.col) == (&b.path, b.line, b.col));
    let counts = crate::resolve::refs::Counts {
        confirmed: rows.len(),
        ..Default::default()
    };
    let head = serde_json::json!({
        "under": "variable",
        "variable": head["variable"],
        "name": head["name"],
        "status": "resolved",
        "counts": counts,
        "references": null,
    });
    Some((head, rows))
}

/// `--refs` at a call whose method trekr cannot place: the `--def` answer's
/// status and reason, no references, and the bare name's listing as a hint.
fn refs_of_unplaced_call(
    out: Output,
    written: &str,
    name: &str,
    answer: &crate::resolve::MethodAnswer,
) -> anyhow::Result<ExitCode> {
    let reason = answer
        .reason
        .clone()
        .unwrap_or_else(|| format!("no method `{name}` trekr can place for this call"));
    let hint = format!("trekr --refs {name}");
    if out != Output::Text {
        let found = serde_json::json!({
            "query": written,
            "status": answer.status,
            "owner": null,
            "method": name,
            "receiver": answer.receiver,
            "receiver_type": answer.receiver_type,
            "definition": [],
            "resolves_to": null,
            "inherited": false,
            "counts": crate::resolve::refs::Counts::default(),
            "references": null,
            "reason": reason,
            "hint": hint,
        });
        emit_listing(
            out,
            found,
            "references",
            &[] as &[crate::resolve::refs::Reference],
        )?;
    } else {
        println!("{reason}\n  every call site of {name} by name: {hint}");
    }
    Ok(exit_on(false))
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
fn index_note() -> Option<serde_json::Value> {
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
    let lag: Vec<&str> = found.iter().filter_map(|f| f.lag.as_deref()).collect();
    if !lag.is_empty() {
        value["cause"] = lag.join("; ").into();
    }
    Some(value)
}

/// Read what changed since the index into `store`, for this command only
/// (DEC-035): every file `probe` finds edited, added or deleted, up to
/// `BULK`, and the file asked about whatever it found. Once per checkout
/// per command — a second store opened on a checkout already checked
/// (`probe` is `None`) gets the same reading. Says what it found
/// (`Freshness::say`, `index_note`); returns whether the store now answers
/// differently, so a tree built before must be built again.
fn freshen(
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
type Probe = scan::Probe<Option<Compared>>;

struct Compared {
    /// The map as indexed, path → blob oid.
    stored: HashMap<String, String>,
    /// Every path whose blob differs, and every one gone.
    changed: crate::store::Overlay,
}

/// The working tree of `root`, begun reading — unless this command has
/// checked `root` already. The stored map is read on a connection of its
/// own while git runs, and compared on git's thread: none of it waits
/// behind the tree build.
fn probe(store: &Store, root: &Path) -> Option<Probe> {
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
        let mut changed: crate::store::Overlay = gone.collect();
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
    let overlay = found.as_ref().map_or(&[][..], |f| f.overlay.as_slice());
    match store.overlay(&root_str, overlay) {
        Ok(changed) => (found, changed),
        Err(_) => {
            if let Some(found) = &mut found {
                found.lag = Some("the edits since the index could not be read".to_string());
                found.refreshed.clear();
                found.overlay.clear();
            }
            (found, store.overlay(&root_str, &[]).unwrap_or(true))
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
            (store.file_map(&root_str).ok()?, Vec::new())
        }
    };
    // More is an operation on the checkout — a branch switch, a rebase —
    // which `--index` reads at once rather than every query one by one.
    if changed.len() > scan::BULK {
        lag = Some(format!(
            "{} files changed since the index, more than a query reads",
            changed.len()
        ));
        changed.retain(|(path, _)| Some(path) == queried.as_ref());
    }
    // The file asked about is read whatever git said: its bytes are the
    // question.
    if let Some(path) = &queried
        && scan::is_indexed(path)
        && !changed.iter().any(|(changed, _)| changed == path)
        && let Ok(bytes) = scan::read_source(root.join(path))
    {
        let oid = scan::hash_blob(&bytes);
        if stored.get(path) != Some(&oid.0) {
            changed.push((path.clone(), Some(oid)));
        }
    }
    // By path: the stored map is a `HashMap`, and the same edits must be the
    // same overlay in every process, or none resumes another's copy.
    changed.sort_by(|a, b| a.0.cmp(&b.0));
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
        overlay,
    })
}

/// The tree a query answers from, with what changed since the index read in
/// first. git's look at the working tree runs while the tree is built, which
/// is most queries' whole answer. The tree is built over what the last query
/// read (`Store::resume`) — the same edits, most often — and built again
/// when git finds otherwise.
fn fresh_tree(
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

/// Why the store holds nothing, when an upgrade emptied it.
fn upgrade_reason(from: i64) -> String {
    if from == crate::store::VERSION {
        return "trekr's index couldn't be read and was rebuilt (DEC-300), which dropped \
                any earlier index; nothing has been indexed since"
            .into();
    }
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
    let raw = read_input(Path::new(&spec.path))?;
    let facts = crate::extract::extract_file(&spec.path, &raw);
    // What the file runs, at its offsets: a template's tags (DEC-520).
    let source = crate::extract::ruby_source(&spec.path, &raw).into_owned();
    // The file as a site names it, whatever directory the question came from.
    let file = std::fs::canonicalize(&spec.path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| spec.path.clone());
    let mut checkout = checkout_for_query(Path::new(&spec.path), pinned).ok();
    // The branches below answer from the file alone, so before any index is
    // waited on, writing their paths against the file's checkout.
    let file_alone = |checkout: &Option<(PathBuf, Store)>| {
        if let Some((root, store)) = checkout {
            answered_alone(store, root);
        }
        index_free();
    };
    // A template a `render` or `extends` names: the file it reaches (DEC-524).
    if let Some(template) = facts
        .templates
        .iter()
        .find(|t| t.pos.line == spec.line && spec.col >= t.pos.col && spec.col < t.pos.col + t.len)
        && let Some((root, store)) = checkout.as_mut().map(|(root, store)| (&*root, store))
    {
        let relative = std::fs::canonicalize(&spec.path)
            .ok()
            .and_then(|abs| abs.strip_prefix(root).ok().map(Path::to_path_buf))
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| spec.path.clone());
        let class = match &template.names {
            crate::core::Named::Object { value, .. } => {
                let tree = fresh_tree(out, store, root, Some(Path::new(&spec.path)))?;
                crate::resolve::views::value_class(&tree, &facts, value, template.pos, &relative)
            }
            _ => None,
        };
        let files =
            crate::tree::views::template_files(root, &relative, &template.names, class.as_deref());
        let answer = template_answer(written.to_string(), root, &files, class.as_deref());
        let found = !files.is_empty();
        let text = match files.first() {
            Some(first) => format!(
                "{}:1:1  template",
                shown(&root.join(first).to_string_lossy())
            ),
            None => "no template by that name".to_string(),
        };
        return report(out, answer, found, &text);
    }
    // A `super` with no fact behind it is one whose method has no owner the
    // source names. Snapping would answer for another name on the line.
    if crate::query::position::at_facts(&facts, spec.line, spec.col).is_none()
        && crate::query::position::word_at(&source, spec.line, spec.col).as_deref() == Some("super")
    {
        file_alone(&checkout);
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
    // A symbol no rule reads as a method's name is a value (`on: :create`,
    // `status: :ok`). Snapping from one answered, resolved, for whatever
    // other name was nearest on the line (DEC-343).
    if spec.col > 0
        && crate::query::position::at_facts(&facts, spec.line, spec.col).is_none()
        && let Some((name, _, _)) =
            crate::extract::symbol_literals(&source)
                .into_iter()
                .find(|(_, pos, len)| {
                    pos.line == spec.line
                        && pos.col.saturating_sub(1) <= spec.col
                        && spec.col < pos.col + *len as u32
                })
    {
        file_alone(&checkout);
        return report(
            out,
            serde_json::json!({
                "query": written,
                "under": "symbol",
                "name": name,
                "status": "residue",
                "confidence": 0.0,
                "definition": [],
                "reason": "a symbol no rule reads as a method's name here: a key or a value",
            }),
            false,
            &format!(":{name}  a key or a value here, not a method's name"),
        );
    }
    // A variable is not a call, and snapping from one answered for whatever
    // name was nearest on the line.
    if spec.col > 0
        && crate::query::position::at_facts(&facts, spec.line, spec.col).is_none()
        && let Some(answer) = position::variable_at(&source, &file, spec.line, spec.col)
    {
        crate::usage::flag("variable");
        let mut answer = answer;
        // A template's `@ivar` with no write of its own is set by the
        // controller that renders it (DEC-522).
        let unset_in_template = answer["variable"] == "ivar"
            && answer["definition"].as_array().is_some_and(Vec::is_empty)
            && crate::tree::views::ViewTemplate::of(&file).is_some();
        let mut from_controller = Vec::new();
        if unset_in_template
            && let Some((root, store)) = checkout.as_mut().map(|(root, store)| (&*root, store))
        {
            let relative = Path::new(&file)
                .strip_prefix(root)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| spec.path.clone());
            let tree = fresh_tree(out, store, root, Some(Path::new(&spec.path)))?;
            let name = answer["name"].as_str().unwrap_or_default().to_string();
            from_controller = crate::resolve::views::template_ivar_writes(&tree, &relative, &name);
        }
        if from_controller.is_empty() {
            file_alone(&checkout);
        } else {
            // Read from the tree: its paths are the checkout's, and an index
            // still filling it may not hold the write yet.
            if let Some((root, store)) = &checkout {
                answering_in(store, &root.to_string_lossy());
            }
            answer["definition"] = from_controller
                .iter()
                .map(|site| {
                    serde_json::json!({
                        "path": site.path, "line": site.line, "col": site.col, "kind": "assigned",
                    })
                })
                .collect();
            answer["status"] = "resolved".into();
            answer["confidence"] = 1.0.into();
            answer["resolved_via"] = "controller".into();
            answer["reason"] = "set by the controller that renders the template".into();
        }
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
    let snapped = crate::query::position::at_or_snap(&facts, spec.line, spec.col);
    let Some((under, snapped)) = snapped else {
        file_alone(&checkout);
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
    // The cursor on a definition is a fact of the file, too.
    if let crate::query::position::Under::Definition(_) = under {
        file_alone(&checkout);
    } else if let Some((root, store)) = checkout {
        let need = Need::File(Path::new(&spec.path));
        if let Some(code) = autoindex::ensure(out, &store, &root, need)? {
            return Ok(code);
        }
    }

    let query = written.to_string();
    // Which checkout's assembled namespace answered. It is only ever a
    // surprise for a position inside a gem, which is answered from an app that
    // resolves it — and an answer that depends on which app must say which.
    let mut context: Option<String> = None;
    let answer = match under {
        // The cursor is on the declaration itself. Ruby has no indirection to
        // follow here, so the honest answer is "you are already there".
        crate::query::position::Under::Definition(def) => {
            let mut answer = serde_json::json!({
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
            });
            // A top-level def is Object's (DEC-311).
            if def.kind == crate::core::Kind::Method {
                answer["owner"] = def.nesting.first().map_or("Object", String::as_str).into();
            }
            answer
        }
        crate::query::position::Under::Constant(reference) => {
            let (root, mut store) = checkout_for_query(Path::new(&spec.path), pinned)?;
            if !store.has_checkout(&root.to_string_lossy())? {
                return not_indexed(out, &root, &store);
            }
            answering_in(&store, &root.to_string_lossy());
            let mut tree = fresh_tree(out, &mut store, &root, Some(Path::new(&spec.path)))?;
            context = Some(root.to_string_lossy().into_owned());
            let relative = std::fs::canonicalize(&spec.path)
                .ok()
                .and_then(|abs| abs.strip_prefix(&root).ok().map(Path::to_path_buf))
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| spec.path.clone());
            let mut resolution = tree.resolve_at(&reference.name, &reference.nesting, &relative);
            // A namespace resolved with nowhere to go yet is no answer yet.
            let found = resolution.status != Status::Residue && !resolution.sites.is_empty();
            match autoindex::after_partial(out, &store, &root, found)? {
                Then::Keep => {}
                Then::Exit(code) => return Ok(code),
                Then::Again => {
                    rewarm(&store, &root.to_string_lossy());
                    tree = build_tree(&store, &root.to_string_lossy())?;
                    resolution = tree.resolve_at(&reference.name, &reference.nesting, &relative);
                }
            }
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
        crate::query::position::Under::Call(call) => {
            let (root, mut store) = checkout_for_query(Path::new(&spec.path), pinned)?;
            if !store.has_checkout(&root.to_string_lossy())? {
                return not_indexed(out, &root, &store);
            }
            answering_in(&store, &root.to_string_lossy());
            let mut tree = fresh_tree(out, &mut store, &root, Some(Path::new(&spec.path)))?;
            context = Some(root.to_string_lossy().into_owned());
            let relative = std::fs::canonicalize(&spec.path)
                .ok()
                .and_then(|abs| abs.strip_prefix(&root).ok().map(Path::to_path_buf))
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| spec.path.clone());
            let facts = crate::extract::extract(&source);
            let answer_from = |tree: &Tree| {
                let answer = crate::resolve::method_at(tree, &facts, &call, &relative);
                // A shared group's body reads what its includers define (DEC-490).
                if !crate::resolve::members::includers_may_answer(&call, &answer) {
                    return answer;
                }
                let root_str = root.to_string_lossy().into_owned();
                let files = crate::query::members::CheckoutFiles::new(&store, &root, &root_str);
                crate::query::members::includer_answer(tree, &files, &relative, &call, &answer)
                    .unwrap_or(answer)
            };
            let mut answer = answer_from(&tree);
            let found = matches!(answer.status, Status::Resolved | Status::Ambiguous);
            match autoindex::after_partial(out, &store, &root, found)? {
                Then::Keep => {}
                Then::Exit(code) => return Ok(code),
                Then::Again => {
                    rewarm(&store, &root.to_string_lossy());
                    // The partial tree is left to the OS with the rest.
                    tree = build_tree(&store, &root.to_string_lossy())?;
                    answer = answer_from(&tree);
                }
            }
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
    if snapped.is_some() {
        crate::usage::flag("snapped");
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
        let label = match field("status").as_deref() {
            Some("residue") => "evidence ",
            _ => "agreement",
        };
        out.push(format!("  {label}   {agreement}"));
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
    let dir = context.unwrap_or(Path::new("."));
    if !dir.exists() {
        return Err(Failure::NotFound.error(format!("no such path: {}", dir.display())));
    }
    let store = open_store()?;
    let root = checkout_for(&store, dir)?;
    // A reopening or an included module may be in any file, or a gem.
    if let Some(code) = autoindex::ensure(out, &store, &root, Need::Whole)? {
        return Ok(code);
    }
    if !store.has_checkout(&root.to_string_lossy())? {
        return not_indexed(out, &root, &store);
    }
    answering_in(&store, &root.to_string_lossy());
    let tree = build_tree(&store, &root.to_string_lossy())?;
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
/// The answer on a template's name: each file it reaches, at its top
/// (DEC-524).
fn template_answer(
    query: String,
    root: &Path,
    files: &[String],
    class: Option<&str>,
) -> serde_json::Value {
    let definition: Vec<serde_json::Value> = files
        .iter()
        .map(|file| {
            serde_json::json!({
                "path": file, "root": root.to_string_lossy(), "line": 1, "col": 1,
                "kind": "template",
            })
        })
        .collect();
    let mut answer = serde_json::json!({
        "query": query,
        "under": "template",
        "status": match files.len() { 0 => "residue", 1 => "resolved", _ => "ambiguous" },
        "confidence": crate::resolve::share(1, files.len()),
        "resolved_via": "render",
        "definition": definition,
    });
    if let Some(class) = class {
        answer["receiver_type"] = class.into();
    }
    if files.is_empty() {
        answer["reason"] = "no template in the checkout's views by that name".into();
    }
    answer
}

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
        _ if !matched && warming().is_some() => Outcome::NotIndexed,
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
    store.clear_warming(&root_str)?;
    crate::tree::forget_snapshots(&store, &root_str);
    store.forget_overlay(&root_str);

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
    if vacuum {
        store.vacuum()?;
    }
    let db_bytes = store.db_bytes()?;
    let found = !garbage.checkouts.is_empty()
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
                "checkouts": garbage.checkouts,
                "files": garbage.files,
                "blobs": garbage.blobs,
                "facts": garbage.facts,
                "reclaimed_bytes": garbage.reclaimed_bytes,
                "snapshots": snapshots,
                "overlays": { "files": overlays, "bytes": overlay_bytes },
                "signatures": garbage.signatures,
                "core_files": core,
                "kept": kept,
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
    if vacuum {
        println!("vacuumed: database is {:.1} MB", mb(db_bytes));
    }
    Ok(exit_on(found))
}

/// 0 when something happened, 1 when nothing did — so a script can branch on it.
/// `0` for an answer, `1` for nothing found — or `2`, "no answer yet", when
/// the nothing comes from a partial index (DEC-320).
fn exit_on(happened: bool) -> ExitCode {
    if happened {
        ExitCode::SUCCESS
    } else if warming().is_some() {
        ExitCode::from(2)
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
    let Some(log) = crate::log::Log::where_to_look() else {
        let why = "the LSP log is off or on stderr (TREKR_LOG), so no misses are recorded";
        match out {
            Output::Text => println!("{why}"),
            _ => {
                eprintln!("trekr: {why}");
                emit_rows(out, &[] as &[crate::log::misses::Recorded])?;
            }
        }
        return Ok(ExitCode::from(1));
    };
    let since = days.map(|n| crate::log::days_ago(n.saturating_sub(1)));
    let misses = crate::log::misses::read(&log, since.as_deref())?;
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
    fn a_files_capitalized_words_are_its_whole_identifiers() {
        let words = capitalized_words(b"Admin::Widget.new(x_Widget, :Gadget) # Widgets");
        let want: HashSet<u64> = ["Admin", "Gadget", "Widget", "Widgets"]
            .iter()
            .map(|w| word_hash(w.as_bytes()))
            .collect();
        assert_eq!(words, want);
    }

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
                let mut parsed = Parsed::default();
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
