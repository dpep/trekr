//! One harness, many cases: the corner cases we have already paid for.
//!
//! Every case in `tests/testbed/` is a directory holding a tiny Ruby source
//! tree and an `expected` file. This test iterates all of them, so **adding a
//! case is dropping in files — no Rust**. That is the whole point: three
//! sessions of hard-won corner cases (an ancestor cycle that killed the
//! process, an override living in a sibling module, a Sorbet stub shadowing
//! real source) deserve a form where the next one costs nothing to record.
//!
//! The `expected` format is one assertion per line, so it stays writable by
//! hand:
//!
//! ```text
//! # why this case exists
//! def app.rb:8:7   status=resolved owner=Widget via=local:new
//! def app.rb:12:11 status=residue candidates=2
//! refs Widget#save confirmed=2 possible=0
//! symbols app.rb   Widget,save,Job,run
//! hover app.rb:12:11 Declared by `attr_reader` in
//! ```
//!
//! `hover` drives the **LSP** rather than the CLI, because some of what an
//! answer carries reaches an editor only through hover — `textDocument/
//! definition` is a bare list of locations and cannot say what kind of location
//! it handed back. A case that stages a server-visible shape should pin the
//! wire, not only the command line.
//!
//! Keys for `def` are fields of `--def --json`: `status`, `owner`, `via`
//! (`resolved_via`), `name`, `confidence`, `candidates` (a count), `site`
//! (`path:line`, matched on the path's tail), `signature` (the first `.rbi`
//! stub, as `site`), and `candidate1` (the top candidate's owner).
//! `definition` and `declaration` lines assert what Go to Definition and Go to
//! Declaration list. Keys for `refs` are the `counts` object, `status` and
//! `resolves_to`. Unknown keys fail loudly rather than passing silently — a
//! typo in an expectation is a test that proves nothing.

#![allow(
    clippy::disallowed_methods,
    reason = "a test reads its own fixtures and scratch files"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

mod support;

use support::git;

/// Stage one case as a real checkout with its own database.
fn stage(case: &Path, label: &str) -> (PathBuf, PathBuf) {
    let (dir, db) = support::scratch(label);

    // Every case runs on a Ruby installed as rvm installs one, under a home
    // of its own, which every command is run with (DEC-180): its core
    // described by the rbs fixture it carries (DEC-240). A case's `ruby/` is
    // that Ruby's stdlib, and its `rbs/` is laid over the rbs gem: its
    // libraries' signatures in `rbs/stdlib/`, more of core's in `rbs/core/`.
    let home = home_of(&dir);
    let _ = fs::remove_dir_all(&home);
    let lib = home.join(".rvm/rubies/ruby-9.8.7/lib/ruby");
    fs::create_dir_all(lib.join("9.8.0")).unwrap();
    if case.join("ruby").is_dir() {
        copy_tree(&case.join("ruby"), &lib.join("9.8.0"));
    }
    fs::create_dir_all(lib.join("gems/9.8.0/specifications/default")).unwrap();
    let rbs = lib.join("gems/9.8.0/gems/rbs-9.9.9");
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rbs"),
        &rbs,
    );
    if case.join("rbs").is_dir() {
        copy_tree(&case.join("rbs"), &rbs);
    }
    fs::write(dir.join(".ruby-version"), "9.8.7\n").unwrap();

    // Everything but the expectations file is source.
    for entry in fs::read_dir(case).unwrap().flatten() {
        let name = entry.file_name();
        if name == "expected" || name == "README.md" || name == "ruby" || name == "rbs" {
            continue;
        }
        let target = dir.join(&name);
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }

    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "-A"]);
    git(
        &dir,
        &[
            "-c",
            "user.email=t@e.st",
            "-c",
            "user.name=test",
            "commit",
            "-qm",
            "case",
        ],
    );
    let indexed = trekr_in(&dir)
        .args(["--index", "--json"])
        .env("TREKR_DB", &db)
        .output()
        .expect("index the case");
    assert!(
        indexed.status.success(),
        "indexing {label} failed: {}",
        String::from_utf8_lossy(&indexed.stderr)
    );
    let answer = serde_json::from_slice(&indexed.stdout).unwrap_or_default();
    shapes::record(&["--index"], &answer);
    (dir, db)
}

/// Where a staged case's Ruby is installed, when it has one.
fn home_of(dir: &Path) -> PathBuf {
    dir.with_extension("home")
}

