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
mod dead_gems;
mod dead_tests;
mod entry_points;
mod fresh;
mod generated;
mod incomplete;
mod index;
mod members;
mod position;
mod profile;
mod refs;
mod routes;
mod views;

use crate::failure::{Failure, Tag};
use autoindex::{Need, Then};
use fresh::{fresh_tree, freshen, index_note, lag, probe};
use index::cmd_index;
use refs::{Parsed, gather_refs};

use crate::cleanup::Retention;
use crate::core::Oid;
use crate::core::paths;
use crate::query::def::{Reached, Subject};
use crate::resolve::DefinedOn;
use crate::store::Store;
use crate::tree::{Status, Tree, public_name};
use crate::{extract, scan};
use clap::{CommandFactory, FromArgMatches, Parser};
use clap_complete::Shell;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, OnceLock, RwLock};

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
        TREKR_NO_INDEX  never index from a query, as --no-index\n  \
        TREKR_RETENTION  how long a file version nothing maps is kept: an age, `0`, or\n               \
        `never` (default 14d); --gc, --status and the background cleanup read it"
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
    /// `test-only` (only tests reach it: a method only they call, a class
    /// only they name), `override` (none
    /// does, but it overrides an ancestor's method, so a call of that may run
    /// it), `convention-only` (named only by a symbol handed to a macro, a
    /// route, or a name Rails or a library looks a class up by),
    /// `super-only` (reached only by `super` from its overrides), and
    /// `single-caller` (one call: the inlining candidate). Each is `clear`, or
    /// `lower` confidence when its `caveat` names a way in trekr cannot see
    /// — for a class, module or constant, a name the checkout computes and
    /// looks up at runtime; for any row, the file sends a computed name,
    /// the one caller's receiver is untyped, it overrides a method, a view, a
    /// script or a gem names it, the checkout
    /// builds a name of its shape, an ancestor is not indexed, or it is an
    /// entry point reached from outside the repo (`entry_point`: a gem's
    /// public API, a routed action, a job). A method whose only caller is
    /// itself a candidate is `single-caller`, and its reason says so; when
    /// that caller is a clear `unreferenced` or `test-only` method,
    /// `cascaded_from` names it, since deleting it frees this one. Handed a
    /// Gemfile, `gems.rb` or a gemspec instead, it grades the gems declared
    /// there (`kind: gem`): `unreferenced` when nothing names a constant,
    /// file or mixin method of theirs, `test-only`, or `convention-only`
    /// when a config, a command line, a symbol or another gem loads one by
    /// name; `lower` when a Railtie, a patch, a command, a plugin's name or
    /// its group may use it unseen. `--explain` lists the gems found in use.
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

    /// Forget a checkout's file map (its blobs stay for the worktrees that
    /// share them, and those no other maps for `TREKR_RETENTION`), and the copy
    /// of its edits queries keep in `trekr.overlays/` beside the index.
    #[arg(long, value_name = "PATH", num_args = 0..=1, default_missing_value = ".")]
    drop: Option<PathBuf>,

    /// Remove checkouts nothing will ask about again: gem versions no
    /// surviving project's bundle names, and projects whose root is gone.
    /// Their blobs go too, unless another checkout still maps them, and so do
    /// versions of files no checkout has mapped for `TREKR_RETENTION`. Also
    /// removes an index set aside as unusable, an early copy a stopped index
    /// left, another trekr's own index once it is idle, and every copy of
    /// the working tree's edits queries keep in `trekr.overlays/` (the next
    /// query over edits writes its own again).
    #[arg(long, conflicts_with_all = ["index", "status", "symbols", "refs", "def", "ancestors", "drop", "lsp"])]
    gc: bool,

    /// With `--gc`: report what would be removed, and the space it would free,
    /// without removing it. It still adds an index this version's schema has
    /// and the store lacks, which removes nothing, and says so.
    #[arg(long, requires = "gc")]
    dry_run: bool,

    /// With `--gc`: spare a checkout an index saw more recently than this —
    /// `36h`, `7d`, `2w`, or `0` for every one collectable now. A version no
    /// checkout maps goes by `TREKR_RETENTION` instead.
    #[arg(long, value_name = "AGE", requires = "gc", default_value = "7d", value_parser = crate::cleanup::parse_age)]
    older_than: u64,

    /// One background cleanup pass over this store, as an index starts it
    /// (DEC-650). Internal.
    #[arg(long, hide = true, value_name = "STORE", conflicts_with_all = ["index", "status", "gc", "lsp"])]
    cleanup: Option<PathBuf>,

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
    // The matches are kept to tell a knob given from its default, for the count.
    let parsed = Cli::command()
        .try_get_matches()
        .and_then(|matches| Cli::from_arg_matches(&matches).map(|cli| (cli, matches)));
    let (cli, matches) = match parsed {
        Ok(parsed) => parsed,
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
    // `--dead Gemfile` grades the bundle's gems (DEC-670).
    let gems_scope = cli.dead.iter().any(|path| dead_gems::is_bundle_file(path));
    for (on, flag, applies) in [
        (cli.explain, "--explain", position || gems_scope),
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
    } else if let Some(store) = &cli.cleanup {
        crate::cleanup::command(store);
        (None, Ok(ExitCode::SUCCESS))
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
    } else if let Some(written) = &cli.def {
        let def = position::Spec::parse(written)
            .ok_or_else(|| Failure::Usage.error(format!("expected FILE:LINE:COL, got `{written}`")))
            .and_then(|spec| spec.map_err(|why| Failure::Usage.error(why)))
            .and_then(|spec| cmd_def(out, written, &spec, cli.explain, cli.context.as_deref()));
        (Some("def"), def)
    } else if gems_scope {
        crate::usage::flag("gems");
        (
            Some("dead"),
            dead_gems::cmd_dead_gems(out, &cli.dead, cli.explain),
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
        (Some("gc"), cmd_gc(out, cli.older_than, cli.dry_run))
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
    // After the answer is out, never before it (rq DECISIONS D13). An index
    // trekr spawned is counted by its spawner: a query's row, as `indexed`,
    // or the language server's own `index` row.
    if let Some(feature) = feature
        && !autoindex::spawned_by_trekr()
    {
        let note = crate::usage::take();
        let mut flags = note.flags;
        flags.extend(cli_flags(&cli, &matches, out));
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
fn cli_flags(cli: &Cli, matches: &clap::ArgMatches, out: Output) -> Vec<&'static str> {
    // Written on this command line: a standing `TREKR_JOBS` would mark every call.
    let given = |id: &str| matches.value_source(id) == Some(clap::parser::ValueSource::CommandLine);
    [
        (out == Output::Json, "json"),
        (out == Output::Ndjson, "ndjson"),
        (cli.explain, "explain"),
        (cli.context.is_some(), "context"),
        (cli.include_excluded, "include-excluded"),
        (cli.no_gems, "no-gems"),
        (cli.no_index, "no-index"),
        (cli.dry_run, "dry-run"),
        (cli.profile, "profile"),
        (cli.all, "all"),
        (given("older_than"), "older-than"),
        (given("jobs"), "jobs"),
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

/// What is indexed. By default the checkout this is run in (or `--context`
/// names), with its gems counted rather than listed — a Rails app's bundle is
/// hundreds of them — and a count of the rest; `--all` lists every checkout.
/// Outside any checkout, the repos are listed and the gems counted. A checkout
/// nobody indexed is `not_indexed`, exit 2, as a query from it would be.
fn cmd_status(out: Output, all: bool, context: Option<&Path>) -> anyhow::Result<ExitCode> {
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
    let kind = match owner.as_str() {
        "" => refs::method_of(&tree, &owner, &query),
        _ => tree.lookup(&owner, query.singleton, &query.name),
    }
    .map(|method| method.kind());
    let written = crate::tree::ruby_owner(&owner);
    let shown = match query.singleton {
        true => format!("{written}.{}", query.name),
        false => format!("{written}#{}", query.name),
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
        "owner": written,
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
    let owner_ok = constant_path(owner.strip_prefix("::").unwrap_or(owner));
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
    if let Some(spec) = position::Spec::parse(text) {
        let spec = spec.map_err(|why| Failure::Usage.error(why))?;
        return cmd_refs_at(out, text, &spec, include_excluded, context);
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

    // `--json` is a bare array for a name, with no room for `index`: it is
    // said on stderr, as text says it. NDJSON's closing line carries it.
    let say = match out {
        Output::Json => Output::Text,
        other => other,
    };
    // A name written with `::` is a constant's whole name: the mentions Ruby's
    // lookup resolves to it, however each is written.
    if let Some(fqn) = whole_constant_name(&query) {
        crate::usage::flag("by-name");
        let tree = fresh_tree(say, &mut store, &root, None)?;
        return constant_refs(out, &tree, &store, &root_str, fqn);
    }
    // A bare name narrows nothing, so it keeps the whole-mention view —
    // definitions and constant references included, which a method-shaped
    // query has no use for.
    if query.owner.is_none() {
        crate::usage::flag("by-name");
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
        "owner": owner.as_deref().map(crate::tree::ruby_owner),
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
    spec: &position::Spec,
    include_excluded: bool,
    pinned: Option<&Path>,
) -> anyhow::Result<ExitCode> {
    let line = spec.line.get();
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
        crate::query::members::member_at_position(&tree, &files, &relative, line, spec.col)
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
    if let Some(col) = spec.col.map(NonZeroU32::get)
        && let Some((head, rows)) =
            variable_mentions(&tree, &root_str, &absolute, &source, &facts, line, col)
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
        None => crate::query::position::at_or_snap(&facts, line, None).map(|(under, _)| under),
        Some(col) => crate::query::position::at_facts(&facts, line, col.get()),
    };
    // A class, module or constant: its references, as `--refs` by its whole
    // name lists them.
    let constant = match &under {
        Some(crate::query::position::Under::Definition(def))
            if def.kind != crate::core::Kind::Method =>
        {
            let mut nesting = def.nesting.clone();
            nesting.insert(0, def.name.clone());
            tree.scope_fqn(&nesting)
        }
        Some(crate::query::position::Under::Constant(reference)) => Some(
            crate::query::refs::placed_name(&tree, &reference.name, &reference.nesting, &relative),
        ),
        _ => None,
    };
    if let Some(fqn) = constant {
        return constant_refs(out, &tree, &store, &root_str, &fqn);
    }
    let owner_and_name = match under {
        Some(crate::query::position::Under::Definition(def))
            if def.kind == crate::core::Kind::Method =>
        {
            // A top-level def is Object's (DEC-837); one on an object
            // (`def io.x`) is no class's.
            let top_level = def.nesting.is_empty() && !def.singleton;
            tree.scope_fqn(&def.nesting)
                .or_else(|| top_level.then(|| "Object".to_string()))
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
    facts: &crate::core::Facts,
    line: u32,
    col: u32,
) -> Option<(serde_json::Value, Vec<Mention>)> {
    use crate::resolve::vars::{self, Sigil};
    let path = file.to_string_lossy();
    let source = crate::extract::ruby_source(&path, raw);
    let here = vars::of_file(&source, &facts.strings);
    let want = crate::query::position::variable_at(facts, &source, &here, line, col)?.clone();
    let head = position::variable_answer(&source, &here, &want, &path);
    crate::usage::flag("variable");
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

    mentions(out, &rows, &query.name, "")
}

/// `--refs` by a constant's whole name, or at a class, module or constant:
/// its definitions and the references Ruby's lookup resolves to it
/// (`query::refs::constant_mentions`), in the rows a bare name lists.
fn constant_refs(
    out: Output,
    tree: &Tree,
    store: &Store,
    root_str: &str,
    fqn: &str,
) -> anyhow::Result<ExitCode> {
    let rows = crate::query::refs::constant_mentions(tree, store, root_str, fqn)?;
    let mut read = " (a whole name, read from the top level)".to_string();
    if rows.is_empty() && out == Output::Text {
        if let Some(has) = in_a_singleton_class(tree, store, root_str, fqn)? {
            read.push_str(&format!("; assigned in `class << self`, it is {has}"));
        } else if let Some((has, ancestor)) = found_as(tree, fqn) {
            read.push_str(&match ancestor {
                Some((scope, ancestor)) => {
                    format!("; found through {scope}'s ancestor {ancestor}, it is {has}")
                }
                None => format!("; Ruby's lookup reaches it as {has}"),
            });
        }
    }
    mentions(out, &rows, fqn, &read)
}

/// The constant Ruby's lookup reaches for a whole name that names none
/// itself — `File::NULL` is `IO::NULL` — and, when its scope's ancestor is
/// why, that scope and ancestor. A mention is listed under the name it
/// reaches, so the name asked has none.
fn found_as(tree: &Tree, fqn: &str) -> Option<(String, Option<(String, String)>)> {
    let has = tree
        .resolve(&format!("::{fqn}"), &[])
        .fqn
        .filter(|has| has != fqn)?;
    let ancestor =
        fqn.rsplit_once("::")
            .zip(has.rsplit_once("::"))
            .and_then(|((scope, _), (owner, _))| {
                let ancestors = tree.ancestors(scope);
                // The chain starts with the scope itself; an alias's is its target's.
                let mut chain = ancestors.chain.iter().map(|a| public_name(a));
                (chain.next() == Some(scope) && chain.any(|a| a == owner))
                    .then(|| (scope.to_string(), owner.to_string()))
            });
    Some((has, ancestor))
}

/// The name a constant has when the whole name asked for missed only because
/// it was assigned in a `class << self` body (DEC-647): `Widget::LIMIT` as
/// `Widget::singleton_class::LIMIT`, the segment tried before each name.
fn in_a_singleton_class(
    tree: &Tree,
    store: &Store,
    root_str: &str,
    fqn: &str,
) -> anyhow::Result<Option<String>> {
    let segments: Vec<&str> = fqn.split("::").collect();
    for at in (1..segments.len()).rev() {
        let mut named = segments.clone();
        named.insert(at, crate::core::SINGLETON_CLASS);
        let named = named.join("::");
        if !crate::query::refs::constant_mentions(tree, store, root_str, &named)?.is_empty() {
            return Ok(Some(named));
        }
    }
    Ok(None)
}

/// A constant's whole name as `--refs` was given it: written with `::`, a
/// [`constant_path`]. `::Widget` is the top-level one.
fn whole_constant_name(query: &crate::resolve::refs::Query) -> Option<&str> {
    if query.owner.is_some() || !query.name.contains("::") {
        return None;
    }
    let name = query.name.strip_prefix("::").unwrap_or(&query.name);
    constant_path(name).then_some(name)
}

/// `Foo::Bar` as trekr spells a constant: every segment a constant, or a
/// class's `singleton_class` (Ruby's own spelling of a `class << self`
/// scope) before the last. A method's owner and a whole constant name are
/// both one, so whatever `--dead` prints, `--refs` takes back.
fn constant_path(name: &str) -> bool {
    let constant = |segment: &str| {
        segment.starts_with(char::is_uppercase)
            && segment.chars().all(|c| c.is_alphanumeric() || c == '_')
    };
    let mut segments: Vec<&str> = name.split("::").collect();
    segments.pop().is_some_and(constant)
        && segments
            .into_iter()
            .all(|segment| constant(segment) || segment == crate::core::SINGLETON_CLASS)
}

/// A name's mentions, one row each, in whichever shape the caller asked for;
/// a miss says `read`, how the name was read.
fn mentions(
    out: Output,
    rows: &[crate::store::Ref],
    name: &str,
    read: &str,
) -> anyhow::Result<ExitCode> {
    if emit_rows(out, rows)? {
        return Ok(exit_on(!rows.is_empty()));
    }
    if rows.is_empty() {
        // Asked of an index, so a miss is a no: not a reason to index again.
        println!("no mention of {name} in what trekr indexed{read}");
        return Ok(ExitCode::from(1));
    }
    for row in rows {
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

/// Whether an `.rbi` the checkout sees defines a method `name` — where every
/// signature comes from, so a `def` with none answers without the tree.
fn any_rbi_defines(store: &Store, root: &str, name: &str) -> anyhow::Result<bool> {
    Ok(store.rbi_defines(&store.tree_roots(root)?, name)?)
}

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
    if let Some(spec) = position::Spec::parse(input) {
        crate::usage::feature("def");
        let spec = spec.map_err(|why| Failure::Usage.error(why))?;
        return cmd_def(out, input, &spec, explain, context);
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
    // `--no-index` beside another trekr's first index: "never indexed" would
    // send the caller to start a second one.
    let (reason, say) = match store.warming(&root)?.filter(|w| !w.interrupted) {
        Some(w) => (
            format!(
                "another trekr is indexing it now ({}); without --no-index a query waits for it",
                w.how_far()
            ),
            true,
        ),
        None => not_indexed_reason(store)?,
    };
    match out {
        Output::Text if say => eprintln!(
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
    written: &str,
    spec: &position::Spec,
    explain: bool,
    pinned: Option<&Path>,
) -> anyhow::Result<ExitCode> {
    let line = spec.line.get();
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
    let vars = crate::resolve::vars::of_file(&source, &facts.strings);
    let requires = match spec.col {
        Some(_) => crate::query::require::requires_in(&source),
        None => Vec::new(),
    };
    let subject = match spec.col {
        // No column: the line chooses, by the snap below (DEC-036).
        None => Subject::Nothing,
        Some(col) => {
            let at = crate::core::Pos {
                line,
                col: col.get(),
            };
            crate::query::def::subject(&facts, &source, &vars, &requires, at, |template| {
                let Some((root, store)) = checkout.as_mut().map(|(root, store)| (&*root, store))
                else {
                    return Ok(None);
                };
                let relative = std::fs::canonicalize(&spec.path)
                    .ok()
                    .and_then(|abs| abs.strip_prefix(root).ok().map(Path::to_path_buf))
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|| spec.path.clone());
                let tree = || fresh_tree(out, store, root, Some(Path::new(&spec.path)));
                crate::query::def::reach(template, &facts, root, &relative, tree).map(Some)
            })?
        }
    };
    let root = checkout.as_ref().map(|(root, _)| root.clone());
    let template = |reached: &Reached| {
        let root = root
            .as_deref()
            .expect("a template is reached through the file's checkout");
        template_answer(
            written.to_string(),
            root,
            &reached.files,
            reached.class.as_deref(),
        )
    };
    let found = match subject {
        // A `require` string: the file it loads, as Go to Definition opens it
        // (DEC-053). Read from the file and the load path alone.
        Subject::Require(require) => {
            file_alone(&checkout);
            let (answer, text) = required_answer(written, &file, &require, checkout.as_ref())?;
            let found = !answer["definition"].as_array().is_none_or(Vec::is_empty);
            return report(out, answer, found, &text);
        }
        // A template a `render` or `extends` names: the file it reaches (DEC-524).
        Subject::Template(reached) => {
            let text = match (reached.files.first(), &root) {
                (Some(first), Some(root)) => format!(
                    "{}:1:1  template",
                    shown(&root.join(first).to_string_lossy())
                ),
                _ => "no template by that name".to_string(),
            };
            return report(out, template(&reached), !reached.files.is_empty(), &text);
        }
        // A `super` with no fact behind it is one whose method has no owner
        // the source names. Snapping would answer for another name on the line.
        Subject::Super => {
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
        Subject::Symbol(name) => {
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
        // A variable is not a call, and snapping from one answered for
        // whatever name was nearest on the line. A `render` written as it
        // that reaches no file is said beside it when it finds no value either.
        Subject::Variable {
            occurrence,
            unreached,
        } => {
            crate::usage::flag("variable");
            let no_template = unreached.as_ref().map(template);
            let mut answer = position::variable_answer(&source, &vars, &occurrence, &file);
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
                from_controller =
                    crate::resolve::views::template_ivar_writes(&tree, &relative, &name);
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
            // Neither found: the missing partial is the more actionable reason,
            // and the variable's is said beside it.
            if let Some(mut template) = no_template
                && answer["definition"].as_array().is_some_and(Vec::is_empty)
            {
                let variable = format!(
                    "{} {}",
                    answer["name"].as_str().unwrap_or_default(),
                    answer["reason"].as_str().unwrap_or_default(),
                );
                let variable = variable.trim_end();
                template["reason"] =
                    format!("no template in the checkout's views by that name; {variable}").into();
                return report(
                    out,
                    template,
                    false,
                    &format!("no template by that name; {variable}"),
                );
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
        Subject::Fact(under) => Some((under, None)),
        Subject::Nothing => crate::query::position::at_or_snap(&facts, line, spec.col),
    };
    let Some((under, snapped)) = found else {
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
    } else if let Some((root, store)) = &checkout {
        let need = Need::File(Path::new(&spec.path));
        if let Some(code) = autoindex::ensure(out, store, root, need)? {
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
                answer["owner"] =
                    crate::tree::ruby_owner(def.nesting.first().map_or("", String::as_str)).into();
                // Its `.rbi` signatures, as Go to Declaration opens them
                // (DEC-646): from the index as it stands, so the answer on
                // a `def` stays the file's own, with no git probe or wait.
                if let Some((root, store)) = &checkout
                    && store.has_checkout(&root.to_string_lossy())?
                    && any_rbi_defines(store, &root.to_string_lossy(), &def.name)?
                {
                    let tree = build_tree(store, &root.to_string_lossy())?;
                    let signatures = crate::resolve::signatures_of_def(&tree, &def);
                    if !signatures.is_empty() {
                        answer["signatures"] = serde_json::to_value(signatures)?;
                    }
                }
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
            // A Sorbet signature is listed, after the code it describes (DEC-646).
            crate::query::locations::real_first(&mut resolution.sites);
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
            let root_str = root.to_string_lossy().into_owned();
            let answer_from = |tree: &Tree| {
                let files = crate::query::members::CheckoutFiles::new(&store, &root, &root_str);
                crate::query::def::method_answer(tree, &files, &facts, &call, &relative)
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
        // The JSON carries `index.stale` and its `cause` beside the reason;
        // a line of text has only itself to say it.
        None => format!(
            "{}  {}{}",
            answer["name"].as_str().unwrap_or("?"),
            answer["reason"].as_str().unwrap_or("unresolved"),
            lag()
                .map(|lag| format!("; but {lag}, so a file trekr has not read may define it"))
                .unwrap_or_default(),
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
                None => "no column given".to_string(),
                Some(col) => format!("no name at column {col}"),
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

/// The answer on a `require` string, and its text: each file it loads, at
/// its top, in load-path order. A compiled extension is no file to open, and
/// Ruby loads it before any `.rb` after it, so it is said rather than listed.
fn required_answer(
    query: &str,
    file: &str,
    require: &crate::query::require::Require,
    checkout: Option<&(PathBuf, Store)>,
) -> anyhow::Result<(serde_json::Value, String)> {
    use crate::query::require::{self, LoadPath};
    let root = checkout.map(|(root, _)| root.as_path());
    let load_path = match checkout {
        Some((root, store)) => {
            let root_str = root.to_string_lossy();
            let gems = store.gems_used(&root_str)?;
            let stdlib = store.tree_roots(&root_str)?.stdlib;
            LoadPath::for_checkout(root, &gems, stdlib.as_deref())
        }
        None => LoadPath::default(),
    };
    let cx = require::Context {
        file: Path::new(file),
        root,
        load_path: &load_path,
    };
    let found = require::resolve(require, &cx, Path::is_file);
    let native = found.iter().find(|found| found.native);
    let files: Vec<String> = found
        .iter()
        .filter(|found| !found.native)
        .map(|found| found.file.to_string_lossy().into_owned())
        .collect();
    let definition: Vec<serde_json::Value> = files
        .iter()
        .map(|path| serde_json::json!({"path": path, "line": 1, "col": 1, "kind": "file"}))
        .collect();
    let mut answer = serde_json::json!({
        "query": query,
        "under": "require",
        "name": require.path,
        "status": match files.len() { 0 => "residue", 1 => "resolved", _ => "ambiguous" },
        "confidence": crate::resolve::share(1, files.len()),
        "resolved_via": "require",
        "definition": definition,
    });
    let text = match native {
        Some(native) => {
            let reason = format!(
                "a compiled extension, {}: nothing to open",
                shown(&native.file.to_string_lossy())
            );
            answer["reason"] = reason.as_str().into();
            format!("{}  {reason}", require.path)
        }
        None if files.is_empty() => {
            let reason = format!(
                "no file found for `{}`: a gem that is not installed, or a path set up at runtime",
                require.path
            );
            answer["reason"] = reason.as_str().into();
            reason
        }
        None => files
            .iter()
            .map(|path| format!("{}:1:1  file", shown(path)))
            .collect::<Vec<_>>()
            .join("\n"),
    };
    Ok((answer, text))
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
    let mut store = open_store()?;
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

fn cmd_gc(out: Output, older_than: u64, dry_run: bool) -> anyhow::Result<ExitCode> {
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
    let db_bytes = store.db_bytes()?;
    let free_bytes = store.free_bytes()?;
    // What left the disk: the files. A delete inside the index frees pages
    // for later writes; the background cleanup hands them back (DEC-650).
    let reclaimed_bytes =
        (snapshots.bytes + overlay_bytes + core.bytes + kept.iter().map(|k| k.bytes).sum::<u64>())
            as i64;
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
    let mut versions: BTreeMap<String, i64> = BTreeMap::new();
    for row in rows {
        let version = match row.version.as_str() {
            "" => "unrecorded",
            v => v,
        };
        *versions.entry(version.to_string()).or_default() += row.count;
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
    let _ = writeln!(text, "trekr versions: {}", ranked(&versions));
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
            version: String::new(),
            count: 2,
        };
        let text = usage_text(&[row("session")], None);
        assert!(text.contains("editor (--lsp)"), "{text}");
        assert!(text.contains("sessions 2"), "{text}");
    }

    #[test]
    fn the_summary_counts_each_version_and_the_rows_from_before_them() {
        let row = |version: &str, count| crate::usage::Row {
            day: "2026-01-01".into(),
            surface: "cli".into(),
            feature: "refs".into(),
            flags: String::new(),
            origin: "human".into(),
            outcome: "hit".into(),
            latency: String::new(),
            cold: false,
            version: version.into(),
            count,
        };
        let text = usage_text(&[row("", 5), row("1.0.0", 2)], None);
        assert!(
            text.contains("trekr versions: unrecorded 5 · 1.0.0 2"),
            "{text}"
        );
    }
}
