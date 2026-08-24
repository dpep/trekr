//! The command line. Every command that prints anything honors `--json` and
//! `--ndjson`, because the primary consumer is an agent, not a person.
//!
//! Operations are flags rather than subcommands (rq's convention): no word is
//! reserved, and the default action stays free for the query verbs the resolve
//! layer will add.

use crate::core::Oid;
use crate::store::Store;
use crate::{extract, scan};
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

    /// Outline one file's definitions, in the order they are written.
    #[arg(long, value_name = "FILE", conflicts_with_all = ["index", "drop"])]
    symbols: Option<PathBuf>,

    /// Forget a checkout's file map (its blobs stay, for the worktrees that
    /// share them).
    #[arg(long, value_name = "PATH", num_args = 0..=1, default_missing_value = ".")]
    drop: Option<PathBuf>,

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
    let out = if cli.ndjson {
        Output::Ndjson
    } else if cli.json {
        Output::Json
    } else {
        Output::Text
    };

    let result = if let Some(path) = &cli.index {
        cmd_index(out, path)
    } else if let Some(path) = &cli.symbols {
        cmd_symbols(out, path)
    } else if let Some(path) = &cli.drop {
        cmd_drop(out, path)
    } else if cli.status {
        cmd_status(out)
    } else {
        eprintln!("trekr: nothing to do (try --index, --status, --symbols FILE)");
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

fn cmd_index(out: Output, path: &Path) -> anyhow::Result<ExitCode> {
    let root = scan::repo_root(path)?;
    let root_str = root.to_string_lossy().into_owned();
    let files = scan::scan(&root)?;

    let mut store = open_store()?;
    let wanted: HashSet<Oid> = files.values().cloned().collect();
    let known = store.known(&wanted)?;

    // One path per unknown blob: identical content under two names is one
    // parse, and which name it was read from cannot matter.
    let mut to_parse: HashMap<&Oid, PathBuf> = HashMap::new();
    for (rel, oid) in &files {
        if !known.contains(oid) {
            to_parse.entry(oid).or_insert_with(|| root.join(rel));
        }
    }

    let facts: Vec<_> = to_parse
        .into_par_iter()
        .filter_map(|(oid, path)| {
            let bytes = std::fs::read(&path).ok()?;
            Some((oid.clone(), extract::extract(&bytes)))
        })
        .collect();

    let counts = store.write(&root_str, &files, facts)?;
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
            &serde_json::json!({ "repo": root_str, "indexed": counts }),
        )?,
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

fn cmd_symbols(out: Output, path: &Path) -> anyhow::Result<ExitCode> {
    let absolute = std::fs::canonicalize(path)?;
    let root = scan::repo_root(&absolute)?;
    let relative = absolute
        .strip_prefix(&root)
        .unwrap_or(&absolute)
        .to_string_lossy()
        .into_owned();
    let store = open_store()?;
    let symbols = store.symbols(&root.to_string_lossy(), &relative)?;

    if emit_rows(out, &symbols)? {
        return Ok(exit_on(!symbols.is_empty()));
    }
    if symbols.is_empty() {
        println!("no symbols for {relative} (indexed? try `trekr --index`)");
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