/// The binary, run in a staged case on the Ruby its home holds.
fn trekr_in(dir: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_trekr"));
    support::neutral(&mut command, &home_of(dir)).current_dir(dir);
    command
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// One `textDocument/hover`, over a real `--lsp` session against the staged
/// checkout. Returns the markdown the editor would show.
fn hover_text(db: &Path, dir: &Path, target: &str) -> String {
    lsp_at(db, dir, target, "textDocument/hover", serde_json::json!({}))["contents"]["value"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The locations `textDocument/references` lists at `FILE:LINE:COL`, as
/// `path:line:col` in the checkout, sorted: what the editor shows, to set
/// beside the CLI's rows for the same position.
fn lsp_references(db: &Path, dir: &Path, target: &str) -> Vec<String> {
    let result = lsp_at(
        db,
        dir,
        target,
        "textDocument/references",
        serde_json::json!({"context": {"includeDeclaration": false}}),
    );
    let mut found: Vec<String> = result
        .as_array()
        .into_iter()
        .flatten()
        .map(|location| site(dir, &location["uri"], &location["range"]["start"]))
        .collect();
    found.sort();
    found
}

/// The locations a `textDocument/definition` or `declaration` lists at
/// `FILE:LINE:COL`, as `path:line`, in the order the server gave them.
fn lsp_locations(lsp: &mut Lsp, dir: &Path, target: &str, method: &str) -> Vec<String> {
    let (file, line, col) = position(target);
    let uri = lsp.open(&file);
    let result = lsp.at(&uri, line, col, method, serde_json::json!({}));
    result
        .as_array()
        .into_iter()
        .flatten()
        .map(|location| {
            let full = site(dir, &location["uri"], &location["range"]["start"]);
            full.rsplit_once(':')
                .map_or(full.clone(), |(at, _)| at.to_string())
        })
        .collect()
}

/// A `--def --json` answer's sites under `field`, as `path:line` in the
/// checkout — `None` when any lies outside it, where the editor's path is a
/// written-out copy rather than the CLI's. A variable's sites have no
/// `root`: their path is the file's own.
fn cli_sites(dir: &Path, answer: &serde_json::Value, field: &str) -> Option<Vec<String>> {
    answer[field]
        .as_array()
        .into_iter()
        .flatten()
        .map(|site| {
            let path = site["path"].as_str().unwrap_or_default();
            let absolute = match site["root"].as_str() {
                Some(root) => format!("{root}/{path}"),
                None => path.to_string(),
            };
            in_checkout(dir, &absolute).map(|path| format!("{path}:{}", site["line"]))
        })
        .collect()
}

/// The `--def` answer for the variable an editor's caret at `FILE:LINE:COL`
/// reads where the column names something else (DEC-036 addendum). A column
/// names a character — `list[0]`'s `[` is the `[]` call — but a caret there
/// sits between `list` and `[`, and the identifier wins.
fn caret_variable(db: &Path, dir: &Path, target: &str) -> Option<serde_json::Value> {
    let (file, line, col) = position(target);
    let text = fs::read_to_string(dir.join(&file)).ok()?;
    let bytes = text.lines().nth(line.checked_sub(1)? as usize)?.as_bytes();
    let word = |i: usize| {
        bytes
            .get(i)
            .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
    };
    let at = col.checked_sub(1)? as usize;
    if at == 0 || !word(at - 1) || word(at) {
        return None;
    }
    let before = format!("{file}:{line}:{}", col - 1);
    let (answer, _) = trekr(db, dir, &["--def", &before, "--json"]);
    (answer["under"] == "variable").then_some(answer)
}

/// What the editor's definition and declaration must list for a placed
/// `--def` answer: an `.rbi` signature is a declaration, and a definition
/// only when it is all there is; a declaration falls back to the definition.
fn editor_owes(definition: &[String], signatures: &[String]) -> (Vec<String>, Vec<String>) {
    let (rbi, real): (Vec<String>, Vec<String>) = definition
        .iter()
        .cloned()
        .partition(|site| site.split(':').next().is_some_and(|p| p.ends_with(".rbi")));
    let defined = if real.is_empty() { rbi.clone() } else { real };
    let declared = if !signatures.is_empty() {
        signatures.to_vec()
    } else if !rbi.is_empty() {
        rbi
    } else {
        defined.clone()
    };
    (defined, declared)
}

/// The call sites incoming calls lists for what `prepareCallHierarchy`
/// prepares at `FILE:LINE:COL`, spelled as [`lsp_references`] spells them —
/// or `None` when nothing is prepared there.
fn lsp_incoming(db: &Path, dir: &Path, target: &str) -> Option<Vec<String>> {
    let (file, line, col) = position(target);
    let mut lsp = Lsp::start(db, dir);
    let uri = lsp.open(&file);
    let prepared = lsp.at(
        &uri,
        line,
        col,
        "textDocument/prepareCallHierarchy",
        serde_json::json!({}),
    );
    let item = prepared.get(0)?.clone();
    let incoming = lsp.request(
        "callHierarchy/incomingCalls",
        serde_json::json!({ "item": item }),
    );
    let mut found: Vec<String> = incoming
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|call| {
            let uri = &call["from"]["uri"];
            call["fromRanges"]
                .as_array()
                .into_iter()
                .flatten()
                .map(move |range| site(dir, uri, &range["start"]))
        })
        .collect();
    found.sort();
    Some(found)
}

/// The callee outgoing calls names for the call at `FILE:LINE:COL`, as
/// `path:line` — asked of the method around the call, so it is the answer
/// the walk gives, not a second route to it. `None` when no method encloses
/// the call; `Some(None)` when the call is not among the method's.
fn lsp_outgoing(lsp: &mut Lsp, dir: &Path, target: &str) -> Option<Option<String>> {
    let (file, line, col) = position(target);
    let at = serde_json::json!({
        "start": {"line": line - 1, "character": col - 1},
        "end": {"line": line - 1, "character": col},
    });
    let item = serde_json::json!({
        "name": "call",
        "kind": 6,
        "uri": format!("file://{}", dir.join(&file).display()),
        "range": at,
        "selectionRange": at,
    });
    let outgoing = lsp.request(
        "callHierarchy/outgoingCalls",
        serde_json::json!({ "item": item }),
    );
    let calls = outgoing.as_array()?;
    let covers = |range: &serde_json::Value| {
        let (start, end) = (&range["start"], &range["end"]);
        start["line"] == line - 1
            && start["character"]
                .as_u64()
                .is_some_and(|c| c < u64::from(col))
            && end["character"]
                .as_u64()
                .is_some_and(|c| c >= u64::from(col))
    };
    Some(
        calls
            .iter()
            .find(|call| {
                call["fromRanges"]
                    .as_array()
                    .is_some_and(|ranges| ranges.iter().any(covers))
            })
            .map(|call| {
                let to = &call["to"];
                let start = &to["selectionRange"]["start"];
                let full = site(dir, &to["uri"], start);
                full.rsplit_once(':')
                    .map_or(full.clone(), |(at, _)| at.to_string())
            }),
    )
}

/// A path relative to the staged checkout, when it is in it.
fn in_checkout(dir: &Path, path: &str) -> Option<String> {
    [Some(dir.to_path_buf()), fs::canonicalize(dir).ok()]
        .into_iter()
        .flatten()
        .find_map(|root| {
            path.strip_prefix(&format!("{}/", root.display()))
                .map(str::to_string)
        })
}

/// An LSP location as `path:line:col`, 1-based, the path relative to the
/// staged checkout.
fn site(dir: &Path, uri: &serde_json::Value, start: &serde_json::Value) -> String {
    let uri = uri.as_str().unwrap_or_default();
    let path = percent_decoded(uri.strip_prefix("file://").unwrap_or(uri));
    let path = in_checkout(dir, &path).unwrap_or_else(|| uri.to_string());
    format!(
        "{path}:{}:{}",
        start["line"].as_u64().unwrap_or_default() + 1,
        start["character"].as_u64().unwrap_or_default() + 1
    )
}

/// A URI's path as the file system spells it: a gem RBI's `@` arrives `%40`.
fn percent_decoded(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (byte, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The CLI's listed rows of a `--refs --json` answer, as [`lsp_references`]
/// spells them: a method's `references`, or a bare name's mentions — those
/// of one `tier`, when it is given.
fn cli_references(answer: &serde_json::Value, tier: Option<&str>) -> Vec<String> {
    let rows = answer["references"].as_array().or(answer.as_array());
    let mut found: Vec<String> = rows
        .into_iter()
        .flatten()
        .filter(|row| tier.is_none_or(|tier| row["tier"] == tier))
        .map(|row| {
            format!(
                "{}:{}:{}",
                row["path"].as_str().unwrap_or_default(),
                row["line"],
                row["col"]
            )
        })
        .collect();
    found.sort();
    found
}

/// One request at `FILE:LINE:COL`, over a real `--lsp` session against the
/// staged checkout with that file open: its `result`.
fn lsp_at(
    db: &Path,
    dir: &Path,
    target: &str,
    method: &str,
    extra: serde_json::Value,
) -> serde_json::Value {
    let (file, line, col) = position(target);
    let mut lsp = Lsp::start(db, dir);
    let uri = lsp.open(&file);
    lsp.at(&uri, line, col, method, extra)
}

/// `FILE:LINE:COL` as its parts; a missing line or column is 1.
fn position(target: &str) -> (String, u32, u32) {
    let mut bits = target.rsplitn(3, ':');
    let col: u32 = bits.next().unwrap_or("1").parse().unwrap_or(1);
    let line: u32 = bits.next().unwrap_or("1").parse().unwrap_or(1);
    (bits.next().unwrap_or_default().to_string(), line, col)
}

/// A real `--lsp` session against a staged checkout, for questions that take
/// more than one request — an item prepared, then expanded.
struct Lsp {
    child: std::process::Child,
    stdin: Option<std::process::ChildStdin>,
    stdout: std::io::BufReader<std::process::ChildStdout>,
    over: Option<std::sync::mpsc::Sender<()>>,
    watchdog: Option<std::thread::JoinHandle<()>>,
    dir: PathBuf,
    next: u64,
}

impl Lsp {
    fn start(db: &Path, dir: &Path) -> Lsp {
        let mut child = trekr_in(dir)
            .arg("--lsp")
            .env("TREKR_DB", db)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("lsp");
        let stdin = child.stdin.take();
        let stdout = std::io::BufReader::new(child.stdout.take().unwrap());

        // A pipe read has no timeout, and a server that never answers would
        // hang CI rather than fail it — which is exactly how the retirement
        // bug reached main, passing on macOS and parking forever on Linux.
        // Bound the wait so the worst case is a red test. The watchdog stands
        // down once the reading is over, and is joined before `wait` reaps the
        // child: a reaped pid is free for the system to hand to another
        // process.
        let pid = child.id();
        let (over, told) = std::sync::mpsc::channel::<()>();
        let watchdog = std::thread::spawn(move || {
            let waited = told.recv_timeout(std::time::Duration::from_secs(30));
            if waited == Err(std::sync::mpsc::RecvTimeoutError::Timeout) {
                let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
            }
        });
        let mut lsp = Lsp {
            child,
            stdin,
            stdout,
            over: Some(over),
            watchdog: Some(watchdog),
            dir: dir.to_path_buf(),
            next: 1,
        };
        lsp.request(
            "initialize",
            serde_json::json!({"rootUri": format!("file://{}", dir.display()), "capabilities":{}}),
        );
        lsp.send(serde_json::json!({"jsonrpc":"2.0","method":"initialized","params":{}}));
        lsp
    }

    /// Open `file` as the editor would; its URI.
    fn open(&mut self, file: &str) -> String {
        let path = self.dir.join(file);
        let uri = format!("file://{}", path.display());
        let text = fs::read_to_string(&path).unwrap_or_default();
        self.send(
            serde_json::json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{
            "textDocument":{"uri":uri,"languageId":"ruby","version":1,"text":text}}}),
        );
        uri
    }

    /// A request at a 1-based position in `uri`.
    fn at(
        &mut self,
        uri: &str,
        line: u32,
        col: u32,
        method: &str,
        extra: serde_json::Value,
    ) -> serde_json::Value {
        let mut params = serde_json::json!({
            "textDocument":{"uri":uri},
            "position":{"line": line - 1, "character": col - 1}});
        if let (Some(params), Some(extra)) = (params.as_object_mut(), extra.as_object()) {
            params.extend(extra.clone());
        }
        self.request(method, params)
    }

    fn send(&mut self, value: serde_json::Value) {
        use std::io::Write;
        let Some(stdin) = self.stdin.as_mut() else {
            return;
        };
        let body = serde_json::to_vec(&value).unwrap();
        let _ = stdin.write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes());
        let _ = stdin.write_all(&body);
        let _ = stdin.flush();
    }

    /// One request's `result`, `null` when the server never answered it.
    fn request(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        use std::io::BufRead;
        let id = self.next;
        self.next += 1;
        self.send(serde_json::json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
        // Notifications — diagnostics, progress — may come first.
        for _ in 0..64 {
            let mut length = 0usize;
            loop {
                let mut header = String::new();
                if self.stdout.read_line(&mut header).unwrap_or(0) == 0 {
                    break;
                }
                let header = header.trim().to_string();
                if header.is_empty() {
                    break;
                }
                if let Some(rest) = header.strip_prefix("Content-Length: ") {
                    length = rest.parse().unwrap_or(0);
                }
            }
            if length == 0 {
                break;
            }
            let mut body = vec![0u8; length];
            if std::io::Read::read_exact(&mut self.stdout, &mut body).is_err() {
                break;
            }
            let message: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
            if message["id"] == serde_json::json!(id) {
                return message["result"].clone();
            }
        }
        serde_json::Value::Null
    }
}

impl Drop for Lsp {
    fn drop(&mut self) {
        drop(self.stdin.take());
        drop(self.over.take());
        if let Some(watchdog) = self.watchdog.take() {
            let _ = watchdog.join();
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn trekr(db: &Path, dir: &Path, args: &[&str]) -> (serde_json::Value, i32) {
    let out = trekr_in(dir)
        .args(args)
        .env("TREKR_DB", db)
        .output()
        .expect("run trekr");
    let code = out.status.code().unwrap_or(-1);
    let parsed = serde_json::from_slice(&out.stdout).unwrap_or(serde_json::Value::Null);
    shapes::record(args, &parsed);
    (parsed, code)
}

/// The JSON shape golden: every field path of every `--json` answer the run
/// sees, per command, with the JSON types found there
/// (`--dead $.candidates[].mentions_by_name: int|null`), compared with
/// `tests/json-shapes.golden` once the whole testbed has run. A field that
/// changes type or vanishes breaks a caller who parses it, and compiles fine.
mod shapes {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Mutex;

    const GOLDEN: &str = "tests/json-shapes.golden";
    const REGENERATE: &str = "UPDATE_GOLDEN=1 cargo test --test testbed";

    static SEEN: Mutex<BTreeMap<String, BTreeSet<&'static str>>> = Mutex::new(BTreeMap::new());

    /// Fold one answer into the run's shapes, under the command that gave it.
    pub(super) fn record(args: &[&str], answer: &serde_json::Value) {
        if answer.is_null() {
            return;
        }
        let command = args
            .iter()
            .find(|arg| arg.starts_with("--") && **arg != "--json")
            .copied()
            .unwrap_or("QUERY");
        let mut seen = SEEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        walk(answer, format!("{command} $"), &mut seen);
    }

    fn walk(
        value: &serde_json::Value,
        path: String,
        seen: &mut BTreeMap<String, BTreeSet<&'static str>>,
    ) {
        use serde_json::Value;
        let kind = match value {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Number(n) if n.is_f64() => "float",
            Value::Number(_) => "int",
            Value::String(_) => "string",
            Value::Array(items) => {
                for item in items {
                    walk(item, format!("{path}[]"), seen);
                }
                "array"
            }
            Value::Object(fields) => {
                for (key, field) in fields {
                    walk(field, format!("{path}.{key}"), seen);
                }
                "object"
            }
        };
        seen.entry(path).or_default().insert(kind);
    }

    /// Compare the run's shapes with the golden, or write it under
    /// `UPDATE_GOLDEN`.
    pub(super) fn check() {
        let seen = SEEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let current: Vec<String> = seen
            .iter()
            .map(|(path, kinds)| {
                format!(
                    "{path}: {}",
                    kinds.iter().copied().collect::<Vec<_>>().join("|")
                )
            })
            .collect();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(GOLDEN);
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            let header = format!(
                "# Every field path of the testbed's --json answers, per command, and the\n\
                 # JSON types seen there. Regenerate: {REGENERATE}\n"
            );
            std::fs::write(&path, header + &current.join("\n") + "\n").expect("golden written");
            return;
        }
        let golden = std::fs::read_to_string(&path).unwrap_or_default();
        let recorded: BTreeSet<&str> = golden.lines().filter(|l| !l.starts_with('#')).collect();
        let now: BTreeSet<&str> = current.iter().map(String::as_str).collect();
        let gone: Vec<&str> = recorded.difference(&now).copied().collect();
        let new: Vec<&str> = now.difference(&recorded).copied().collect();
        assert!(
            gone.is_empty() && new.is_empty(),
            "the --json answers' shapes moved from {GOLDEN}. A field that changed type \
             or vanished breaks a caller that parses it; if that is intended, or the \
             change is only new fields, regenerate with {REGENERATE}\n\n{}",
            gone.iter()
                .map(|l| format!("- {l}"))
                .chain(new.iter().map(|l| format!("+ {l}")))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}

/// `--dead . --json` exactly as printed, with this run's scratch paths named
/// rather than spelled. The store and home sit beside the checkout and share
/// its prefix, so they are named first.
fn dead_snapshot(db: &Path, dir: &Path) -> String {
    let out = trekr_in(dir)
        .args(["--dead", ".", "--json"])
        .env("TREKR_DB", db)
        .output()
        .expect("run trekr");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    let store = db.parent().expect("a store in its own directory");
    for (path, name) in [
        (store, "<store>"),
        (&home_of(dir), "<home>"),
        (dir, "<checkout>"),
    ] {
        for spelled in [fs::canonicalize(path).ok(), Some(path.to_path_buf())]
            .into_iter()
            .flatten()
        {
            text = text.replace(&*spelled.to_string_lossy(), name);
        }
    }
    text
}

/// Every snapshot against `tests/dead.golden`, which pins `--dead`'s output
/// byte for byte, hashed: a refactor of it must leave the golden unchanged,
/// and a change of behaviour names the cases it moved. `UPDATE_GOLDEN=1`
/// rewrites it from this run (with `TESTBED_ONLY`, only those cases).
fn dead_golden(snapshots: &[(String, String)]) -> Vec<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/dead.golden");
    let recorded = fs::read_to_string(&path).unwrap_or_default();
    let mut golden: std::collections::BTreeMap<String, String> = recorded
        .lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split_once(' '))
        .map(|(case, hash)| (case.to_string(), hash.to_string()))
        .collect();
    let hashed = |text: &str| format!("{:016x}", fnv(text.as_bytes()));
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        for (label, text) in snapshots {
            golden.insert(label.clone(), hashed(text));
        }
        let mut out = String::from(
            "# `--dead . --json` on each testbed case that asserts `--dead`, hashed.\n\
             # Regenerate: UPDATE_GOLDEN=1 cargo test --test testbed\n",
        );
        for (label, hash) in &golden {
            out.push_str(&format!("{label} {hash}\n"));
        }
        fs::write(&path, out).expect("write tests/dead.golden");
        return Vec::new();
    }
    snapshots
        .iter()
        .filter(|(label, text)| golden.get(label) != Some(&hashed(text)))
        .map(|(label, text)| {
            format!(
                "{label}: `--dead . --json` moved from tests/dead.golden \
                 (UPDATE_GOLDEN=1 to accept). It printed:\n{text}"
            )
        })
        .collect()
}

/// FNV-1a, as the extraction golden hashes.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
    })
}

/// `key=value`, where the value may itself contain `=` or `:`.
fn pairs(rest: &str) -> Vec<(String, String)> {
    rest.split_whitespace()
        .filter_map(|token| token.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn check_def(
    case: &str,
    line: &str,
    answer: &serde_json::Value,
    code: i32,
    failures: &mut Vec<String>,
) {
    let mut fail = |what: String| failures.push(format!("{case}: {line}\n      {what}"));
    for (key, want) in pairs(line) {
        let got = match key.as_str() {
            "status" => answer["status"].as_str().unwrap_or("<none>").to_string(),
            "owner" => answer["owner"].as_str().unwrap_or("<none>").to_string(),
            "via" => answer["resolved_via"]
                .as_str()
                .unwrap_or("<none>")
                .to_string(),
            "name" => answer["name"].as_str().unwrap_or("<none>").to_string(),
            "kind" => answer["kind"].as_str().unwrap_or("<none>").to_string(),
            "variable" => answer["variable"].as_str().unwrap_or("<none>").to_string(),
            "defined_via" => answer["defined_via"]
                .as_str()
                .unwrap_or("<none>")
                .to_string(),
            "confidence" => answer["confidence"]
                .as_f64()
                .map(|c| format!("{c}"))
                .unwrap_or_default(),
            "exit" => code.to_string(),
            "candidates" => answer["candidates"]
                .as_array()
                .map(|a| a.len())
                .unwrap_or(0)
                .to_string(),
            "candidate1" => answer["candidates"][0]["owner"]
                .as_str()
                .unwrap_or("<none>")
                .to_string(),
            "receiver_type" => answer["receiver_type"]
                .as_str()
                .unwrap_or("<none>")
                .to_string(),
            "reason" => answer["reason"].as_str().unwrap_or("<none>").to_string(),
            "resolves_to" => answer["resolves_to"]
                .as_str()
                .unwrap_or("<none>")
                .to_string(),
            "site" | "signature" => {
                let field = match key.as_str() {
                    "site" => "definition",
                    _ => "signatures",
                };
                let site = &answer[field][0];
                format!(
                    "{}:{}",
                    site["path"].as_str().unwrap_or("<none>"),
                    site["line"]
                )
            }
            other => {
                fail(format!("unknown key `{other}`"));
                continue;
            }
        };
        // Paths are absolute in an answer and relative in an expectation, so a
        // site matches on its tail. A reason is prose, so one word of it is
        // asserted. Everything else is exact.
        let matched = if key == "site" || key == "signature" {
            got.ends_with(&want)
        } else if key == "reason" {
            got.contains(&want)
        } else {
            got == want
        };
        if !matched {
            fail(format!("{key}: expected `{want}`, got `{got}`"));
        }
    }
}

fn check_refs(
    case: &str,
    line: &str,
    (answer, code): &(serde_json::Value, i32),
    failures: &mut Vec<String>,
) {
    for (key, want) in pairs(line) {
        let got = match key.as_str() {
            "exit" => code.to_string(),
            // A name's answer is its mentions, one row each.
            "rows" => answer
                .as_array()
                .map_or_else(|| "<none>".into(), |rows| rows.len().to_string()),
            "status" => answer["status"].as_str().unwrap_or("<none>").to_string(),
            "resolves_to" => answer["resolves_to"]
                .as_str()
                .unwrap_or("<none>")
                .to_string(),
            _ => answer["counts"][&key]
                .as_i64()
                .map(|n| n.to_string())
                .unwrap_or_else(|| "<none>".into()),
        };
        if got != want {
            failures.push(format!(
                "{case}: {line}\n      {key}: expected `{want}`, got `{got}`"
            ));
        }
    }
}

/// `Owner#name=tier` for each candidate asked about; `none` means not reported.
fn check_dead(case: &str, line: &str, answer: &serde_json::Value, failures: &mut Vec<String>) {
    let rows = answer["candidates"].as_array().cloned().unwrap_or_default();
    // Reached only through `super` means reached from somewhere: the tier is
    // not honest without the overrides that reach it.
    for row in &rows {
        let from = row["super_from"].as_array().map_or(0, Vec::len);
        if row["tier"] == "super-only" && from == 0 {
            failures.push(format!(
                "{case}: {line}\n      {}#{} is super-only with no super_from",
                row["owner"], row["name"]
            ));
        }
        // Likewise an override names what it overrides.
        let overrides = row["overrides"].as_array().map_or(0, Vec::len);
        if row["tier"] == "override" && overrides == 0 {
            failures.push(format!(
                "{case}: {line}\n      {}#{} is an override of nothing",
                row["owner"], row["name"]
            ));
        }
    }
    // A writer's name ends in `=` (`Widget#mode==unreferenced`): the tier is
    // after the last one.
    let methods = line
        .split_whitespace()
        .filter_map(|token| token.rsplit_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()));
    for (method, want) in methods {
        let (owner, name) = method.rsplit_once('#').unwrap_or(("", &method));
        // `tier~word`: the row's caveat must also contain `word`, `tier~` that
        // it has none, `tier!~word` that it does not contain `word`.
        let (want, caveat, absent) = match want.split_once('~') {
            Some((tier, word)) => match tier.strip_suffix('!') {
                Some(tier) => (tier.to_string(), Some(word.to_string()), true),
                None => (tier.to_string(), Some(word.to_string()), false),
            },
            None => (want.clone(), None, false),
        };
        // `@LINE` is the example group member written at that line, which no
        // owner names (DEC-490).
        let at_line = method.strip_prefix('@').and_then(|n| n.parse::<u64>().ok());
        // A name with no `#` is a class, module or constant, by its whole
        // name (`Admin::Widget=unreferenced`, `on_load(:x)=none`).
        let constant = !method.contains('#') && at_line.is_none();
        let row = rows.iter().find(|row| {
            if let Some(line) = at_line {
                row["line"] == line && row.get("group").is_some()
            } else if constant {
                let whole = match row["owner"].as_str().unwrap_or_default() {
                    "" => row["name"].as_str().unwrap_or_default().to_string(),
                    owner => format!("{owner}::{}", row["name"].as_str().unwrap_or_default()),
                };
                row["kind"] != "method" && whole == method
            } else {
                row["owner"] == owner && row["name"] == name && row["kind"] == "method"
            }
        });
        let got = row.and_then(|row| row["tier"].as_str()).unwrap_or("none");
        if got != want {
            failures.push(format!(
                "{case}: {line}\n      {method}: expected `{want}`, got `{got}`"
            ));
        }
        let said = row.and_then(|row| row["caveat"].as_str()).unwrap_or("");
        let fits = match caveat.as_deref() {
            None => true,
            Some("") => said.is_empty(),
            Some(word) => said.contains(word) != absent,
        };
        if !fits {
            failures.push(format!(
                "{case}: {line}\n      {method}: expected a caveat {} `{}`, got `{said}`",
                if absent { "without" } else { "with" },
                caveat.unwrap_or_default()
            ));
        }
    }
}

#[test]
fn every_testbed_case_answers_as_recorded() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/testbed");
    let mut cases: Vec<PathBuf> = fs::read_dir(&root)
        .expect("tests/testbed exists")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        // `TESTBED_ONLY=580` runs the cases whose name starts with it.
        .filter(|p| {
            std::env::var("TESTBED_ONLY").map_or(true, |only| {
                p.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(&only))
            })
        })
        .collect();
    cases.sort();
    assert!(!cases.is_empty(), "no cases in {}", root.display());

    // Each case stages its own checkout, home and database, so they run side
    // by side; a worker takes the next case until none are left.
    let next = std::sync::atomic::AtomicUsize::new(0);
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get());
    let mut outcomes: Vec<(usize, usize, Vec<String>, Option<String>)> =
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..workers.min(cases.len()))
                .map(|_| {
                    scope.spawn(|| {
                        let mut done = Vec::new();
                        loop {
                            let at = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            let Some(case) = cases.get(at) else {
                                return done;
                            };
                            let (checks, failures, dead) = run_case(case);
                            done.push((at, checks, failures, dead));
                        }
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|h| h.join().expect("a worker catches its cases' panics"))
                .collect()
        });
    // Reported in case order, as a serial run would.
    outcomes.sort_by_key(|(at, ..)| *at);
    let checks: usize = outcomes.iter().map(|(_, checks, ..)| checks).sum();
    let dead: Vec<(String, String)> = outcomes
        .iter()
        .filter_map(|(at, .., dead)| Some((label_of(&cases[*at]), dead.clone()?)))
        .collect();
    let mut failures: Vec<String> = outcomes.into_iter().flat_map(|(_, _, f, _)| f).collect();
    failures.extend(dead_golden(&dead));

    assert!(
        failures.is_empty(),
        "{} of {checks} testbed checks failed across {} cases:\n\n  {}\n",
        failures.len(),
        cases.len(),
        failures.join("\n\n  ")
    );
    // A run of some cases sees only some shapes.
    if std::env::var_os("TESTBED_ONLY").is_none() {
        working_tree_shapes();
        shapes::check();
    }
}

/// The `index` object every query that reads edits carries (DEC-035), in each
/// of its forms, for the shape golden: a clean case never has one. An
/// unstaged edit and an untracked file, read; an edit left at the indexed
/// version while another process holds the store (`busy`); and more changes
/// than a query reads (`stale`, `cause`).
fn working_tree_shapes() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/working-tree");
    let (dir, db) = stage(&fixture, "working-tree");
    let queries: [&[&str]; 5] = [
        &["--def", "app.rb:3:5", "--json"],
        &["--refs", "Widget#fresh", "--json"],
        &["--refs", "app.rb:3:5", "--json"],
        &["--refs", "fresh", "--json"],
        &["--dead", ".", "--json"],
    ];
    let ask_all = || {
        for args in queries {
            trekr(&db, &dir, args);
        }
        // `--refs NAME`'s `--json` is a bare array; `--ndjson` says `index`
        // in its closing line.
        let out = trekr_in(&dir)
            .args(["--refs", "fresh", "--ndjson"])
            .env("TREKR_DB", &db)
            .output()
            .expect("run trekr");
        let text = String::from_utf8_lossy(&out.stdout);
        if let Some(last) = text.lines().last() {
            let closing = serde_json::from_str(last).unwrap_or(serde_json::Value::Null);
            shapes::record(&["--refs NAME --ndjson"], &closing);
        }
    };

    let app = dir.join("app.rb");
    let source = fs::read_to_string(&app).unwrap();
    let edited = source.replace("    helper\n", "    helper\n    fresh\n");
    fs::write(
        &app,
        edited.replace("  def helper", "  def fresh\n  end\n\n  def helper"),
    )
    .unwrap();
    fs::write(
        dir.join("extra.rb"),
        "class Extra\n  def run\n    Widget.new.fresh\n  end\nend\n",
    )
    .unwrap();
    ask_all();

    // Changed again while a writer holds the store: left at the indexed
    // version, and said so.
    fs::write(
        &app,
        format!("# moved\n{}", fs::read_to_string(&app).unwrap()),
    )
    .unwrap();
    let held = rusqlite::Connection::open(&db).unwrap();
    held.execute_batch("BEGIN IMMEDIATE").unwrap();
    trekr(&db, &dir, &["--def", "app.rb:4:5", "--json"]);
    drop(held);

    for n in 0..33 {
        fs::write(
            dir.join(format!("more_{n}.rb")),
            format!("class More{n}\nend\n"),
        )
        .unwrap();
    }
    ask_all();
    let _ = fs::remove_dir_all(&dir);
}

fn label_of(case: &Path) -> String {
    case.file_name().unwrap().to_string_lossy().into_owned()
}

/// Stage one case and check each of its expectations: how many it checked,
/// what failed, and its `--dead` snapshot when it asserts `--dead`. A panic —
/// staging that could not index — is the case's failure, not the run's.
fn run_case(case: &Path) -> (usize, Vec<String>, Option<String>) {
    let label = label_of(case);
    let mut checks = 0usize;
    let mut failures: Vec<String> = Vec::new();
    let mut dead = None;
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        check_case(case, &label, &mut checks, &mut failures, &mut dead)
    }));
    if let Err(panic) = ran {
        let why = panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_string()))
            .unwrap_or_default();
        failures.push(format!("{label}: panicked: {why}"));
    }
    (checks, failures, dead)
}

fn check_case(
    case: &Path,
    label: &str,
    checks: &mut usize,
    failures: &mut Vec<String>,
    dead: &mut Option<String>,
) {
    let expectations = fs::read_to_string(case.join("expected"))
        .unwrap_or_else(|_| panic!("{label} has no `expected` file"));
    let (dir, db) = stage(case, label);
    // Every case's status, for the shape golden: no expectation names it.
    trekr(&db, &dir, &["--status", "--json"]);
    // (expectation, position, --def's site) for each call --def placed.
    let mut calls: Vec<(String, String, String)> = Vec::new();
    // (expectation, position, what the editor owes) for each placed --def.
    type Owed = (Vec<String>, Vec<String>);
    let mut placed_defs: Vec<(String, String, Owed)> = Vec::new();
    if expectations.lines().any(|line| line.starts_with("dead ")) {
        *dead = Some(dead_snapshot(&db, &dir));
    }

    for line in expectations.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        *checks += 1;
        let Some((verb, rest)) = line.split_once(char::is_whitespace) else {
            failures.push(format!("{label}: cannot parse `{line}`"));
            continue;
        };
        let target = rest.split_whitespace().next().unwrap_or_default();
        match verb {
            "def" => {
                let (answer, code) = trekr(&db, &dir, &["--def", target, "--json"]);
                check_def(label, line, &answer, code, failures);
                // Go to Definition and Declaration list what --def placed,
                // by one rule: an `.rbi` is a definition only when it is all
                // there is. An editor never snaps (DEC-036), so a snapped
                // answer is owed at the column --def says it answered; and
                // its caret reads a variable just before that column, so
                // there it owes what --def says on the variable.
                let at = match answer["snapped_to"]["col"].as_u64() {
                    Some(col) => {
                        let (file, line, _) = position(target);
                        format!("{file}:{line}:{col}")
                    }
                    None => target.to_string(),
                };
                let read = caret_variable(&db, &dir, &at);
                let owed = read.as_ref().unwrap_or(&answer);
                if matches!(owed["status"].as_str(), Some("resolved" | "ambiguous"))
                    && let Some(definition) = cli_sites(&dir, owed, "definition")
                    && let Some(signatures) = cli_sites(&dir, owed, "signatures")
                    && !definition.is_empty()
                {
                    placed_defs.push((line.to_string(), at, editor_owes(&definition, &signatures)));
                }
                // A call --def places in the checkout: outgoing calls from the
                // method around it must reach the same definition. Not a
                // symbol (`:save`, `&:name`): it is handed to a call, not made
                // by the method, and outgoing calls leaves it out.
                let placed = &answer["definition"][0];
                // A site's path is relative to its `root`, which may be a gem.
                let absolute = format!(
                    "{}/{}",
                    placed["root"].as_str().unwrap_or_default(),
                    placed["path"].as_str().unwrap_or_default()
                );
                if answer["receiver"].as_str().is_some_and(|r| r != "symbol")
                    && answer["status"] == "resolved"
                    && let Some(path) = in_checkout(&dir, &absolute)
                {
                    calls.push((
                        line.to_string(),
                        target.to_string(),
                        format!("{path}:{}", placed["line"]),
                    ));
                }
            }
            "definition" | "declaration" => {
                let want: Vec<&str> = rest
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .collect();
                let mut lsp = Lsp::start(&db, &dir);
                let got = lsp_locations(&mut lsp, &dir, target, &format!("textDocument/{verb}"));
                if got != want {
                    failures.push(format!("{label}: {line}\n      the editor listed {got:?}"));
                }
            }
            "card" => {
                let (answer, code) = trekr(&db, &dir, &[target, "--json"]);
                check_def(label, line, &answer, code, failures);
            }
            "hover" => {
                let want = rest
                    .split_once(char::is_whitespace)
                    .map(|(_, w)| w.trim())
                    .unwrap_or_default();
                let got = hover_text(&db, &dir, target);
                if !got.contains(want) {
                    failures.push(format!(
                        "{label}: {line}\n      hover said `{}`",
                        got.replace('\n', " ")
                    ));
                }
            }
            "refs" => {
                let answer = trekr(&db, &dir, &["--refs", target, "--json"]);
                check_refs(label, line, &answer, failures);
                // At a position, the editor's Find References lists what the
                // CLI lists: one rule, two fronts. Not at a call nothing
                // places, where the CLI discloses the residue and the editor
                // lists the bare name's sites instead (DEC-563).
                let mut at = target.rsplitn(3, ':');
                let position = at.next().is_some_and(|n| n.parse::<u32>().is_ok())
                    && at.next().is_some_and(|n| n.parse::<u32>().is_ok())
                    && at.next().is_some();
                if position && answer.0["status"] != "residue" {
                    let editor = lsp_references(&db, &dir, target);
                    let cli = cli_references(&answer.0, None);
                    if editor != cli {
                        failures.push(format!(
                            "{label}: {line}\n      the editor listed {editor:?}, the CLI {cli:?}"
                        ));
                    }
                    // Incoming calls are the same rule's confirmed sites,
                    // wherever a method is prepared at the position.
                    if let Some(incoming) = lsp_incoming(&db, &dir, target) {
                        let confirmed = cli_references(&answer.0, Some("confirmed"));
                        if incoming != confirmed {
                            failures.push(format!(
                                "{label}: {line}\n      incoming calls listed {incoming:?}, \
                                 the CLI's confirmed {confirmed:?}"
                            ));
                        }
                    }
                }
            }
            "ancestors" => {
                let (answer, _) = trekr(&db, &dir, &["--ancestors", target, "--json"]);
                let got: Vec<&str> = answer["ancestors"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|n| n.as_str()).collect())
                    .unwrap_or_default();
                let want: Vec<&str> = rest
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .split(',')
                    .collect();
                // A prefix: the tail is core's, and not what a case is about.
                if !got.starts_with(&want) {
                    failures.push(format!(
                        "{label}: {line}\n      expected {want:?} first, got {got:?}"
                    ));
                }
                for assertion in rest.split_whitespace().skip(2) {
                    let Some(listed) = assertion.strip_prefix("unresolved=") else {
                        failures.push(format!("{label}: {line}\n      unknown key {assertion}"));
                        continue;
                    };
                    let want: Vec<&str> = listed.split(',').filter(|n| !n.is_empty()).collect();
                    let got: Vec<&str> = answer["unresolved_ancestors"]
                        .as_array()
                        .map(|a| a.iter().filter_map(|n| n.as_str()).collect())
                        .unwrap_or_default();
                    if got != want {
                        failures.push(format!(
                            "{label}: {line}\n      unresolved: expected {want:?}, got {got:?}"
                        ));
                    }
                }
            }
            "dead" => {
                let (answer, _) = trekr(&db, &dir, &["--dead", target, "--json"]);
                check_dead(label, line, &answer, failures);
            }
            "symbols" => {
                let (answer, _) = trekr(&db, &dir, &["--symbols", target, "--json"]);
                let got: Vec<&str> = answer
                    .as_array()
                    .map(|rows| {
                        rows.iter()
                            .filter_map(|r| r["name"].as_str())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let want: Vec<&str> = rest
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .split(',')
                    .collect();
                if got != want {
                    failures.push(format!(
                        "{label}: {line}\n      expected {want:?}, got {got:?}"
                    ));
                }
                // `private=a,b`: the rows that are private, in source order.
                if let Some(want) = rest
                    .split_whitespace()
                    .find_map(|token| token.strip_prefix("private="))
                {
                    let got: Vec<&str> = answer
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|r| r["visibility"] == "private")
                        .filter_map(|r| r["name"].as_str())
                        .collect();
                    let want: Vec<&str> = want.split(',').filter(|n| !n.is_empty()).collect();
                    if got != want {
                        failures.push(format!(
                            "{label}: {line}\n      private: expected {want:?}, got {got:?}"
                        ));
                    }
                }
            }
            other => failures.push(format!("{label}: unknown verb `{other}`")),
        }
    }
    if !placed_defs.is_empty() {
        let mut lsp = Lsp::start(&db, &dir);
        for (line, target, (defined, declared)) in &placed_defs {
            for (method, owed) in [("definition", defined), ("declaration", declared)] {
                let got = lsp_locations(&mut lsp, &dir, target, &format!("textDocument/{method}"));
                if got != *owed {
                    failures.push(format!(
                        "{label}: {line}\n      the editor's {method} listed {got:?}, \
                         --def owes {owed:?}"
                    ));
                }
            }
        }
    }
    if !calls.is_empty() {
        let mut lsp = Lsp::start(&db, &dir);
        for (line, target, placed) in &calls {
            match lsp_outgoing(&mut lsp, &dir, target) {
                // A call outside any method has no outgoing walk to agree.
                None => {}
                Some(Some(reached)) if reached == *placed => {}
                Some(reached) => failures.push(format!(
                    "{label}: {line}\n      outgoing calls reached {reached:?}, --def {placed}"
                )),
            }
        }
    }
    // A case that passed is done with its scratch now, not at exit; one that
    // failed keeps it to look at.
    if failures.is_empty() {
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(home_of(&dir));
        let _ = fs::remove_dir_all(db.parent().expect("a store in its own directory"));
    }
}
