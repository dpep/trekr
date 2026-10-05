//! End-to-end: a scripted LSP session against the built binary over stdio.
//!
//! Same isolation as the CLI suite — a temp git repo and its own database — so
//! this runs in CI without touching anything real.

#![allow(
    clippy::disallowed_methods,
    reason = "a test reads its own fixtures and scratch files"
)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

mod support;

use support::{fixture_home, git};

/// A scratch repo and database for one test (see `support::scratch`), whose
/// checkout runs on the fixture's Ruby.
fn scratch(label: &str) -> (PathBuf, PathBuf) {
    let (dir, db) = support::scratch(label);
    fs::write(dir.join(".ruby-version"), "9.8.7\n").unwrap();
    (dir, db)
}

/// `program`, run as `support::neutral` says, on the fixture's Ruby.
fn isolated(program: &str) -> Command {
    let mut command = Command::new(program);
    support::neutral(&mut command, fixture_home());
    command
}

/// The usage rows a test's database has counted.
fn usage_rows(db: &Path) -> Vec<serde_json::Value> {
    let out = trekr()
        .args(["--usage", "--json"])
        .env("TREKR_DB", db)
        .output()
        .unwrap();
    serde_json::from_slice::<serde_json::Value>(&out.stdout)
        .expect("--usage --json is an array")
        .as_array()
        .unwrap()
        .clone()
}

/// The summed count of the LSP rows matching every `(field, value)` given.
fn counted(rows: &[serde_json::Value], matching: &[(&str, serde_json::Value)]) -> i64 {
    rows.iter()
        .filter(|r| r["surface"] == "lsp")
        .filter(|r| matching.iter().all(|(k, v)| &r[*k] == v))
        .map(|r| r["count"].as_i64().unwrap())
        .sum()
}

fn trekr() -> Command {
    isolated(env!("CARGO_BIN_EXE_trekr"))
}

/// A repo with a call whose receiver resolves, so definition has a real answer.
fn repo(dir: &Path) -> String {
    let source = concat!(
        "class Widget\n",       // 1
        "  def save\n",         // 2
        "  end\n",              // 3
        "end\n",                // 4
        "class Job\n",          // 5
        "  def run\n",          // 6
        "    w = Widget.new\n", // 7
        "    w.save\n",         // 8
        "  end\n",              // 9
        "end\n",                // 10
    );
    git(dir, &["init", "-q"]);
    fs::write(dir.join("app.rb"), source).unwrap();
    git(dir, &["add", "-A"]);
    git(
        dir,
        &[
            "-c",
            "user.email=t@e.st",
            "-c",
            "user.name=test",
            "commit",
            "-qm",
            "init",
        ],
    );
    source.to_string()
}

/// A live LSP conversation with the server.
struct Session {
    child: Child,
    /// Dropped to close the pipe on shutdown — the server's reader thread
    /// blocks on stdin until it does, so waiting without closing hangs.
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next_id: i64,
}

impl Session {
    fn start(db: &Path, dir: &Path) -> Session {
        Session::start_with(db, dir, &[])
    }

    fn start_with(db: &Path, dir: &Path, env: &[(&str, &str)]) -> Session {
        let mut child = trekr()
            .envs(env.iter().copied())
            .arg("--lsp")
            .current_dir(dir)
            .env("TREKR_DB", db)
            // Its own log, or every test in this file appends to one shared
            // file beside the temp dir and they read each other's lines.
            .env("TREKR_LOG", log_path(db))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start trekr --lsp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Session {
            child,
            stdin: Some(stdin),
            stdout,
            next_id: 0,
        }
    }

    fn send(&mut self, message: serde_json::Value) {
        let body = serde_json::to_string(&message).unwrap();
        let stdin = self.stdin.as_mut().expect("session still open");
        write!(stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
        stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params
        }));
        // Notifications (diagnostics) can arrive first; the answer is the
        // message carrying our id.
        loop {
            let message = self.read();
            if message.get("id").and_then(|v| v.as_i64()) == Some(id) {
                return message;
            }
        }
    }

    fn notify(&mut self, method: &str, params: serde_json::Value) {
        self.send(serde_json::json!({
            "jsonrpc": "2.0", "method": method, "params": params
        }));
    }

    fn read(&mut self) -> serde_json::Value {
        let mut length = 0usize;
        loop {
            let mut line = String::new();
            self.stdout
                .read_line(&mut line)
                .expect("server still alive");
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            if let Some(value) = trimmed.strip_prefix("Content-Length: ") {
                length = value.parse().unwrap();
            }
        }
        let mut body = vec![0u8; length];
        std::io::Read::read_exact(&mut self.stdout, &mut body).unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    fn initialize(&mut self, dir: &Path) -> serde_json::Value {
        self.initialize_with(dir, serde_json::json!({}))
    }

    fn initialize_with(
        &mut self,
        dir: &Path,
        capabilities: serde_json::Value,
    ) -> serde_json::Value {
        let uri = format!("file://{}", dir.display());
        let result = self.request(
            "initialize",
            serde_json::json!({
                "processId": null,
                "rootUri": uri,
                "capabilities": capabilities,
            }),
        );
        self.notify("initialized", serde_json::json!({}));
        result
    }

    fn stop(mut self) {
        self.send(serde_json::json!({
            "jsonrpc": "2.0", "id": 9999, "method": "shutdown", "params": null
        }));
        self.notify("exit", serde_json::json!(null));
        // Closing the pipe is what lets the server's reader thread finish.
        self.stdin.take();
        let _ = self.child.wait();
    }
}

/// An outline's names, depth-first — the order a reader scans it in.
fn outline_names(symbols: &serde_json::Value) -> Vec<String> {
    let mut names = Vec::new();
    for symbol in symbols.as_array().expect("an outline, not null") {
        names.push(symbol["name"].as_str().unwrap().to_string());
        if !symbol["children"].is_null() {
            names.extend(outline_names(&symbol["children"]));
        }
    }
    names
}

fn uri_of(dir: &Path, name: &str) -> String {
    format!("file://{}/{}", dir.display(), name)
}

fn log_path(db: &Path) -> PathBuf {
    db.with_extension("log")
}

/// Every line the server logged, as parsed ndjson.
fn log_lines(db: &Path) -> Vec<serde_json::Value> {
    fs::read_to_string(log_path(db))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).expect("each line is one JSON object"))
        .collect()
}

#[test]
fn the_server_announces_only_what_it_answers() {
    let (dir, db) = scratch("caps");
    repo(&dir);
    let mut session = Session::start(&db, &dir);
    let result = session.initialize(&dir);
    let caps = &result["result"]["capabilities"];

    for provider in [
        "definitionProvider",
        "referencesProvider",
        "documentSymbolProvider",
        "workspaceSymbolProvider",
        "hoverProvider",
        "implementationProvider",
        "callHierarchyProvider",
        "documentLinkProvider",
        "documentHighlightProvider",
    ] {
        assert!(!caps[provider].is_null(), "{provider} is announced");
    }
    // Never: these are not what an agent uses, and claiming them would invite
    // an editor to route work here that this engine has no business doing.
    assert_eq!(
        caps["completionProvider"]["triggerCharacters"],
        serde_json::json!([".", ":"]),
        "completion, since DEC-040"
    );
    for absent in [
        "renameProvider",
        "documentFormattingProvider",
        "semanticTokensProvider",
    ] {
        assert!(caps[absent].is_null(), "{absent} must not be announced");
    }
    // What a client checks before it sends a template: an older trekr reads
    // a `.erb` as Ruby, and would flood it with syntax errors.
    assert_eq!(
        caps["experimental"]["trekr"]["templates"],
        serde_json::json!(["erb", "rabl"])
    );
    assert_eq!(result["result"]["serverInfo"]["name"], "trekr");
    assert_eq!(
        result["result"]["serverInfo"]["version"],
        env!("CARGO_PKG_VERSION")
    );
    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn go_to_definition_answers_from_the_resolved_receiver() {
    let (dir, db) = scratch("def");
    let source = repo(&dir);
    // The index has to exist; the server reads it, it does not build it.
    let indexed = trekr()
        .args(["--index"])
        .current_dir(&dir)
        .env("TREKR_DB", &db)
        .output()
        .unwrap();
    assert!(indexed.status.success());

    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);
    session.notify(
        "textDocument/didOpen",
        serde_json::json!({"textDocument": {
            "uri": uri_of(&dir, "app.rb"), "languageId": "ruby", "version": 1, "text": source
        }}),
    );

    // `w.save` on line 8, column 7 (0-based) — `w` resolves to Widget.
    let answer = session.request(
        "textDocument/definition",
        serde_json::json!({
            "textDocument": {"uri": uri_of(&dir, "app.rb")},
            "position": {"line": 7, "character": 6},
        }),
    );
    let locations = answer["result"].as_array().expect("an array of locations");
    assert_eq!(locations.len(), 1);
    assert_eq!(
        locations[0]["range"]["start"]["line"], 1,
        "Widget#save is defined on line 2, which is line 1 zero-based"
    );

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_click_that_finds_nothing_is_logged_where_usage_misses_reads_it() {
    let (dir, db) = scratch("misses");
    let source = repo(&dir);
    let indexed = trekr()
        .args(["--index"])
        .current_dir(&dir)
        .env("TREKR_DB", &db)
        .output()
        .unwrap();
    assert!(indexed.status.success());

    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);
    session.notify(
        "textDocument/didOpen",
        serde_json::json!({"textDocument": {
            "uri": uri_of(&dir, "app.rb"), "languageId": "ruby", "version": 1, "text": source
        }}),
    );
    let at = |line: u32, character: u32| {
        serde_json::json!({
            "textDocument": {"uri": uri_of(&dir, "app.rb")},
            "position": {"line": line, "character": character},
        })
    };
    // `w.save` resolves: not a miss. The `end` on line 9 names nothing.
    session.request("textDocument/definition", at(7, 6));
    session.request("textDocument/definition", at(8, 3));
    session.request("textDocument/hover", at(8, 3));
    session.stop();

    let out = trekr()
        .args(["--usage", "--misses", "--json"])
        .env("TREKR_DB", &db)
        .env("TREKR_LOG", log_path(&db))
        .output()
        .unwrap();
    assert!(out.status.success(), "misses were found");
    let misses: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).unwrap();
    let ops: Vec<&str> = misses.iter().map(|m| m["op"].as_str().unwrap()).collect();
    assert_eq!(ops, ["definition", "hover"], "the hit is not a miss");
    let miss = &misses[0];
    assert_eq!(
        (
            miss["line"].as_u64(),
            miss["col"].as_u64(),
            miss["token"].as_str()
        ),
        (Some(9), Some(4), Some("end")),
        "1-based, as --def takes it"
    );
    assert_eq!(miss["outcome"], "empty");
    assert!(miss["file"].as_str().unwrap().ends_with("app.rb"));
    assert!(miss["why"].as_str().is_some(), "the engine says why");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn references_narrow_to_the_method_asked_about_not_the_name() {
    let (dir, db) = scratch("refs");
    git(&dir, &["init", "-q"]);
    // Two classes with a `save`, and one call site of each.
    let source = concat!(
        "class Widget\n",       // 1
        "  def save\n",         // 2
        "  end\n",              // 3
        "end\n",                // 4
        "class Gadget\n",       // 5
        "  def save\n",         // 6
        "  end\n",              // 7
        "end\n",                // 8
        "class Job\n",          // 9
        "  def run\n",          // 10
        "    w = Widget.new\n", // 11
        "    w.save\n",         // 12
        "    g = Gadget.new\n", // 13
        "    g.save\n",         // 14
        "  end\n",              // 15
        "end\n",                // 16
    );
    fs::write(dir.join("app.rb"), source).unwrap();
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
            "init",
        ],
    );
    trekr()
        .args(["--index"])
        .current_dir(&dir)
        .env("TREKR_DB", &db)
        .output()
        .unwrap();

    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);
    session.notify(
        "textDocument/didOpen",
        serde_json::json!({"textDocument": {
            "uri": uri_of(&dir, "app.rb"), "languageId": "ruby", "version": 1, "text": source
        }}),
    );

    // Standing on `Gadget#save` (line 6) must not return Widget's call site.
    let answer = session.request(
        "textDocument/references",
        serde_json::json!({
            "textDocument": {"uri": uri_of(&dir, "app.rb")},
            "position": {"line": 5, "character": 6},
            "context": {"includeDeclaration": false},
        }),
    );
    let lines: Vec<u64> = answer["result"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["range"]["start"]["line"].as_u64().unwrap() + 1)
        .collect();
    assert_eq!(
        lines,
        vec![14],
        "only Gadget's call site — Widget's resolves elsewhere and is excluded, \
         where a bare-name answer would have returned both"
    );

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// Find References on `def initialize` lists the `X.new`s that run it, each
/// range on the `new` it writes, and none of a class with its own (DEC-541).
#[test]
fn references_on_initialize_are_the_news_that_run_it() {
    let (dir, db) = scratch("refs-initialize");
    git(&dir, &["init", "-q"]);
    let source = concat!(
        "class Widget\n",          // 1
        "  def initialize(a)\n",   // 2
        "  end\n",                 // 3
        "end\n",                   // 4
        "class Gadget\n",          // 5
        "  def initialize\n",      // 6
        "  end\n",                 // 7
        "end\n",                   // 8
        "Widget.new(1)\n",         // 9
        "Gadget.new\n",            // 10
        "x = Widget.new(2).dup\n", // 11
    );
    fs::write(dir.join("app.rb"), source).unwrap();
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
            "init",
        ],
    );
    trekr()
        .args(["--index"])
        .current_dir(&dir)
        .env("TREKR_DB", &db)
        .output()
        .unwrap();

    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);
    session.notify(
        "textDocument/didOpen",
        serde_json::json!({"textDocument": {
            "uri": uri_of(&dir, "app.rb"), "languageId": "ruby", "version": 1, "text": source
        }}),
    );
    let answer = session.request(
        "textDocument/references",
        serde_json::json!({
            "textDocument": {"uri": uri_of(&dir, "app.rb")},
            "position": {"line": 1, "character": 7},
            "context": {"includeDeclaration": false},
        }),
    );
    let spans: Vec<(u64, u64, u64)> = answer["result"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| {
            let range = &l["range"];
            (
                range["start"]["line"].as_u64().unwrap() + 1,
                range["start"]["character"].as_u64().unwrap(),
                range["end"]["character"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        spans,
        vec![(9, 7, 10), (11, 11, 14)],
        "Widget's two `new`s, each spanning `new`, and not Gadget's"
    );

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// Each `initializationOptions.unresolved` mode, on a residue whose first
/// candidate is a weak guess (four classes define `run`) and one that is a
/// fair one (one class defines `only`) — DEC-443.
#[test]
fn the_unresolved_setting_decides_which_guesses_reach_the_editor() {
    let source = concat!(
        "class A; def run; end; end\n",  // 1
        "class B; def run; end; end\n",  // 2
        "class C; def run; end; end\n",  // 3
        "class D; def run; end; end\n",  // 4
        "class E; def only; end; end\n", // 5
        "class Caller\n",                // 6
        "  def go(thing)\n",             // 7
        "    thing.run\n",               // 8
        "    thing.only\n",              // 9
        "  end\n",                       // 10
        "end\n",                         // 11
    );
    let (dir, db) = scratch("unresolved-modes");
    git(&dir, &["init", "-q"]);
    fs::write(dir.join("app.rb"), source).unwrap();
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
            "init",
        ],
    );
    trekr()
        .args(["--index"])
        .current_dir(&dir)
        .env("TREKR_DB", &db)
        .output()
        .unwrap();

    let answers = |mode: Option<&str>| -> (usize, usize) {
        let mut session = Session::start(&db, &dir);
        let options = match mode {
            Some(mode) => serde_json::json!({ "index": false, "unresolved": mode }),
            None => serde_json::json!({ "index": false }),
        };
        session.request(
            "initialize",
            serde_json::json!({
                "processId": null,
                "rootUri": format!("file://{}", dir.display()),
                "capabilities": {},
                "initializationOptions": options,
            }),
        );
        session.notify("initialized", serde_json::json!({}));
        session.notify(
            "textDocument/didOpen",
            serde_json::json!({"textDocument": {
                "uri": uri_of(&dir, "app.rb"), "languageId": "ruby", "version": 1, "text": source
            }}),
        );
        let mut count = |line: u64| {
            let answer = session.request(
                "textDocument/definition",
                serde_json::json!({
                    "textDocument": {"uri": uri_of(&dir, "app.rb")},
                    "position": {"line": line, "character": 11},
                }),
            );
            answer["result"].as_array().map_or(0, Vec::len)
        };
        let counts = (count(7), count(8));
        session.stop();
        counts
    };

    assert_eq!(
        answers(None),
        (0, 1),
        "confident by default: the weak guess is held back"
    );
    assert_eq!(answers(Some("confident")), (0, 1));
    assert_eq!(answers(Some("peek")), (4, 1), "every candidate");
    assert_eq!(answers(Some("best")), (1, 1), "the first only");
    assert_eq!(answers(Some("none")), (0, 0), "no guess at all");
    assert_eq!(
        answers(Some("bogus")),
        (0, 1),
        "an unknown mode is the default"
    );
    // ...and is said once, in the log, so a typo is findable.
    let invalid = logged_events(&db, "setting_invalid");
    assert_eq!(invalid.len(), 1, "{invalid:?}");
    assert_eq!(invalid[0]["value"], "bogus");
    // A held-back guess is logged with the reason its mode gives.
    let mut why: Vec<String> = logged_events(&db, "miss")
        .iter()
        .filter_map(|m| m["why"].as_str())
        .filter(|why| why.starts_with("an unresolved call"))
        .map(str::to_string)
        .collect();
    why.sort();
    why.dedup();
    assert_eq!(
        why,
        [
            "an unresolved call whose first guess is below `confident`'s bar",
            "an unresolved call, and `unresolved` is `none`",
        ]
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn definition_on_an_unresolved_receiver_offers_ranked_guesses() {
    let (dir, db) = scratch("guesses");
    git(&dir, &["init", "-q"]);
    // `thing.save` — a call receiver, so untyped. Two classes define `save`;
    // Job inherits from Near, so Near's should rank first.
    let source = concat!(
        "class Near\n",       // 1
        "  def save\n",       // 2
        "  end\n",            // 3
        "end\n",              // 4
        "class Far\n",        // 5
        "  def save\n",       // 6
        "  end\n",            // 7
        "end\n",              // 8
        "class Job < Near\n", // 9
        "  def run\n",        // 10
        "    thing.save\n",   // 11
        "  end\n",            // 12
        "end\n",              // 13
    );
    fs::write(dir.join("app.rb"), source).unwrap();
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
            "init",
        ],
    );
    trekr()
        .args(["--index"])
        .current_dir(&dir)
        .env("TREKR_DB", &db)
        .output()
        .unwrap();

    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);
    session.notify(
        "textDocument/didOpen",
        serde_json::json!({"textDocument": {
            "uri": uri_of(&dir, "app.rb"), "languageId": "ruby", "version": 1, "text": source
        }}),
    );

    let answer = session.request(
        "textDocument/definition",
        serde_json::json!({
            "textDocument": {"uri": uri_of(&dir, "app.rb")},
            "position": {"line": 10, "character": 10},
        }),
    );
    let lines: Vec<u64> = answer["result"]
        .as_array()
        .expect("guesses, not null")
        .iter()
        .map(|l| l["range"]["start"]["line"].as_u64().unwrap() + 1)
        .collect();
    assert_eq!(
        lines,
        vec![2, 6],
        "both candidates, with the enclosing class's ancestor first — order is \
         the disclosure"
    );

    // And hover at the same position must say it was never resolved, so an
    // agent can tell a guess from an answer.
    let hover = session.request(
        "textDocument/hover",
        serde_json::json!({
            "textDocument": {"uri": uri_of(&dir, "app.rb")},
            "position": {"line": 10, "character": 10},
        }),
    );
    let text = hover["result"]["contents"]["value"].as_str().unwrap();
    assert!(
        text.contains("receiver type unknown — 2 possible definitions"),
        "hover says it guessed, in words: {text}"
    );
    assert!(!text.contains("confidence"), "and not as a number: {text}");

    session.stop();
    // A guess is counted as one, in the editor and at the command line alike:
    // `--usage` must not report a residue as a hit, nor as nothing.
    let cli = trekr()
        .args(["--def", "app.rb:11:11"])
        .current_dir(&dir)
        .env("TREKR_DB", &db)
        .output()
        .unwrap();
    assert_eq!(cli.status.code(), Some(1), "residue exits 1");
    let rows = usage_rows(&db);
    assert_eq!(
        counted(
            &rows,
            &[
                ("feature", "definition".into()),
                ("outcome", "uncertain".into())
            ]
        ),
        1,
        "{rows:?}"
    );
    assert!(
        rows.iter()
            .any(|r| r["surface"] == "cli" && r["feature"] == "def" && r["outcome"] == "uncertain"),
        "{rows:?}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_core_method_lands_on_a_readable_stub_rather_than_nothing() {
    let (dir, db) = scratch("core");
    git(&dir, &["init", "-q"]);
    let source = "class W\n  def go\n    puts 1\n  end\nend\n";
    fs::write(dir.join("app.rb"), source).unwrap();
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
            "init",
        ],
    );
    trekr()
        .args(["--index"])
        .current_dir(&dir)
        .env("TREKR_DB", &db)
        .output()
        .unwrap();

    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);
    session.notify(
        "textDocument/didOpen",
        serde_json::json!({"textDocument": {
            "uri": uri_of(&dir, "app.rb"), "languageId": "ruby", "version": 1, "text": source
        }}),
    );
    // `puts` resolves to Kernel, which used to answer nothing because the stub
    // was compiled in and had no file.
    let answer = session.request(
        "textDocument/definition",
        serde_json::json!({
            "textDocument": {"uri": uri_of(&dir, "app.rb")},
            "position": {"line": 2, "character": 4},
        }),
    );
    let locations = answer["result"].as_array().expect("a location, not null");
    let uri = locations[0]["uri"].as_str().unwrap();
    // A file named for the owner, so a peek list says whose method it is.
    assert!(
        uri.contains(".core/rbs-") && uri.ends_with("/Kernel.rb"),
        "lands in Kernel's stub: {uri}"
    );
    let path = uri.strip_prefix("file://").unwrap();
    let line = locations[0]["range"]["start"]["line"].as_u64().unwrap() as usize;
    let text = fs::read_to_string(path).expect("the file is really there");
    assert_eq!(
        text.lines().nth(line).map(str::trim),
        Some("def puts(*objects)"),
        "the line a peek shows is the signature, with no `; end`"
    );

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// The editor's peek list shows a file name and the target's first line, so a
/// core candidate has to say whose it is and read as a signature. And a chain
/// the core stub types resolves instead of offering every owner of the name.
#[test]
fn core_targets_read_as_their_owners_signatures() {
    let (dir, db) = scratch("core-peek");
    git(&dir, &["init", "-q"]);
    let source =
        "class W\n  def go(x)\n    x.downcase\n    x.gsub(/a/, \"\").downcase\n  end\nend\n";
    fs::write(dir.join("app.rb"), source).unwrap();
    commit_all(&dir);
    trekr()
        .args(["--index"])
        .current_dir(&dir)
        .env("TREKR_DB", &db)
        .output()
        .unwrap();

    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);
    session.notify(
        "textDocument/didOpen",
        serde_json::json!({"textDocument": {
            "uri": uri_of(&dir, "app.rb"), "languageId": "ruby", "version": 1, "text": source
        }}),
    );
    // What a peek list shows for each location: the file's name and its line.
    let mut peek = |line: u32, character: u32| -> Vec<(String, String)> {
        let answer = session.request(
            "textDocument/definition",
            serde_json::json!({
                "textDocument": {"uri": uri_of(&dir, "app.rb")},
                "position": {"line": line, "character": character},
            }),
        );
        answer["result"]
            .as_array()
            .expect("locations")
            .iter()
            .map(|location| {
                let path = location["uri"]
                    .as_str()
                    .unwrap()
                    .strip_prefix("file://")
                    .unwrap();
                let at = location["range"]["start"]["line"].as_u64().unwrap() as usize;
                let text = fs::read_to_string(path).unwrap();
                let name = Path::new(path)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .to_string();
                (name, text.lines().nth(at).unwrap().trim().to_string())
            })
            .collect()
    };

    // `x` is untyped, so every owner of `downcase` is offered — each by name.
    let untyped = peek(2, 7);
    assert!(
        untyped.contains(&(
            "String.rb".to_string(),
            "def downcase(*options)".to_string()
        )),
        "{untyped:?}"
    );
    assert!(
        untyped.iter().any(|(file, _)| file == "Symbol.rb"),
        "{untyped:?}"
    );

    // `gsub` returns a String whatever `x` is, so there is one answer.
    assert_eq!(
        peek(3, 23),
        [(
            "String.rb".to_string(),
            "def downcase(*options)".to_string()
        )]
    );
    let hover = hover_at(&mut session, &dir, 4, 23);
    assert!(hover.contains("String#downcase(*options)"), "{hover}");

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// A repo whose definitions carry doc comments, and calls that resolve three
/// ways: exactly, by a naming convention, and not at all.
fn documented_repo(dir: &Path) -> String {
    let source = concat!(
        "# frozen_string_literal: true\n",              // 1
        "\n",                                           // 2
        "class Base\n",                                 // 3
        "end\n",                                        // 4
        "\n",                                           // 5
        "# A thing on a shelf.\n",                      // 6
        "class Widget < Base\n",                        // 7
        "  # Largest size a widget may take.\n",        // 8
        "  LIMIT = 10\n",                               // 9
        "\n",                                           // 10
        "  # Saves the widget.\n",                      // 11
        "  #\n",                                        // 12
        "  # Writes it through to the store.\n",        // 13
        "  # @return [Boolean] whether it saved\n",     // 14
        "  def save(force = false, *rest, key: nil)\n", // 15
        "  end\n",                                      // 16
        "\n",                                           // 17
        "  def plain\n",                                // 18
        "  end\n",                                      // 19
        "end\n",                                        // 20
        "\n",                                           // 21
        "class Gadget\n",                               // 22
        "  def save\n",                                 // 23
        "  end\n",                                      // 24
        "end\n",                                        // 25
        "\n",                                           // 26
        "class Job\n",                                  // 27
        "  def run\n",                                  // 28
        "    w = Widget.new\n",                         // 29
        "    w.save\n",                                 // 30
        "    w.plain\n",                                // 31
        "    @gadget.save\n",                           // 32
        "    Widget::LIMIT\n",                          // 33
        "  end\n",                                      // 34
        "end\n",                                        // 35
    );
    git(dir, &["init", "-q"]);
    fs::write(dir.join("app.rb"), source).unwrap();
    commit_all(dir);
    source.to_string()
}

/// Hover text at a 1-based line and 0-based character of `app.rb`.
fn hover_at(session: &mut Session, dir: &Path, line: u32, character: u32) -> String {
    let answer = session.request(
        "textDocument/hover",
        serde_json::json!({
            "textDocument": {"uri": uri_of(dir, "app.rb")},
            "position": {"line": line - 1, "character": character},
        }),
    );
    answer["result"]["contents"]["value"]
        .as_str()
        .expect("markdown")
        .to_string()
}

/// None of the engine's bookkeeping reaches a reader: no status, no
/// confidence, no rung, and no number standing in for any of them.
fn assert_no_internals(text: &str) {
    for internal in [
        "status",
        "confidence",
        "via `",
        "Resolved",
        "Residue",
        "local:new",
    ] {
        assert!(!text.contains(internal), "leaks `{internal}`: {text}");
    }
    // Links carry paths, and paths carry versions; the prose must not.
    let prose: String = text
        .split("](")
        .map(|part| part.split_once(')').map_or(part, |(_, after)| after))
        .collect();
    let decimal = prose.as_bytes().windows(4).any(|w| {
        w[0].is_ascii_digit() && w[1] == b'.' && w[2].is_ascii_digit() && w[3].is_ascii_digit()
    });
    assert!(!decimal, "no raw number stands in for certainty: {text}");
}

fn documented_session(label: &str) -> (PathBuf, Session) {
    let (dir, db) = scratch(label);
    let source = documented_repo(&dir);
    trekr()
        .args(["--index"])
        .current_dir(&dir)
        .env("TREKR_DB", &db)
        .output()
        .unwrap();
    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);
    session.notify(
        "textDocument/didOpen",
        serde_json::json!({"textDocument": {
            "uri": uri_of(&dir, "app.rb"), "languageId": "ruby", "version": 1, "text": source
        }}),
    );
    (dir, session)
}

#[test]
fn hover_shows_the_signature_as_written_and_the_doc_summary() {
    let (dir, mut session) = documented_session("hover-doc");

    let text = hover_at(&mut session, &dir, 30, 6);
    assert!(
        text.starts_with("```ruby\nWidget#save(force = false, *rest, key: nil)\n```"),
        "the signature as written, owner first: {text}"
    );
    assert!(text.contains("Saves the widget."), "the summary: {text}");
    assert!(
        !text.contains("Writes it through"),
        "only the first paragraph: {text}"
    );
    assert!(
        text.contains("**Returns** `Boolean` — whether it saved"),
        "and what it returns: {text}"
    );
    assert!(
        text.contains("Defined in [`app.rb:15`](file://"),
        "and where, linked: {text}"
    );
    assert!(
        !text.contains("\n\n_"),
        "a certain answer carries no caveat: {text}"
    );
    assert_no_internals(&text);

    // No doc comment: the signature and the location, and nothing invented.
    let text = hover_at(&mut session, &dir, 31, 6);
    assert_eq!(
        text,
        format!(
            "```ruby\nWidget#plain\n```\n\nDefined in [`app.rb:18`](file://{}/app.rb#L18)",
            // As the client spelled its workspace: on macOS the temp dir is
            // behind the `/var` symlink.
            dir.display()
        )
    );

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn hover_on_a_constant_or_class_reads_its_declaration() {
    let (dir, mut session) = documented_session("hover-const");

    let text = hover_at(&mut session, &dir, 33, 13);
    assert!(
        text.starts_with("```ruby\nWidget::LIMIT = 10\n```"),
        "the constant and its value: {text}"
    );
    assert!(text.contains("Largest size a widget may take."), "{text}");

    let text = hover_at(&mut session, &dir, 29, 9);
    assert!(
        text.starts_with("```ruby\nclass Widget < Base\n```"),
        "the class and its parent: {text}"
    );
    assert!(
        text.contains("A thing on a shelf."),
        "the magic comment above it is not its doc, the class comment is: {text}"
    );
    assert!(!text.contains("frozen_string_literal"), "{text}");
    assert_no_internals(&text);

    // On the definition itself, from the buffer: no location, it is here.
    let text = hover_at(&mut session, &dir, 15, 7);
    assert!(text.contains("Widget#save(force"), "{text}");
    assert!(text.contains("Saves the widget."), "{text}");
    assert!(!text.contains("Defined in"), "{text}");

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// `@gadget` names `Gadget` by convention, and `Widget` defines `save` too:
/// the answer is a guess, and the hover says so in words — not a number.
#[test]
fn hover_on_a_guess_says_so_in_plain_words() {
    let (dir, mut session) = documented_session("hover-guess");

    let text = hover_at(&mut session, &dir, 32, 13);
    assert!(text.contains("Gadget#save"), "the pick: {text}");
    assert!(
        text.contains(
            "_Best guess — the receiver's type is inferred, and 1 other definition of `save` exists._"
        ),
        "said plainly: {text}"
    );
    assert_no_internals(&text);

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// Following a definition into a gem lands in a file outside the checkout,
/// and navigation has to keep working from there. The server placed the file
/// in the app that answers for it, then failed to find it under the app's
/// root, and answered nothing at all.
#[test]
fn navigation_keeps_working_inside_a_gem_file() {
    let (dir, db) = scratch("inside-gem");
    let gem_home = PathBuf::from(format!("{}-gems", dir.display()));
    let _ = fs::remove_dir_all(&gem_home);
    let lib = gem_home.join("gems/shelf-1.0.0/lib");
    fs::create_dir_all(&lib).unwrap();
    let gem_source = concat!(
        "module Shelf\n", // 0
        "  class Box\n",  // 1
        "    def fill\n", // 2
        "    end\n",      // 3
        "\n",             // 4
        "    def pack\n", // 5
        "      fill\n",   // 6
        "    end\n",      // 7
        "  end\n",        // 8
        "end\n",          // 9
    );
    let gem_file = lib.join("shelf.rb");
    fs::write(&gem_file, gem_source).unwrap();
    git(&dir, &["init", "-q"]);
    fs::write(
        dir.join("Gemfile.lock"),
        "GEM\n  remote: https://rubygems.org/\n  specs:\n    shelf (1.0.0)\n\nDEPENDENCIES\n  shelf\n",
    )
    .unwrap();
    fs::write(
        dir.join("app.rb"),
        "class Job\n  def run\n    Shelf::Box.new.fill\n  end\nend\n",
    )
    .unwrap();
    commit_all(&dir);
    let indexed = trekr()
        .args(["--index"])
        .current_dir(&dir)
        .env("TREKR_DB", &db)
        .env("GEM_HOME", &gem_home)
        .output()
        .unwrap();
    assert!(indexed.status.success());

    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);
    let uri = format!("file://{}", gem_file.display());
    session.notify(
        "textDocument/didOpen",
        serde_json::json!({"textDocument": {
            "uri": uri, "languageId": "ruby", "version": 1, "text": gem_source
        }}),
    );
    let at = |line: u32, character: u32| {
        serde_json::json!({
            "textDocument": {"uri": uri},
            "position": {"line": line, "character": character},
            "context": {"includeDeclaration": false},
        })
    };

    let definition = session.request("textDocument/definition", at(6, 6));
    let locations = definition["result"]
        .as_array()
        .unwrap_or_else(|| panic!("a definition from inside the gem: {definition}"));
    assert_eq!(locations.len(), 1, "{definition}");
    assert!(
        locations[0]["uri"].as_str().unwrap().ends_with("/shelf.rb"),
        "{definition}"
    );
    assert_eq!(locations[0]["range"]["start"]["line"], 2, "{definition}");

    let hover = session.request("textDocument/hover", at(6, 6));
    let text = hover["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_default();
    assert!(text.contains("fill"), "{hover}");

    // The app's callers of the gem's method, the checkout being the evidence.
    let references = session.request("textDocument/references", at(2, 8));
    let found = references["result"]
        .as_array()
        .unwrap_or_else(|| panic!("references from inside the gem: {references}"));
    assert!(
        found.iter().any(|location| {
            location["uri"].as_str().unwrap().ends_with("/app.rb")
                && location["range"]["start"]["line"] == 2
        }),
        "{references}"
    );

    session.stop();
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&gem_home);
}

/// Two apps whose bundles hold the same gem: a gem file opened from one
/// app's workspace is answered from that app. The most recently indexed app
/// answered instead, so references from inside the gem listed the other
/// app's callers, and the session cached the pick.
#[test]
fn a_gem_file_is_answered_from_the_workspaces_own_app() {
    let (mine, db) = scratch("gem-app-b");
    let (other, _) = scratch("gem-app-a");
    let gem_home = PathBuf::from(format!("{}-gems", mine.display()));
    let _ = fs::remove_dir_all(&gem_home);
    let lib = gem_home.join("gems/shelf-1.0.0/lib");
    fs::create_dir_all(&lib).unwrap();
    let gem_source = "module Shelf\n  class Box\n    def fill\n    end\n  end\nend\n";
    let gem_file = lib.join("shelf.rb");
    fs::write(&gem_file, gem_source).unwrap();
    // `other` is indexed last, and sorts first on a tie in the same second.
    for app in [&mine, &other] {
        git(app, &["init", "-q"]);
        fs::write(
            app.join("Gemfile.lock"),
            "GEM\n  remote: https://rubygems.org/\n  specs:\n    shelf (1.0.0)\n\nDEPENDENCIES\n  shelf\n",
        )
        .unwrap();
        fs::write(
            app.join("app.rb"),
            "class Job\n  def run\n    Shelf::Box.new.fill\n  end\nend\n",
        )
        .unwrap();
        commit_all(app);
        let indexed = trekr()
            .args(["--index"])
            .current_dir(app)
            .env("TREKR_DB", &db)
            .env("GEM_HOME", &gem_home)
            .output()
            .unwrap();
        assert!(indexed.status.success());
    }

    let mut session = Session::start(&db, &mine);
    session.initialize(&mine);
    let uri = format!("file://{}", gem_file.display());
    session.notify(
        "textDocument/didOpen",
        serde_json::json!({"textDocument": {
            "uri": uri, "languageId": "ruby", "version": 1, "text": gem_source
        }}),
    );
    let references = session.request(
        "textDocument/references",
        serde_json::json!({
            "textDocument": {"uri": uri},
            "position": {"line": 2, "character": 8},
            "context": {"includeDeclaration": false},
        }),
    );
    let callers: Vec<&str> = references["result"]
        .as_array()
        .unwrap_or_else(|| panic!("references from inside the gem: {references}"))
        .iter()
        .filter_map(|location| location["uri"].as_str())
        .filter(|uri| uri.ends_with("/app.rb"))
        .collect();
    let mine_name = mine.file_name().unwrap().to_str().unwrap();
    assert!(
        !callers.is_empty() && callers.iter().all(|uri| uri.contains(mine_name)),
        "{references}"
    );

    session.stop();
    for dir in [&mine, &other, &gem_home] {
        let _ = fs::remove_dir_all(dir);
    }
}

/// Bundler's checkout of a git gem is a clone, `.git` and all, and git's
/// toplevel for a file in it — the clone, never indexed — answered instead of
/// the app (DEC-150).
#[test]
fn a_git_gem_file_is_answered_from_the_app() {
    let (app, db) = scratch("gitgem-app");
    let gem_home = PathBuf::from(format!("{}-gems", app.display()));
    let _ = fs::remove_dir_all(&gem_home);
    let checkout = gem_home.join("bundler/gems/shelf-abc123def456");
    let lib = checkout.join("shelf/lib");
    fs::create_dir_all(&lib).unwrap();
    fs::write(checkout.join("shelf/shelf.gemspec"), "").unwrap();
    let gem_source = "module Shelf\n  class Box\n    def fill\n    end\n  end\nend\n";
    let gem_file = lib.join("shelf.rb");
    fs::write(&gem_file, gem_source).unwrap();
    git(&checkout, &["init", "-q"]);

    git(&app, &["init", "-q"]);
    fs::write(
        app.join("Gemfile.lock"),
        concat!(
            "GIT\n",
            "  remote: https://github.com/example/shelf.git\n",
            "  revision: abc123def4567890abc123def4567890abc12345\n",
            "  specs:\n",
            "    shelf (1.0.0)\n",
            "\n",
            "DEPENDENCIES\n",
            "  shelf!\n",
        ),
    )
    .unwrap();
    fs::write(
        app.join("app.rb"),
        "class Job\n  def run\n    Shelf::Box.new.fill\n  end\nend\n",
    )
    .unwrap();
    commit_all(&app);
    let indexed = trekr()
        .args(["--index"])
        .current_dir(&app)
        .env("TREKR_DB", &db)
        .env("GEM_HOME", &gem_home)
        .output()
        .unwrap();
    assert!(indexed.status.success());

    let mut session = Session::start(&db, &app);
    session.initialize(&app);
    let uri = format!("file://{}", gem_file.display());
    session.notify(
        "textDocument/didOpen",
        serde_json::json!({"textDocument": {
            "uri": uri, "languageId": "ruby", "version": 1, "text": gem_source
        }}),
    );
    let references = session.request(
        "textDocument/references",
        serde_json::json!({
            "textDocument": {"uri": uri},
            "position": {"line": 2, "character": 8},
            "context": {"includeDeclaration": false},
        }),
    );
    let callers = references["result"]
        .as_array()
        .map(|locations| {
            locations
                .iter()
                .filter(|location| {
                    location["uri"]
                        .as_str()
                        .is_some_and(|u| u.ends_with("/app.rb"))
                })
                .count()
        })
        .unwrap_or_default();
    assert_eq!(
        callers, 1,
        "the app's caller, from inside the gem: {references}"
    );

    session.stop();
    let _ = fs::remove_dir_all(&app);
    let _ = fs::remove_dir_all(&gem_home);
}

/// Gems' own docs are much of the value: hovering a gem method shows what the
/// gem wrote, and says which gem.
#[test]
fn hover_on_a_gem_method_shows_the_gems_doc() {
    let (dir, db) = scratch("hover-gem");
    git(&dir, &["init", "-q"]);
    let gem = dir.join("vendor/bundle/ruby/3.3.0/gems/shelf-1.0.0/lib");
    fs::create_dir_all(&gem).unwrap();
    fs::write(
        gem.join("shelf.rb"),
        concat!(
            "module Shelf\n",
            "  # Stacks +items+ onto the shelf.\n",
            "  # @param items [Array<Item>]\n",
            "  # @return [Array<Item>] what is on the shelf now\n",
            "  def self.stack(*items)\n",
            "  end\n",
            "end\n",
        ),
    )
    .unwrap();
    fs::write(dir.join(".gitignore"), "vendor/\n").unwrap();
    fs::write(
        dir.join("Gemfile.lock"),
        concat!(
            "GEM\n",
            "  remote: https://rubygems.org/\n",
            "  specs:\n",
            "    shelf (1.0.0)\n",
            "\n",
            "DEPENDENCIES\n",
            "  shelf\n",
        ),
    )
    .unwrap();
    let source = "class Job\n  def run\n    Shelf.stack(1)\n  end\nend\n";
    fs::write(dir.join("app.rb"), source).unwrap();
    commit_all(&dir);
    trekr()
        .args(["--index"])
        .current_dir(&dir)
        .env("TREKR_DB", &db)
        .output()
        .unwrap();

    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);
    let text = hover_at(&mut session, &dir, 3, 11);
    assert!(
        text.starts_with("```ruby\nShelf.stack(*items)\n```"),
        "{text}"
    );
    assert!(
        text.contains("Stacks `items` onto the shelf."),
        "the gem's own doc, RDoc markup as Markdown: {text}"
    );
    assert!(
        text.contains("**Returns** `Array<Item>` — what is on the shelf now"),
        "{text}"
    );
    assert!(
        text.contains("[`lib/shelf.rb:5`]") && text.contains("gem `shelf-1.0.0`"),
        "which gem, and where in it: {text}"
    );
    assert_no_internals(&text);

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// A file edited since the index moved its definitions. The doc must follow
/// the definition, or not be shown — never attach to whatever is on the old
/// line now.
#[test]
fn hover_follows_a_definition_that_moved_since_the_index() {
    let (dir, mut session) = documented_session("hover-moved");
    let other = dir.join("other.rb");
    fs::write(
        &other,
        "class Task\n  def go\n    w = Widget.new\n    w.save\n  end\nend\n",
    )
    .unwrap();
    let moved = format!(
        "# Unrelated.\nX = 1\n\n{}",
        fs::read_to_string(dir.join("app.rb")).unwrap()
    );
    // On disk only: the index still has `save` on line 15, and `Gadget` has a
    // `save` of its own to be confused with.
    session.notify(
        "textDocument/didClose",
        serde_json::json!({"textDocument": {"uri": uri_of(&dir, "app.rb")}}),
    );
    fs::write(dir.join("app.rb"), &moved).unwrap();
    session.notify(
        "textDocument/didOpen",
        serde_json::json!({"textDocument": {
            "uri": uri_of(&dir, "other.rb"), "languageId": "ruby", "version": 1,
            "text": fs::read_to_string(&other).unwrap()
        }}),
    );
    let answer = session.request(
        "textDocument/hover",
        serde_json::json!({
            "textDocument": {"uri": uri_of(&dir, "other.rb")},
            "position": {"line": 3, "character": 6},
        }),
    );
    let text = answer["result"]["contents"]["value"].as_str().unwrap();
    assert!(text.contains("Widget#save(force"), "{text}");
    assert!(text.contains("Saves the widget."), "{text}");
    assert!(!text.contains("Unrelated"), "{text}");
    assert!(
        text.contains("`app.rb:18`"),
        "and the line it is on now, not the indexed one: {text}"
    );

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// The doc arrives when an item is chosen, not with the list: a list can be
/// hundreds of items, and reading a file for each would stall every keystroke.
#[test]
fn completion_resolves_a_chosen_items_doc() {
    let (dir, mut session) = documented_session("complete-doc");
    let edited = "class Job\n  def run\n    w = Widget.new\n    w.sa\n  end\nend\n";
    fs::write(dir.join("job.rb"), edited).unwrap();
    session.notify(
        "textDocument/didOpen",
        serde_json::json!({"textDocument": {
            "uri": uri_of(&dir, "job.rb"), "languageId": "ruby", "version": 1, "text": edited
        }}),
    );
    let list = listed(
        &mut session,
        serde_json::json!({
            "textDocument": {"uri": uri_of(&dir, "job.rb")},
            "position": {"line": 3, "character": 8},
        }),
    );
    let items = list["result"]["items"].as_array().expect("a list");
    let save = items
        .iter()
        .find(|item| item["label"] == "save")
        .expect("save offered")
        .clone();
    assert!(
        save.get("documentation").is_none(),
        "not read up front: {save}"
    );

    let resolved = session.request("completionItem/resolve", save);
    let item = &resolved["result"];
    assert_eq!(
        item["detail"], "Widget#save(force = false, *rest, key: nil)",
        "{item}"
    );
    let doc = item["documentation"]["value"].as_str().expect("markdown");
    assert!(doc.contains("Saves the widget."), "{doc}");
    assert!(doc.contains("Defined in [`app.rb:15`]"), "{doc}");

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_syntax_error_is_published_as_a_diagnostic_and_cleared_when_fixed() {
    let (dir, db) = scratch("diag");
    repo(&dir);
    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);

    session.notify(
        "textDocument/didOpen",
        serde_json::json!({"textDocument": {
            "uri": uri_of(&dir, "app.rb"), "languageId": "ruby", "version": 1,
            "text": "class Widget\n  def broken(\nend\n"
        }}),
    );
    let published = session.read();
    assert_eq!(published["method"], "textDocument/publishDiagnostics");
    let diagnostics = published["params"]["diagnostics"].as_array().unwrap();
    assert!(!diagnostics.is_empty(), "a truncated def is a syntax error");
    assert_eq!(diagnostics[0]["source"], "trekr");

    // Fixing it must clear them, or the gutter lies.
    session.notify(
        "textDocument/didChange",
        serde_json::json!({
            "textDocument": {"uri": uri_of(&dir, "app.rb"), "version": 2},
            "contentChanges": [{"text": "class Widget\nend\n"}],
        }),
    );
    let cleared = session.read();
    assert!(
        cleared["params"]["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// The log has to record an *empty* answer as plainly as a full one — the
/// defect it was written for was nine operations all returning nothing, with
/// no way to tell whether the requests even arrived.
#[test]
fn the_log_records_each_request_and_how_much_came_back() {
    let (dir, db) = scratch("log");
    repo(&dir);
    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);
    session.notify(
        "textDocument/didOpen",
        serde_json::json!({"textDocument": {
            "uri": uri_of(&dir, "app.rb"), "languageId": "ruby", "version": 1,
            "text": "class Fresh\n  def added\n  end\nend\n"
        }}),
    );
    session.request(
        "textDocument/documentSymbol",
        serde_json::json!({"textDocument": {"uri": uri_of(&dir, "app.rb")}}),
    );
    // A file this workspace has no business answering for.
    session.request(
        "textDocument/documentSymbol",
        serde_json::json!({"textDocument": {"uri": "file:///nowhere/absent.rb"}}),
    );
    session.stop();

    let lines = log_lines(&db);
    let initialize = lines
        .iter()
        .find(|l| l["event"] == "initialize")
        .expect("the client's root is recorded");
    assert_eq!(
        initialize["root"].as_str(),
        dir.canonicalize().unwrap().to_str()
    );

    let requests: Vec<&serde_json::Value> = lines
        .iter()
        .filter(|l| l["event"] == "request" && l["op"] == "textDocument/documentSymbol")
        .collect();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["status"], "ok");
    assert_eq!(
        requests[0]["answered"], 1,
        "Fresh, with added nested inside"
    );
    assert_eq!(
        requests[1]["answered"], 0,
        "an empty answer is logged as one"
    );
    assert!(requests[0]["ms"].is_number(), "and how long it took");
    assert!(
        lines.iter().all(|l| l["event"] != "request_params"),
        "wire-level params stay behind --profile"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// One panicking request costs that request, not the session: a dead server
/// is restarted a few times by the client, then left dead.
#[cfg(debug_assertions)]
#[test]
fn a_handler_that_panics_answers_an_error_and_the_session_survives() {
    let (dir, db) = scratch("panic");
    repo(&dir);
    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);

    // In the handler, and before it, reading what the request asks about.
    for method in ["trekr/panic", "trekr/panic-before"] {
        let answer = session.request(method, serde_json::json!({}));
        assert_eq!(answer["error"]["code"], -32603, "an internal error");
        assert!(
            answer["error"]["message"]
                .as_str()
                .is_some_and(|m| m.contains("panicked")),
            "{answer}"
        );
    }

    let answer = session.request(
        "textDocument/documentSymbol",
        serde_json::json!({"textDocument": {"uri": uri_of(&dir, "app.rb")}}),
    );
    assert_eq!(outline_names(&answer["result"])[0], "Widget");
    session.stop();

    let logged = log_lines(&db)
        .into_iter()
        .find(|l| l["event"] == "request" && l["op"] == "trekr/panic")
        .expect("the panic is logged");
    assert_eq!(logged["status"], "error");

    let _ = fs::remove_dir_all(&dir);
}

/// A store the server cannot open ends it on the store's exit code, as on
/// the command line — not 70, which says trekr has a bug.
#[test]
fn a_store_the_server_cannot_open_exits_as_a_store_failure() {
    let (dir, db) = scratch("lsp-unopenable");
    repo(&dir);
    fs::create_dir_all(&db).unwrap();
    let mut session = Session::start(&db, &dir);
    session.send(serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"processId": null, "rootUri": format!("file://{}", dir.display()), "capabilities": {}},
    }));
    session.notify("initialized", serde_json::json!({}));
    session.stdin.take();
    let status = session.child.wait().unwrap();
    assert_eq!(status.code(), Some(74), "{status:?}");
    let _ = fs::remove_dir_all(&dir);
}

/// What a client sends is not trusted to be a file trekr can read: a URI
/// with a malformed escape names nothing, and a device or a pipe is not read
/// at all — `/dev/zero` would never end. Each answers, and the session goes on.
#[test]
fn a_uri_that_names_no_readable_file_answers_and_the_session_survives() {
    let (dir, db) = scratch("bad-uri");
    repo(&dir);
    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);

    let malformed = format!("file://{}/x%a\u{e9}.rb", dir.display());
    let fifo = dir.join("pipe.rb");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    let started = std::time::Instant::now();
    for uri in [
        malformed,
        "file:///dev/zero".to_string(),
        uri_of(&dir, "pipe.rb"),
    ] {
        for method in ["textDocument/definition", "textDocument/hover"] {
            let answer = session.request(
                method,
                serde_json::json!({
                    "textDocument": {"uri": uri},
                    "position": {"line": 0, "character": 0},
                }),
            );
            assert!(answer["result"].is_null(), "{method} {uri}: {answer}");
        }
    }
    assert!(started.elapsed() < std::time::Duration::from_secs(10));

    let answer = session.request(
        "textDocument/documentSymbol",
        serde_json::json!({"textDocument": {"uri": uri_of(&dir, "app.rb")}}),
    );
    assert_eq!(outline_names(&answer["result"])[0], "Widget");
    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn document_symbol_outlines_the_open_buffer_not_the_index() {
    let (dir, db) = scratch("symbols");
    repo(&dir);
    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);

    // Never indexed, and edited since — the answer still has to be right.
    session.notify(
        "textDocument/didOpen",
        serde_json::json!({"textDocument": {
            "uri": uri_of(&dir, "app.rb"), "languageId": "ruby", "version": 1,
            "text": "class Fresh\n  def added\n  end\nend\n"
        }}),
    );
    let answer = session.request(
        "textDocument/documentSymbol",
        serde_json::json!({"textDocument": {"uri": uri_of(&dir, "app.rb")}}),
    );
    let class = &answer["result"][0];
    assert_eq!(class["name"], "Fresh");
    assert_eq!(
        class["children"][0]["name"], "added",
        "the method is nested inside its class"
    );
    assert_eq!(
        class["range"]["end"]["line"], 3,
        "and the class spans its whole body"
    );

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// A commit-and-index helper for a second repo the client never roots at.
fn ruby_repo(dir: &Path, db: &Path, source: &str) {
    git(dir, &["init", "-q"]);
    fs::write(dir.join("app.rb"), source).unwrap();
    git(dir, &["add", "-A"]);
    git(
        dir,
        &[
            "-c",
            "user.email=t@e.st",
            "-c",
            "user.name=test",
            "commit",
            "-qm",
            "init",
        ],
    );
    trekr()
        .args(["--index"])
        .current_dir(dir)
        .env("TREKR_DB", db)
        .output()
        .unwrap();
}

/// The client's root is not the unit; the file's own checkout is.
///
/// Claude Code roots the server at the session's directory, which is routinely
/// another repo — or, as it was when this was found, a Rust one. Every
/// operation returned empty because the file could not be made relative to that
/// root. An agent asks about files across repos constantly, so the file's
/// enclosing checkout is what has to answer (DEC-024).
#[test]
fn a_file_outside_the_clients_root_is_still_answered() {
    let (root, db) = scratch("elsewhere-root");
    // The client's workspace: a repo with no Ruby in it at all.
    git(&root, &["init", "-q"]);
    fs::write(root.join("README.md"), "not ruby\n").unwrap();

    let (other, _) = scratch("elsewhere-code");
    ruby_repo(
        &other,
        &db,
        concat!(
            "class Widget\n",       // 1
            "  def save\n",         // 2
            "  end\n",              // 3
            "end\n",                // 4
            "class Job\n",          // 5
            "  def run\n",          // 6
            "    w = Widget.new\n", // 7
            "    w.save\n",         // 8
            "  end\n",              // 9
            "end\n",                // 10
        ),
    );

    let mut session = Session::start(&db, &root);
    session.initialize(&root);
    let uri = uri_of(&other, "app.rb");

    // No didOpen: an agent points at a path it has never "opened".
    let symbols = session.request(
        "textDocument/documentSymbol",
        serde_json::json!({"textDocument": {"uri": uri}}),
    );
    assert_eq!(
        outline_names(&symbols["result"]),
        ["Widget", "save", "Job", "run"]
    );

    let answer = session.request(
        "textDocument/definition",
        serde_json::json!({
            "textDocument": {"uri": uri},
            "position": {"line": 7, "character": 6},
        }),
    );
    let locations = answer["result"].as_array().expect("a location, not null");
    assert_eq!(locations[0]["range"]["start"]["line"], 1, "Widget#save");
    assert!(
        locations[0]["uri"].as_str().unwrap().ends_with("/app.rb"),
        "and it points into the other repo: {}",
        locations[0]["uri"]
    );
    // Every URI an agent is handed must name a file it can actually open. A
    // site carrying a checkout-relative path once got joined onto whichever
    // repo was being asked about, which fabricated plausible paths to nothing.
    for location in locations {
        let uri = location["uri"].as_str().expect("a uri");
        let path = uri.strip_prefix("file://").expect("a file uri");
        assert!(
            Path::new(path).exists(),
            "returned a path to nothing: {uri}"
        );
    }

    let hover = session.request(
        "textDocument/hover",
        serde_json::json!({
            "textDocument": {"uri": uri},
            "position": {"line": 7, "character": 6},
        }),
    );
    let text = hover["result"]["contents"]["value"]
        .as_str()
        .expect("markdown, not null");
    assert!(text.contains("Widget"), "the receiver still types: {text}");

    session.stop();
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&other);
}

/// Outlining a file needs its bytes and nothing else — no index, and not even
/// a repository. Requiring one was why `documentSymbol` answered nothing, and
/// that operation needs no resolution at all.
#[test]
fn an_outline_needs_no_index_and_no_repository() {
    let (root, db) = scratch("outline-root");
    git(&root, &["init", "-q"]);
    let loose = support::fresh("loose");
    fs::write(loose.join("app.rb"), "module Loose\n  def go\n  end\nend\n").unwrap();

    let mut session = Session::start(&db, &root);
    session.initialize(&root);
    let answer = session.request(
        "textDocument/documentSymbol",
        serde_json::json!({"textDocument": {"uri": uri_of(&loose, "app.rb")}}),
    );
    assert_eq!(outline_names(&answer["result"]), ["Loose", "go"]);

    session.stop();
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&loose);
}

/// `workspaceSymbol` is the one operation with no file to key on. A client
/// whose root this engine has never indexed used to get nothing; widening to
/// every checkout is the only answer that is of any use to an agent.
#[test]
fn workspace_symbol_widens_when_the_clients_root_is_not_a_checkout() {
    let (root, db) = scratch("wsym-root");
    git(&root, &["init", "-q"]);
    let (other, _) = scratch("wsym-code");
    ruby_repo(&other, &db, "class Sprocket\nend\n");

    let mut session = Session::start(&db, &root);
    session.initialize(&root);
    let answer = session.request("workspace/symbol", serde_json::json!({"query": "Sprocket"}));
    let names: Vec<&str> = answer["result"]
        .as_array()
        .expect("symbols, not null")
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Sprocket"]);

    session.stop();
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&other);
}

/// A bundle moving to another gem version changes no Ruby file in the
/// checkout — only its lockfile — so a session keyed on the checkout's own
/// files went on answering from the old version's tree.
#[test]
fn a_bundle_moving_to_another_gem_version_is_not_served_stale() {
    let (dir, db) = scratch("gem-moved");
    git(&dir, &["init", "-q"]);
    for (version, class) in [("1.0.0", "Old"), ("2.0.0", "New")] {
        let lib = dir.join(format!("vendor/bundle/ruby/3.3.0/gems/shelf-{version}/lib"));
        fs::create_dir_all(&lib).unwrap();
        fs::write(
            lib.join("shelf.rb"),
            format!("module Shelf\n  class {class}\n  end\nend\n"),
        )
        .unwrap();
    }
    fs::write(dir.join(".gitignore"), "vendor/\n").unwrap();
    let lock = |version: &str| {
        fs::write(
            dir.join("Gemfile.lock"),
            format!("GEM\n  remote: https://rubygems.org/\n  specs:\n    shelf ({version})\n\nDEPENDENCIES\n  shelf\n"),
        )
        .unwrap();
    };
    lock("1.0.0");
    fs::write(
        dir.join("app.rb"),
        "class Job\n  def run\n    Shelf::New\n  end\nend\n",
    )
    .unwrap();
    commit_all(&dir);
    let index = || {
        trekr()
            .args(["--index"])
            .current_dir(&dir)
            .env("TREKR_DB", &db)
            .output()
            .unwrap();
    };
    index();

    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);
    let ask = |session: &mut Session| {
        session.request(
            "textDocument/definition",
            serde_json::json!({
                "textDocument": {"uri": uri_of(&dir, "app.rb")},
                "position": {"line": 2, "character": 12},
            }),
        )["result"]
            .clone()
    };
    assert!(ask(&mut session).is_null(), "1.0.0 has no Shelf::New");

    lock("2.0.0");
    index();
    let found = ask(&mut session);
    let found = found
        .as_array()
        .expect("the session must see the new version");
    assert!(
        found[0]["uri"].as_str().unwrap().contains("shelf-2.0.0"),
        "{found:?}"
    );

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// A resident session must notice an edit that reindexed underneath it.
///
/// The rebuild key was (schema version, file count), and *editing* a file moves
/// neither — so the session went on answering from a tree assembled before the
/// edit. Adding a file happened to work, which is what hid this.
#[test]
fn an_edit_reindexed_underneath_the_session_is_not_served_stale() {
    let (dir, db) = scratch("stale");
    git(&dir, &["init", "-q"]);
    fs::write(dir.join("app.rb"), "class Widget\nend\n").unwrap();
    let caller = "class Job\n  def run\n    Gadget\n  end\nend\n";
    fs::write(dir.join("other.rb"), caller).unwrap();
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
            "init",
        ],
    );
    let index = || {
        trekr()
            .args(["--index"])
            .current_dir(&dir)
            .env("TREKR_DB", &db)
            .output()
            .unwrap();
    };
    index();

    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);
    let ask = |session: &mut Session| {
        session.request(
            "textDocument/definition",
            serde_json::json!({
                "textDocument": {"uri": uri_of(&dir, "other.rb")},
                "position": {"line": 2, "character": 4},
            }),
        )["result"]
            .clone()
    };
    assert!(ask(&mut session).is_null(), "Gadget does not exist yet");

    // Edit an existing file — the file *count* is unchanged, which is the case
    // the old key could not see.
    fs::write(dir.join("app.rb"), "class Widget\nend\nclass Gadget\nend\n").unwrap();
    index();

    let locations = ask(&mut session);
    let locations = locations
        .as_array()
        .expect("the session must see the reindexed definition");
    assert_eq!(locations[0]["range"]["start"]["line"], 2, "Gadget, line 3");

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// `goToImplementation` answers two different questions and only ever answered
/// one: on a class it means "who mixes this in", on a **method** it means "who
/// overrides this". Standing on an abstract method returned nothing.
#[test]
fn implementation_on_an_abstract_method_finds_its_overrides() {
    let (dir, db) = scratch("impl");
    git(&dir, &["init", "-q"]);
    let source = concat!(
        "class Adapter\n",            // 1
        "  def write_query?\n",       // 2
        "    raise\n",                // 3
        "  end\n",                    // 4
        "end\n",                      // 5
        "class Sqlite < Adapter\n",   // 6
        "  def write_query?\n",       // 7
        "    true\n",                 // 8
        "  end\n",                    // 9
        "end\n",                      // 10
        "class Postgres < Adapter\n", // 11
        "  def write_query?\n",       // 12
        "    false\n",                // 13
        "  end\n",                    // 14
        "end\n",                      // 15
        "class Unrelated\n",          // 16
        "  def write_query?\n",       // 17
        "  end\n",                    // 18
        "end\n",                      // 19
    );
    fs::write(dir.join("app.rb"), source).unwrap();
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
            "init",
        ],
    );
    trekr()
        .args(["--index"])
        .current_dir(&dir)
        .env("TREKR_DB", &db)
        .output()
        .unwrap();

    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);
    session.notify(
        "textDocument/didOpen",
        serde_json::json!({"textDocument": {
            "uri": uri_of(&dir, "app.rb"), "languageId": "ruby", "version": 1, "text": source
        }}),
    );

    // Standing on the abstract `def write_query?` (line 2, 0-based 1).
    let answer = session.request(
        "textDocument/implementation",
        serde_json::json!({
            "textDocument": {"uri": uri_of(&dir, "app.rb")},
            "position": {"line": 1, "character": 6},
        }),
    );
    let lines: Vec<u64> = answer["result"]
        .as_array()
        .expect("the overrides, not null")
        .iter()
        .map(|l| l["range"]["start"]["line"].as_u64().unwrap() + 1)
        .collect();
    assert_eq!(
        lines,
        vec![7, 12],
        "both subclasses — not the abstract one it is standing on, and not \
         Unrelated, which merely shares the name"
    );

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// The shape a subclass search misses: Rails puts the concrete `write_query?`
/// in a *sibling module* mixed into a class in the same hierarchy, not in a
/// subclass of the abstract module. The owners are unrelated; the classes are
/// not, which is why the question has to be asked of the classes.
#[test]
fn implementation_finds_an_override_that_lives_in_a_sibling_module() {
    let (dir, db) = scratch("implmod");
    git(&dir, &["init", "-q"]);
    let source = concat!(
        "module Statements\n",           // 1
        "  def write_query?\n",          // 2
        "  end\n",                       // 3
        "end\n",                         // 4
        "module Sqlite3Statements\n",    // 5
        "  def write_query?\n",          // 6
        "  end\n",                       // 7
        "end\n",                         // 8
        "class Abstract\n",              // 9
        "  include Statements\n",        // 10
        "end\n",                         // 11
        "class Sqlite < Abstract\n",     // 12
        "  include Sqlite3Statements\n", // 13
        "end\n",                         // 14
    );
    fs::write(dir.join("app.rb"), source).unwrap();
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
            "init",
        ],
    );
    trekr()
        .args(["--index"])
        .current_dir(&dir)
        .env("TREKR_DB", &db)
        .output()
        .unwrap();

    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);
    session.notify(
        "textDocument/didOpen",
        serde_json::json!({"textDocument": {
            "uri": uri_of(&dir, "app.rb"), "languageId": "ruby", "version": 1, "text": source
        }}),
    );
    let answer = session.request(
        "textDocument/implementation",
        serde_json::json!({
            "textDocument": {"uri": uri_of(&dir, "app.rb")},
            "position": {"line": 1, "character": 6},
        }),
    );
    let lines: Vec<u64> = answer["result"]
        .as_array()
        .expect("the sibling module's definition, not null")
        .iter()
        .map(|l| l["range"]["start"]["line"].as_u64().unwrap() + 1)
        .collect();
    assert_eq!(lines, vec![6]);

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// A call tree's rows name the *callers*. Labelling them with the callee's
/// owner made every row identical and told a reader nothing.
#[test]
fn incoming_calls_name_the_method_each_call_sits_in() {
    let (dir, db) = scratch("incoming");
    git(&dir, &["init", "-q"]);
    let source = concat!(
        "class Widget\n",       // 1
        "  def save\n",         // 2
        "  end\n",              // 3
        "end\n",                // 4
        "class Job\n",          // 5
        "  def run\n",          // 6
        "    w = Widget.new\n", // 7
        "    w.save\n",         // 8
        "  end\n",              // 9
        "  def self.sweep\n",   // 10
        "    w = Widget.new\n", // 11
        "    w.save\n",         // 12
        "  end\n",              // 13
        "end\n",                // 14
    );
    fs::write(dir.join("app.rb"), source).unwrap();
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
            "init",
        ],
    );
    trekr()
        .args(["--index"])
        .current_dir(&dir)
        .env("TREKR_DB", &db)
        .output()
        .unwrap();

    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);
    session.notify(
        "textDocument/didOpen",
        serde_json::json!({"textDocument": {
            "uri": uri_of(&dir, "app.rb"), "languageId": "ruby", "version": 1, "text": source
        }}),
    );
    let prepared = session.request(
        "textDocument/prepareCallHierarchy",
        serde_json::json!({
            "textDocument": {"uri": uri_of(&dir, "app.rb")},
            "position": {"line": 1, "character": 6},
        }),
    );
    let item = prepared["result"][0].clone();
    let answer = session.request(
        "callHierarchy/incomingCalls",
        serde_json::json!({ "item": item }),
    );
    let mut names: Vec<&str> = answer["result"]
        .as_array()
        .expect("callers, not null")
        .iter()
        .map(|c| c["from"]["name"].as_str().unwrap())
        .collect();
    names.sort();
    assert_eq!(
        names,
        ["Job#run", "Job.sweep"],
        "each caller named by the method it sits in, singleton marked"
    );

    // And a caller can be expanded in turn: nothing calls `Job#run`, but the
    // question has to be answerable, which it was not when the item was the
    // call site rather than the method.
    let run = answer["result"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["from"]["name"] == "Job#run")
        .unwrap()["from"]
        .clone();
    assert_eq!(
        run["selectionRange"]["start"]["line"], 5,
        "the def, not the call"
    );
    let deeper = session.request(
        "callHierarchy/incomingCalls",
        serde_json::json!({ "item": run }),
    );
    assert!(deeper["result"].is_array(), "answered, not an error");

    // Outgoing from Job#run reaches Widget#save's definition, not the call.
    let outgoing = session.request(
        "callHierarchy/outgoingCalls",
        serde_json::json!({ "item": run }),
    );
    let save = outgoing["result"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["to"]["name"] == "Widget#save")
        .expect("the resolved callee, named by its owner");
    assert_eq!(save["to"]["selectionRange"]["start"]["line"], 1);

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// An `X.new` is a call of the `initialize` it runs, as `--def` and Find
/// References say (DEC-541): preparing it, walking out of a method that
/// makes one, and asking who calls `initialize` all agree.
#[test]
fn call_hierarchy_reads_a_new_as_its_initialize() {
    let (dir, db) = scratch("hierarchy-new");
    git(&dir, &["init", "-q"]);
    let source = concat!(
        "class Widget\n",     // 1
        "  def initialize\n", // 2
        "  end\n",            // 3
        "end\n",              // 4
        "class Job\n",        // 5
        "  def run\n",        // 6
        "    Widget.new\n",   // 7
        "  end\n",            // 8
        "end\n",              // 9
    );
    fs::write(dir.join("app.rb"), source).unwrap();
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
            "init",
        ],
    );
    trekr()
        .args(["--index"])
        .current_dir(&dir)
        .env("TREKR_DB", &db)
        .output()
        .unwrap();

    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);
    let at = |line: u32, character: u32| {
        serde_json::json!({
            "textDocument": {"uri": uri_of(&dir, "app.rb")},
            "position": {"line": line, "character": character},
        })
    };
    let prepared = session.request("textDocument/prepareCallHierarchy", at(6, 11));
    let item = prepared["result"][0].clone();
    assert_eq!(item["name"], "Widget#initialize", "the method `new` runs");
    assert_eq!(item["selectionRange"]["start"]["line"], 1);

    let incoming = session.request(
        "callHierarchy/incomingCalls",
        serde_json::json!({ "item": item }),
    );
    let callers = incoming["result"].as_array().expect("callers, not null");
    assert_eq!(callers.len(), 1, "{callers:?}");
    assert_eq!(callers[0]["from"]["name"], "Job#run");
    assert_eq!(callers[0]["fromRanges"][0]["start"]["line"], 6);

    let run = session.request("textDocument/prepareCallHierarchy", at(5, 6))["result"][0].clone();
    let outgoing = session.request(
        "callHierarchy/outgoingCalls",
        serde_json::json!({ "item": run }),
    );
    let names: Vec<&str> = outgoing["result"]
        .as_array()
        .expect("callees, not null")
        .iter()
        .filter_map(|c| c["to"]["name"].as_str())
        .collect();
    assert!(names.contains(&"Widget#initialize"), "{names:?}");

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// Start a server from `binary` — the test's own copy, at a path the test can
/// then replace, which is the whole subject of the hot-reload tests.
fn start_from(binary: &Path, db: &Path, dir: &Path) -> Session {
    let spawn = || {
        isolated(binary.to_str().unwrap())
            .arg("--lsp")
            .current_dir(dir)
            .env("TREKR_DB", db)
            .env("TREKR_LOG", log_path(db))
            // A loaded machine starts a copied debug binary slowly; these
            // tests are about what its probe answer says, not how soon.
            .env("TREKR_TEST_PROBE_MS", "60000")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
    };
    // Retry ETXTBSY. On Linux, exec refuses a file any process still holds
    // open for writing — and a sibling test spawning at the wrong moment forks
    // while this copy's write descriptor is open, inheriting it. Nothing here
    // can close another thread's fd, but the window is microseconds and the
    // inheriting child is short-lived.
    let mut child = loop {
        match spawn() {
            Ok(child) => break child,
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => panic!("start the copied binary: {e:?}"),
        }
    };
    Session {
        stdin: Some(child.stdin.take().unwrap()),
        stdout: BufReader::new(child.stdout.take().unwrap()),
        child,
        next_id: 0,
    }
}

/// Put `bytes` at `path` as a new file — a new inode, the way an installer
/// renames one into place.
fn install(path: &Path, bytes: &[u8], mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let staged = path.with_extension("staged");
    fs::write(&staged, bytes).unwrap();
    fs::set_permissions(&staged, fs::Permissions::from_mode(mode)).unwrap();
    fs::rename(&staged, path).unwrap();
}

fn the_binary() -> Vec<u8> {
    fs::read(env!("CARGO_BIN_EXE_trekr")).unwrap()
}

/// The database `scratch(label)` hands out, for a helper that kept it.
fn scratch_db(label: &str) -> PathBuf {
    support::root()
        .join(format!("{label}.store"))
        .join("trekr.db")
}

/// The first logged `event`, waiting for it to appear. An idle server looks at
/// its binary every couple of seconds, so this is what "trigger the check" is.
fn logged(db: &Path, event: &str) -> serde_json::Value {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        if let Some(line) = log_lines(db).into_iter().find(|l| l["event"] == event) {
            return line;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no {event} event: {:?}",
            log_lines(db)
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// On disk: a `Widget#save`. In the editor, unsaved: a second method, and a
/// call to `save` — what a reload must not lose.
const SAVED: &str = "class Widget\n  def save\n  end\nend\n";
const UNSAVED: &str = concat!(
    "class Widget\n",       // 1
    "  def save\n",         // 2
    "  end\n",              // 3
    "  def unsaved_edit\n", // 4
    "  end\n",              // 5
    "end\n",                // 6
    "w = Widget.new\n",     // 7
    "w.save\n",             // 8
);

/// A session with `app.rb` open and edited but not saved, served from a copy
/// of the binary at `binary`.
fn edited_session(label: &str, binary: &Path) -> (PathBuf, PathBuf, Session) {
    let (dir, db) = scratch(label);
    ruby_repo(&dir, &db, SAVED);
    let mut session = start_from(binary, &db, &dir);
    session.initialize(&dir);
    session.notify(
        "textDocument/didOpen",
        serde_json::json!({"textDocument": {
            "uri": uri_of(&dir, "app.rb"), "languageId": "ruby", "version": 1, "text": SAVED
        }}),
    );
    session.notify(
        "textDocument/didChange",
        serde_json::json!({
            "textDocument": {"uri": uri_of(&dir, "app.rb"), "version": 2},
            "contentChanges": [{"text": UNSAVED}],
        }),
    );
    assert_the_unsaved_buffer_is_honored(&mut session, &dir);
    (dir, db, session)
}

/// Both answers come from the editor's copy: the outline has the unsaved
/// method, and the only call to `save` exists nowhere but the buffer.
fn assert_the_unsaved_buffer_is_honored(session: &mut Session, dir: &Path) {
    let outline = session.request(
        "textDocument/documentSymbol",
        serde_json::json!({"textDocument": {"uri": uri_of(dir, "app.rb")}}),
    );
    assert!(
        outline_names(&outline["result"]).contains(&"unsaved_edit".to_string()),
        "{outline}"
    );
    assert_eq!(reference_lines(session, dir, 1, 6, false), [8]);
}

fn bin_dir(dir: &Path) -> PathBuf {
    // Outside the repo, so a new file in it is not a change to the checkout.
    let bin = dir.with_extension("bin");
    let _ = fs::remove_dir_all(&bin);
    fs::create_dir_all(&bin).unwrap();
    bin
}

/// Replacing the binary under a running server — `cargo build`, a reinstall —
/// hands the session to the new build in place. Same process, same pipes: the
/// editor keeps its connection, and the new build knows the unsaved buffer
/// without being told again.
#[test]
fn a_replaced_binary_takes_over_the_session_in_place() {
    let (_, scratch_db) = scratch("reload-bin");
    let bin = bin_dir(&scratch_db);
    let binary = bin.join("trekr");
    install(&binary, &the_binary(), 0o755);
    let (dir, db, mut session) = edited_session("reload", &binary);
    let pid = session.child.id();

    install(&binary, &the_binary(), 0o755);
    // Nothing is asked: an idle server notices on its own.
    let reload = logged(&db, "reload");
    assert_eq!(reload["documents"], 1, "the edited buffer went with it");
    let resume = logged(&db, "resume");
    assert_eq!(resume["documents"], 1);

    assert_the_unsaved_buffer_is_honored(&mut session, &dir);
    // Edits keep flowing into the resumed session.
    session.notify(
        "textDocument/didChange",
        serde_json::json!({
            "textDocument": {"uri": uri_of(&dir, "app.rb"), "version": 3},
            "contentChanges": [{"text": format!("{UNSAVED}w.save\n")}],
        }),
    );
    assert_eq!(reference_lines(&mut session, &dir, 1, 6, false), [8, 9]);
    assert_eq!(session.child.id(), pid);
    assert!(
        session.child.try_wait().unwrap().is_none(),
        "one process throughout"
    );
    assert_eq!(
        log_lines(&db)
            .iter()
            .filter(|l| l["event"] == "start")
            .count(),
        1,
        "a resume is not a new session"
    );

    session.stop();
    let rows = usage_rows(&db);
    assert_eq!(
        counted(&rows, &[("feature", "session".into())]),
        1,
        "{rows:?}"
    );
    assert_eq!(counted(&rows, &[("feature", "resume".into())]), 1);
    assert_eq!(counted(&rows, &[("feature", "reload".into())]), 1);
    // The successor starts with nothing warmed, so its first request is a
    // session opener too — and kept out of the warm latencies.
    assert_eq!(
        counted(
            &rows,
            &[("feature", "documentSymbol".into()), ("cold", true.into())]
        ),
        2,
        "{rows:?}"
    );
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&bin);
}

/// `brew upgrade` does not touch the running file: it installs into a new
/// Cellar directory, re-points the symlink the editor launched, and its
/// cleanup removes the old directory. The link is what is watched, so the
/// relink is the upgrade, and the running build's file being gone is no
/// obstacle.
#[test]
fn a_relinked_symlink_is_an_upgrade_too() {
    let (_, scratch_db) = scratch("relink-bin");
    let bin = bin_dir(&scratch_db);
    for version in ["v1", "v2"] {
        fs::create_dir_all(bin.join(version)).unwrap();
        install(&bin.join(version).join("trekr"), &the_binary(), 0o755);
    }
    let link = bin.join("trekr");
    std::os::unix::fs::symlink(bin.join("v1/trekr"), &link).unwrap();
    let (dir, db, mut session) = edited_session("relink", &link);

    fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(bin.join("v2/trekr"), &link).unwrap();
    fs::remove_dir_all(bin.join("v1")).unwrap();
    // Asked straight away, so the question may arrive mid-swap: either build
    // answers it, and neither loses it.
    assert_the_unsaved_buffer_is_honored(&mut session, &dir);
    logged(&db, "resume");
    assert_the_unsaved_buffer_is_honored(&mut session, &dir);

    session.stop();
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&bin);
}

/// A replacement that cannot run is not exec'd into — that would take the
/// connection down with it. The old build keeps serving, and a later fix to
/// the same file is picked up.
#[test]
fn a_binary_that_cannot_run_is_not_reloaded_into() {
    let (_, scratch_db) = scratch("broken-bin");
    let bin = bin_dir(&scratch_db);
    let binary = bin.join("trekr");
    install(&binary, &the_binary(), 0o755);
    let (dir, db, mut session) = edited_session("broken", &binary);

    install(&binary, &the_binary(), 0o644);
    let failed = logged(&db, "reload_failed");
    assert_eq!(failed["retry"], false);
    assert_the_unsaved_buffer_is_honored(&mut session, &dir);
    assert!(!log_lines(&db).iter().any(|l| l["event"] == "resume"));

    // `chmod +x` changes nothing but the mode, and that is enough.
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    logged(&db, "resume");
    assert_the_unsaved_buffer_is_honored(&mut session, &dir);

    session.stop();
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&bin);
}

/// A new build that cannot read this one's handoff — one from before hot
/// reload, or a different handoff format — cannot be resumed into. The server
/// falls back to retiring: it answers what it has, then exits, so a client
/// that restarts servers (VS Code's does) starts the new build afresh.
#[test]
fn a_build_that_cannot_resume_the_session_is_retired_to() {
    let (_, scratch_db) = scratch("retire-bin");
    let bin = bin_dir(&scratch_db);
    let binary = bin.join("trekr");
    install(&binary, &the_binary(), 0o755);
    let (dir, db, mut session) = edited_session("retire", &binary);

    // Runs, and knows its version, but serves the probe as a session.
    install(
        &binary,
        b"#!/bin/sh\n[ \"$1\" = --version ] && echo 'trekr 0.0.1' && exit 0\nexit 1\n",
        0o755,
    );
    // Wait **without** closing stdin: closing it makes any server exit, so a
    // test that closes first cannot tell retiring from ordinary shutdown. An
    // editor holds stdin open, so this is also the real situation.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let status = loop {
        match session.child.try_wait().expect("poll the child") {
            Some(status) => break status,
            None if std::time::Instant::now() > deadline => {
                let _ = session.child.kill();
                panic!("the server never retired");
            }
            None => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    };
    assert!(status.success(), "and cleanly: {status:?}");
    let retire = logged(&db, "retire");
    assert!(
        retire["reason"].as_str().unwrap().contains("predates"),
        "{retire}"
    );
    session.stdin.take();

    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&bin);
}

/// A new build with a new store VERSION drops the index on open (DEC-009).
/// The resumed session then has no index behind it — and must behave exactly
/// like a cold start: answer what it can, index in the background with
/// progress, and answer fully once that lands. Deleting the database stands in
/// for the version bump; both leave the new build an empty store.
#[test]
fn a_resumed_build_with_an_empty_store_indexes_in_the_background() {
    let (_, scratch_db) = scratch("rebuild-bin");
    let bin = bin_dir(&scratch_db);
    let binary = bin.join("trekr");
    install(&binary, &the_binary(), 0o755);
    let (dir, db) = scratch("rebuild");
    fs::write(dir.join("Gemfile"), "source 'https://rubygems.org'\n").unwrap();
    ruby_repo(
        &dir,
        &db,
        "class Widget\n  def save\n  end\nend\nw = Widget.new\nw.save\n",
    );
    let mut session = start_from(&binary, &db, &dir);
    session.initialize_with(
        &dir,
        serde_json::json!({"window": {"workDoneProgress": true}}),
    );
    let uri = uri_of(&dir, "app.rb");
    assert_eq!(
        definition_eventually(&mut session, &uri, 5, 3)[0]["range"]["start"]["line"],
        1
    );

    for suffix in ["", "-wal", "-shm"] {
        let _ = fs::remove_file(format!("{}{suffix}", db.display()));
    }
    install(&binary, &the_binary(), 0o755);
    logged(&db, "resume");

    // The resumed build indexes the checkout it was handed, and says so.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "no progress from the new build"
        );
        let message = session.read();
        if message["method"] == "$/progress" && message["params"]["value"]["kind"] == "end" {
            break;
        }
    }
    let answer = definition_eventually(&mut session, &uri, 5, 3);
    assert_eq!(
        answer[0]["range"]["start"]["line"], 1,
        "Widget#save, from the new index"
    );

    session.stop();
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&bin);
}

/// A client that asks for something this server does not do has to be told
/// *that*, in the protocol's words — not handed a `null` it will read as "no
/// answer here", and not an InternalError that reads as a crash.
#[test]
fn an_unsupported_method_and_a_malformed_request_get_their_own_error_codes() {
    let (dir, db) = scratch("errors");
    repo(&dir);
    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);

    let unknown = session.request("textDocument/formatting", serde_json::json!({}));
    assert_eq!(unknown["error"]["code"], -32601, "MethodNotFound");

    let malformed = session.request("textDocument/definition", serde_json::json!({"bogus": 1}));
    assert_eq!(malformed["error"]["code"], -32602, "InvalidParams");

    // And the server is still there afterwards.
    let fine = session.request(
        "textDocument/documentSymbol",
        serde_json::json!({"textDocument": {"uri": uri_of(&dir, "app.rb")}}),
    );
    assert!(fine["result"].is_array());

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// A cancelled request is answered with RequestCancelled rather than worked.
///
/// The cancellation is sent *first*, which is the one ordering a test can make
/// deterministic: sent after, it races the server picking the request up. The
/// server reads ahead, so a cancellation that arrives while an earlier request
/// is still being answered is seen the same way.
#[test]
fn a_cancelled_request_is_not_answered_with_a_result() {
    let (dir, db) = scratch("cancel");
    repo(&dir);
    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);

    let id = session.next_id + 1;
    session.notify("$/cancelRequest", serde_json::json!({ "id": id }));
    let answer = session.request(
        "textDocument/documentSymbol",
        serde_json::json!({"textDocument": {"uri": uri_of(&dir, "app.rb")}}),
    );
    assert_eq!(answer["error"]["code"], -32800, "RequestCancelled");
    assert!(answer.get("result").is_none());

    // The next request with a fresh id is served normally.
    let next = session.request(
        "textDocument/documentSymbol",
        serde_json::json!({"textDocument": {"uri": uri_of(&dir, "app.rb")}}),
    );
    assert!(next["result"].is_array());

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn closing_a_file_clears_its_diagnostics() {
    let (dir, db) = scratch("close");
    repo(&dir);
    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);
    let uri = uri_of(&dir, "app.rb");
    session.notify(
        "textDocument/didOpen",
        serde_json::json!({"textDocument": {
            "uri": uri, "languageId": "ruby", "version": 3, "text": "def broken(\n"
        }}),
    );
    let published = session.read();
    assert!(
        !published["params"]["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(published["params"]["version"], 3, "tagged with the version");

    session.notify(
        "textDocument/didClose",
        serde_json::json!({"textDocument": {"uri": uri}}),
    );
    let cleared = session.read();
    assert_eq!(cleared["method"], "textDocument/publishDiagnostics");
    assert!(
        cleared["params"]["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// FULL sync is what the server asks for, but a ranged edit must not be
/// mistaken for the whole document.
#[test]
fn a_ranged_change_is_applied_rather_than_replacing_the_document() {
    let (dir, db) = scratch("ranged");
    repo(&dir);
    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);
    let uri = uri_of(&dir, "app.rb");
    session.notify(
        "textDocument/didOpen",
        serde_json::json!({"textDocument": {
            "uri": uri, "languageId": "ruby", "version": 1,
            "text": "class Widget\n  def save\n  end\nend\n"
        }}),
    );
    session.read();
    // Rename `save` to `store` in place.
    session.notify(
        "textDocument/didChange",
        serde_json::json!({
            "textDocument": {"uri": uri, "version": 2},
            "contentChanges": [{
                "range": {"start": {"line": 1, "character": 6}, "end": {"line": 1, "character": 10}},
                "text": "store"
            }],
        }),
    );
    session.read();
    let answer = session.request(
        "textDocument/documentSymbol",
        serde_json::json!({"textDocument": {"uri": uri}}),
    );
    let text = answer["result"].to_string();
    assert!(text.contains("\"store\""), "{text}");
    assert!(text.contains("\"Widget\""), "the rest of the file survived");

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// A file the editor never opened is read from disk — and re-read when it
/// changes there. An agent edits files and then asks about them; answering
/// from the first read served it the file as it was before its own edit.
#[test]
fn a_file_read_from_disk_is_reread_after_it_changes() {
    let (dir, db) = scratch("reread");
    repo(&dir);
    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);
    let names = |session: &mut Session| {
        session.request(
            "textDocument/documentSymbol",
            serde_json::json!({"textDocument": {"uri": uri_of(&dir, "app.rb")}}),
        )["result"]
            .to_string()
    };
    assert!(!names(&mut session).contains("Gadget"));
    fs::write(
        dir.join("app.rb"),
        "class Gadget\n  def spin\n  end\nend\n# longer than before\n",
    )
    .unwrap();
    assert!(names(&mut session).contains("Gadget"));

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// `exit` ends the process whether or not `shutdown` came first — a client
/// that skips it must not leave a server behind.
#[test]
fn exit_without_shutdown_still_stops_the_server() {
    let (dir, db) = scratch("exit");
    repo(&dir);
    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);
    session.notify("exit", serde_json::json!(null));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while session.child.try_wait().unwrap().is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "the server ignored exit"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    session.stdin.take();
    let _ = fs::remove_dir_all(&dir);
}

/// Index `source` as `app.rb` in a fresh repo, and open a session on it with
/// the file open in the editor.
fn indexed_session(label: &str, source: &str) -> (PathBuf, PathBuf, Session) {
    let (dir, db) = scratch(label);
    ruby_repo(&dir, &db, source);
    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);
    session.notify(
        "textDocument/didOpen",
        serde_json::json!({"textDocument": {
            "uri": uri_of(&dir, "app.rb"), "languageId": "ruby", "version": 1, "text": source
        }}),
    );
    (dir, db, session)
}

fn reference_lines(
    session: &mut Session,
    dir: &Path,
    line: u32,
    character: u32,
    declarations: bool,
) -> Vec<u64> {
    let answer = session.request(
        "textDocument/references",
        serde_json::json!({
            "textDocument": {"uri": uri_of(dir, "app.rb")},
            "position": {"line": line, "character": character},
            "context": {"includeDeclaration": declarations},
        }),
    );
    answer["result"]
        .as_array()
        .expect("an array of locations")
        .iter()
        .map(|l| l["range"]["start"]["line"].as_u64().unwrap() + 1)
        .collect()
}

/// References to a class are the constants that resolve to it — not every
/// constant with the same last name. Asking on a class returned nothing at
/// all, because only method call sites were ever searched.
#[test]
fn references_to_a_class_are_the_constants_that_resolve_to_it() {
    let source = concat!(
        "class Widget\n",         // 1
        "end\n",                  // 2
        "module Shop\n",          // 3
        "  class Widget\n",       // 4
        "  end\n",                // 5
        "  def self.make\n",      // 6
        "    Widget.new\n",       // 7 — Shop::Widget, not ::Widget
        "  end\n",                // 8
        "end\n",                  // 9
        "a = Widget.new\n",       // 10
        "b = ::Widget.new\n",     // 11
        "c = Shop::Widget.new\n", // 12
    );
    let (dir, _db, mut session) = indexed_session("constrefs", source);

    // On the top-level class's own name.
    assert_eq!(reference_lines(&mut session, &dir, 0, 6, false), [10, 11]);
    // With the declaration, which comes first.
    assert_eq!(reference_lines(&mut session, &dir, 0, 6, true), [1, 10, 11]);
    // And from a reference to the nested one.
    assert_eq!(reference_lines(&mut session, &dir, 11, 11, false), [7, 12]);

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn include_declaration_puts_the_definition_first() {
    let source = concat!(
        "class Widget\n",   // 1
        "  def save\n",     // 2
        "  end\n",          // 3
        "end\n",            // 4
        "w = Widget.new\n", // 5
        "w.save\n",         // 6
    );
    let (dir, _db, mut session) = indexed_session("decl", source);
    assert_eq!(reference_lines(&mut session, &dir, 5, 2, true), [2, 6]);
    assert_eq!(reference_lines(&mut session, &dir, 5, 2, false), [6]);
    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// An unsaved call site counts. The index is as of the last save; the buffer
/// is what the user is looking at, and references are read from it.
#[test]
fn references_count_a_call_that_exists_only_in_an_unsaved_buffer() {
    let source = concat!(
        "class Widget\n", // 1
        "  def save\n",   // 2
        "  end\n",        // 3
        "end\n",          // 4
    );
    let (dir, _db, mut session) = indexed_session("overlay", source);
    session.read();
    let edited = format!("{source}w = Widget.new\nw.save\n");
    session.notify(
        "textDocument/didChange",
        serde_json::json!({
            "textDocument": {"uri": uri_of(&dir, "app.rb"), "version": 2},
            "contentChanges": [{"text": edited}],
        }),
    );
    assert_eq!(reference_lines(&mut session, &dir, 1, 6, false), [6]);
    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// Columns are UTF-16 on the wire and bytes inside. A reference after a
/// multibyte character has to land on the name, not one short of it.
#[test]
fn a_reference_after_a_multibyte_character_lands_on_the_name() {
    let source = concat!(
        "class Widget\n",   // 1
        "  def save\n",     // 2
        "  end\n",          // 3
        "end\n",            // 4
        "w = Widget.new\n", // 5
        "é = 1; w.save\n",  // 6
    );
    let (dir, _db, mut session) = indexed_session("utf16", source);
    let answer = session.request(
        "textDocument/references",
        serde_json::json!({
            "textDocument": {"uri": uri_of(&dir, "app.rb")},
            "position": {"line": 1, "character": 6},
            "context": {"includeDeclaration": false},
        }),
    );
    let range = &answer["result"][0]["range"];
    // `é = 1; w.` is 9 UTF-16 units and 10 bytes.
    assert_eq!(range["start"]["character"], 9);
    assert_eq!(range["end"]["character"], 13, "and spans the name");
    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// Ask for the definition at a position until it answers, or give up. A
/// background index finishes when it finishes; the test waits for the
/// *answer*, not for a sleep that is long enough on this machine.
fn definition_eventually(
    session: &mut Session,
    uri: &str,
    line: u32,
    character: u32,
) -> serde_json::Value {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let answer = session.request(
            "textDocument/definition",
            serde_json::json!({
                "textDocument": {"uri": uri},
                "position": {"line": line, "character": character},
            }),
        );
        if !answer["result"].is_null() || std::time::Instant::now() > deadline {
            return answer["result"].clone();
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn commit_all(dir: &Path) {
    git(dir, &["add", "-A"]);
    git(
        dir,
        &[
            "-c",
            "user.email=t@e.st",
            "-c",
            "user.name=test",
            "commit",
            "-qm",
            "change",
        ],
    );
}

/// A save moves the index. A method added and saved in one file is found from
/// another straight away — the tree is assembled from the index, so without
/// this the new method did not exist until someone ran `--index`.
#[test]
fn a_saved_file_is_reindexed_so_other_files_see_its_new_methods() {
    let (dir, db) = scratch("save");
    ruby_repo(&dir, &db, "class Widget\n  def save\n  end\nend\n");
    fs::write(dir.join("job.rb"), "w = Widget.new\nw.polish\n").unwrap();
    commit_all(&dir);
    trekr()
        .args(["--index"])
        .current_dir(&dir)
        .env("TREKR_DB", &db)
        .output()
        .unwrap();

    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);
    let job = uri_of(&dir, "job.rb");
    let before = session.request(
        "textDocument/definition",
        serde_json::json!({"textDocument": {"uri": job}, "position": {"line": 1, "character": 3}}),
    );
    assert!(before["result"].is_null(), "no `polish` anywhere yet");

    fs::write(
        dir.join("app.rb"),
        "class Widget\n  def save\n  end\n  def polish\n  end\nend\n",
    )
    .unwrap();
    session.notify(
        "textDocument/didSave",
        serde_json::json!({"textDocument": {"uri": uri_of(&dir, "app.rb")}}),
    );
    let after = session.request(
        "textDocument/definition",
        serde_json::json!({"textDocument": {"uri": job}, "position": {"line": 1, "character": 3}}),
    );
    assert_eq!(
        after["result"][0]["range"]["start"]["line"], 3,
        "Widget#polish, found through the index the save refreshed"
    );

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// A save that meets another process writing the index is not lost: the
/// server answers meanwhile from what is committed, and the refresh lands once
/// the lock is free (DEC-065). A background index scanned before the save, so
/// its own write would not carry the edit.
#[test]
fn a_save_during_someone_elses_write_lands_once_the_lock_is_free() {
    let (dir, db) = scratch("save-busy");
    ruby_repo(&dir, &db, "class Widget\n  def save\n  end\nend\n");
    fs::write(dir.join("job.rb"), "w = Widget.new\nw.polish\n").unwrap();
    commit_all(&dir);
    trekr()
        .args(["--index"])
        .current_dir(&dir)
        .env("TREKR_DB", &db)
        .output()
        .unwrap();

    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);
    let job = uri_of(&dir, "job.rb");

    let writer = rusqlite::Connection::open(&db).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    fs::write(
        dir.join("app.rb"),
        "class Widget\n  def save\n  end\n  def polish\n  end\nend\n",
    )
    .unwrap();
    session.notify(
        "textDocument/didSave",
        serde_json::json!({"textDocument": {"uri": uri_of(&dir, "app.rb")}}),
    );
    let started = std::time::Instant::now();
    let during = session.request(
        "textDocument/definition",
        serde_json::json!({"textDocument": {"uri": job}, "position": {"line": 1, "character": 3}}),
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "the save must not hold up the next answer"
    );
    assert!(during["result"].is_null(), "not refreshed yet: {during}");

    drop(writer);
    let after = definition_eventually(&mut session, &job, 1, 3);
    assert_eq!(after[0]["range"]["start"]["line"], 3, "{after}");

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// A server started, warmed, and then left to find its store replaced by
/// `replace`. It must keep answering, say so once, and refill a store of its
/// own (DEC-300) — not stop answering, and not exit into a restart loop.
fn a_server_recovers_when(label: &str, replace: impl FnOnce(&Path)) -> (PathBuf, PathBuf) {
    let (dir, db) = scratch(label);
    ruby_repo(&dir, &db, "class Widget\n  def save\n  end\nend\n");
    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);
    for _ in 0..2 {
        session.request(
            "textDocument/definition",
            serde_json::json!({"textDocument": {"uri": uri_of(&dir, "app.rb")}, "position": {"line": 0, "character": 7}}),
        );
    }
    replace(&db);
    let mut shown = Vec::new();
    let mut refilled = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut id = 100;
    while !refilled && std::time::Instant::now() < deadline {
        id += 1;
        session.send(serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": "textDocument/documentSymbol",
            "params": {"textDocument": {"uri": uri_of(&dir, "app.rb")}},
        }));
        loop {
            let message = session.read();
            if message["method"] == "window/showMessage" {
                shown.push(message["params"]["message"].as_str().unwrap().to_string());
            }
            if message["id"] == id {
                assert!(message["result"].is_array(), "still answering: {message}");
                break;
            }
        }
        refilled = logged_events(&db, "index").iter().any(|e| e["ok"] == true);
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    session.stop();
    assert!(refilled, "the server refilled its store");
    assert_eq!(shown.len(), 1, "told once: {shown:?}");
    assert!(shown[0].contains("reindexing"), "{shown:?}");
    assert_eq!(logged_events(&db, "store_reopened").len(), 1);
    (dir, db)
}

fn logged_events(db: &Path, event: &str) -> Vec<serde_json::Value> {
    log_lines(db)
        .into_iter()
        .filter(|l| l["event"] == event)
        .collect()
}

fn blobs(db: &Path) -> i64 {
    rusqlite::Connection::open(db)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM blob", [], |r| r.get(0))
        .unwrap()
}

/// Another trekr rebuilt the store for a newer schema under a running server.
/// Writing into it would put this build's facts in the wrong format, so the
/// server moves to a store of its own beside it and refills that.
#[test]
fn a_server_whose_store_a_newer_trekr_rebuilt_keeps_its_own() {
    let before = std::cell::Cell::new(0);
    let (dir, db) = a_server_recovers_when("store-newer", |db| {
        let store = rusqlite::Connection::open(db).unwrap();
        let version: i64 = store
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        store
            .pragma_update(None, "user_version", version + 1)
            .unwrap();
        before.set(blobs(db));
    });
    assert_eq!(
        blobs(&db),
        before.get(),
        "nothing written into the newer store"
    );
    let own = fs::read_dir(db.parent().unwrap())
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .find(|n| n.starts_with("trekr.v") && n.ends_with(".db"))
        .expect("a store of its own");
    assert!(blobs(&db.with_file_name(own)) > 0, "refilled");
    let _ = fs::remove_dir_all(&dir);
}

/// A server started on a store a newer trekr wrote — an older build still
/// launched by an editor, beside a newer CLI — never writes into it: it
/// answers from a store of its own beside it, which it fills itself.
#[test]
fn a_server_started_on_a_newer_trekrs_store_keeps_its_own() {
    let (dir, db) = scratch("store-newer-at-start");
    fs::write(dir.join("Gemfile"), "source 'https://rubygems.org'\n").unwrap();
    ruby_repo(
        &dir,
        &db,
        "class Widget\n  def save\n  end\nend\nWidget.new.save\n",
    );
    let store = rusqlite::Connection::open(&db).unwrap();
    let version: i64 = store
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    store
        .pragma_update(None, "user_version", version + 1)
        .unwrap();
    drop(store);
    let before = blobs(&db);

    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);
    let answer = definition_eventually(&mut session, &uri_of(&dir, "app.rb"), 4, 12);
    assert_eq!(answer[0]["range"]["start"]["line"], 1, "{answer}");
    session.stop();

    assert_eq!(blobs(&db), before, "nothing written into the newer store");
    let own = db.with_file_name(format!("trekr.v{version}.db"));
    assert!(blobs(&own) > 0, "filled its own");
    let _ = fs::remove_dir_all(&dir);
}

/// The store was set aside as damaged (by another process) under a running
/// server: it reopens the rebuilt one and refills it.
#[test]
fn a_server_whose_store_was_set_aside_reopens_the_rebuilt_one() {
    let (dir, db) = a_server_recovers_when("store-damaged", |db| {
        let garbage = db.with_file_name("garbage");
        fs::write(&garbage, "not a database ".repeat(1000)).unwrap();
        fs::rename(&garbage, db).unwrap();
    });
    assert!(blobs(&db) > 0, "the rebuilt store was refilled");
    let broken = fs::read_dir(db.parent().unwrap())
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.contains(".broken-") && !n.ends_with("-wal"))
        .count();
    assert_eq!(broken, 1);
    let _ = fs::remove_dir_all(&dir);
}

/// The progress of one background index, read up to its end: the messages
/// its `report`s carried, and the one its `end` did. Messages the server sent
/// meanwhile are kept in `seen`.
fn progress_until_end(
    session: &mut Session,
    seen: &mut Vec<serde_json::Value>,
) -> (Vec<String>, String) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut reports = Vec::new();
    loop {
        assert!(std::time::Instant::now() < deadline, "progress never ended");
        let message = session.read();
        if message["method"] == "$/progress" {
            let value = &message["params"]["value"];
            let text = value["message"].as_str().unwrap_or_default().to_string();
            match value["kind"].as_str() {
                Some("report") => reports.push(text),
                Some("end") => return (reports, text),
                _ => {}
            }
        } else {
            seen.push(message);
        }
    }
}

/// A Ruby project's checkout, committed, that the store does not hold, and a
/// store that exists — holding another checkout — for a test to lock.
fn unindexed_beside_a_store(label: &str) -> (PathBuf, PathBuf, PathBuf) {
    let (dir, db) = scratch(label);
    let other = dir.with_extension("other");
    let _ = fs::remove_dir_all(&other);
    fs::create_dir_all(&other).unwrap();
    ruby_repo(&other, &db, "class Gadget\nend\n");
    git(&dir, &["init", "-q"]);
    fs::write(dir.join("Gemfile"), "source 'https://rubygems.org'\n").unwrap();
    fs::write(
        dir.join("app.rb"),
        "class Widget\n  def save\n  end\nend\nWidget.new.save\n",
    )
    .unwrap();
    commit_all(&dir);
    (dir, other, db)
}

/// A background index that outwaits another writer's lock has been told "ask
/// again" (exit 2, DEC-400), so the server does: it says it is waiting where
/// the editor shows it, a hover promises nothing that is not under way, and
/// once the lock is free the checkout is indexed after all.
#[test]
fn a_background_index_that_outwaits_the_lock_runs_again_once_it_is_free() {
    let (dir, other, db) = unindexed_beside_a_store("behind");
    let holder = rusqlite::Connection::open(&db).unwrap();
    holder.execute_batch("BEGIN IMMEDIATE").unwrap();

    let mut session = Session::start_with(&db, &dir, &[("TREKR_TEST_WRITER_WAIT_MS", "1000")]);
    session.initialize_with(
        &dir,
        serde_json::json!({"window": {"workDoneProgress": true}}),
    );
    let mut seen = Vec::new();
    let (reports, ended) = progress_until_end(&mut session, &mut seen);
    assert!(ended.contains("waiting for another trekr"), "{ended}");
    // VS Code shows no `end` message: the last `report` is what it shows.
    assert_eq!(reports.last(), Some(&ended), "{reports:?}");

    session.notify(
        "textDocument/didOpen",
        serde_json::json!({"textDocument": {
            "uri": uri_of(&dir, "app.rb"), "languageId": "ruby", "version": 1,
            "text": fs::read_to_string(dir.join("app.rb")).unwrap()
        }}),
    );
    let hover = hover_at(&mut session, &dir, 5, 12);
    assert!(hover.contains("not indexed yet"), "{hover}");
    assert!(
        hover.contains("another trekr") || hover.contains("is indexing it"),
        "says what is under way: {hover}"
    );

    holder.execute_batch("ROLLBACK").unwrap();
    let (_, ended) = progress_until_end(&mut session, &mut seen);
    assert_eq!(ended, "indexed");
    let answer = definition_eventually(&mut session, &uri_of(&dir, "app.rb"), 4, 12);
    assert_eq!(
        answer[0]["range"]["start"]["line"], 1,
        "Widget#save: {answer}"
    );

    // Its index has ended, so stopping leaves no child behind.
    session.stop();
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&other);
}

/// A first index cut short and then outwaited is asked again too, and says how
/// far the store got (DEC-400) — in the counts every other surface uses.
#[test]
fn a_background_index_cut_short_says_how_far_it_got() {
    let (dir, db) = scratch("outwaited");
    // A Ruby project: what the server indexes unasked.
    fs::write(dir.join("Gemfile"), "source 'https://rubygems.org'\n").unwrap();
    ruby_repo(&dir, &db, "class Widget\nend\n");
    let holder = rusqlite::Connection::open(&db).unwrap();
    let root: String = holder
        .query_row("SELECT root FROM checkout WHERE kind = 'repo'", [], |r| {
            r.get(0)
        })
        .unwrap();
    holder
        .execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)",
            [format!("warming {root}"), format!("{} 1 2", i32::MAX)],
        )
        .unwrap();
    holder.execute_batch("BEGIN IMMEDIATE").unwrap();

    let mut session = Session::start_with(&db, &dir, &[("TREKR_TEST_WRITER_WAIT_MS", "1000")]);
    session.initialize_with(
        &dir,
        serde_json::json!({"window": {"workDoneProgress": true}}),
    );
    let mut seen = Vec::new();
    let (_, ended) = progress_until_end(&mut session, &mut seen);
    assert!(ended.contains("(1 of 2 files read"), "{ended}");
    assert!(ended.contains("waiting for another trekr"), "{ended}");

    // Reaped, not left reading a deleted directory after the suite.
    holder.execute_batch("ROLLBACK").unwrap();
    let (_, ended) = progress_until_end(&mut session, &mut seen);
    assert_eq!(ended, "indexed");
    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// A Ruby project nobody indexed is indexed in the background, with progress
/// for a client that can show it, and answers start arriving without anyone
/// running `trekr --index`.
#[test]
fn an_unindexed_project_is_indexed_in_the_background_with_progress() {
    let (dir, db) = scratch("cold");
    git(&dir, &["init", "-q"]);
    fs::write(dir.join("Gemfile"), "source 'https://rubygems.org'\n").unwrap();
    fs::write(
        dir.join("app.rb"),
        "class Widget\n  def save\n  end\nend\nw = Widget.new\nw.save\n",
    )
    .unwrap();
    commit_all(&dir);

    let mut session = Session::start(&db, &dir);
    session.initialize_with(
        &dir,
        serde_json::json!({"window": {"workDoneProgress": true}}),
    );

    // The server asks to create a progress token, then begins and ends it.
    let mut seen = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !seen.contains(&"end".to_string()) {
        assert!(
            std::time::Instant::now() < deadline,
            "progress never ended: {seen:?}"
        );
        let message = session.read();
        if message["method"] == "window/workDoneProgress/create" {
            seen.push("create".into());
        } else if message["method"] == "$/progress" {
            seen.push(
                message["params"]["value"]["kind"]
                    .as_str()
                    .unwrap()
                    .to_string(),
            );
        }
    }
    assert_eq!(seen, ["create", "begin", "report", "end"]);

    let answer = definition_eventually(&mut session, &uri_of(&dir, "app.rb"), 5, 3);
    assert_eq!(
        answer[0]["range"]["start"]["line"], 1,
        "Widget#save, from the new index"
    );

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// After an upgrade emptied the store, the background index refilling it is
/// said — in its progress, and in every hover until it ends — rather than
/// each answer reading as "nothing trekr has indexed defines" (DEC-275).
#[test]
fn a_hover_while_the_index_refills_after_an_upgrade_says_so() {
    let (dir, db) = scratch("upgrade-window");
    git(&dir, &["init", "-q"]);
    fs::write(dir.join("Gemfile"), "source 'https://rubygems.org'\n").unwrap();
    fs::write(
        dir.join("app.rb"),
        "class Widget\n  def save\n  end\nend\nWidget.new.save\n",
    )
    .unwrap();
    commit_all(&dir);
    // An older trekr's store, which this one rebuilds, and so empties.
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch(
            "PRAGMA journal_mode=WAL; CREATE TABLE checkout (x); PRAGMA user_version = 1;",
        )
        .unwrap();
    trekr()
        .args(["--status"])
        .current_dir(&dir)
        .env("TREKR_DB", &db)
        .output()
        .unwrap();
    // Someone else writing: the index waits, and the window stays open.
    let writer = rusqlite::Connection::open(&db).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();

    let mut session = Session::start(&db, &dir);
    session.initialize_with(
        &dir,
        serde_json::json!({"window": {"workDoneProgress": true}}),
    );
    let progress = |session: &mut Session, kind: &str| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            assert!(std::time::Instant::now() < deadline, "no progress {kind}");
            let message = session.read();
            if message["method"] == "$/progress" && message["params"]["value"]["kind"] == kind {
                return message["params"]["value"].clone();
            }
        }
    };
    let begun = progress(&mut session, "begin");
    assert!(
        begun["message"]
            .as_str()
            .unwrap()
            .contains("after an upgrade"),
        "{begun}"
    );
    let hover = |session: &mut Session| {
        let answer = ask(session, &dir, "textDocument/hover", "app.rb", 4, 12);
        answer["result"]["contents"]["value"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    };
    let during = hover(&mut session);
    assert!(
        during.contains("reindexing this checkout after an upgrade"),
        "{during}"
    );

    drop(writer);
    progress(&mut session, "end");
    let after = hover(&mut session);
    assert!(!after.contains("after an upgrade"), "{after}");

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// The background index steps out of the editor's way — lower CPU and disk
/// priority — and a `trekr --index` run by hand does not.
#[test]
fn the_background_index_runs_at_lowered_priority_and_a_hand_run_does_not() {
    let (dir, db) = scratch("niced");
    git(&dir, &["init", "-q"]);
    fs::write(dir.join("Gemfile"), "source 'https://rubygems.org'\n").unwrap();
    fs::write(dir.join("app.rb"), "class Widget\nend\n").unwrap();
    commit_all(&dir);

    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);
    // The child reads its own priority back after lowering it, so this is
    // what the kernel holds, not what was asked for.
    let lowered = logged(&db, "index_priority");
    assert!(
        lowered["nice"].as_i64().unwrap() >= 10,
        "niced by 10 from wherever the server ran: {lowered}"
    );
    let io = if cfg!(target_os = "macos") {
        "utility"
    } else {
        "best-effort-7"
    };
    assert_eq!(lowered["io"], io, "{lowered}");
    session.stop();

    let by_hand = trekr()
        .arg("--index")
        .current_dir(&dir)
        .env("TREKR_DB", &db)
        .env("TREKR_LOG", log_path(&db))
        .output()
        .unwrap();
    assert!(by_hand.status.success());
    let lowered = log_lines(&db)
        .into_iter()
        .filter(|line| line["event"] == "index_priority")
        .count();
    assert_eq!(lowered, 1, "only the server's child stepped aside");

    let _ = fs::remove_dir_all(&dir);
}

/// A deleted file leaves the index. A deletion is reported by the client's
/// watcher and handed to a full index, since refreshing one file can add or
/// replace it but not remove it.
#[test]
fn a_deleted_file_reported_by_the_watcher_leaves_the_index() {
    let (dir, db) = scratch("deleted");
    ruby_repo(&dir, &db, "class Widget\n  def save\n  end\nend\n");
    fs::write(dir.join("gadget.rb"), "class Gadget\nend\n").unwrap();
    commit_all(&dir);
    trekr()
        .args(["--index"])
        .current_dir(&dir)
        .env("TREKR_DB", &db)
        .output()
        .unwrap();

    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);
    let found = |session: &mut Session| {
        session.request("workspace/symbol", serde_json::json!({"query": "Gadget"}))["result"]
            .as_array()
            .map_or(0, Vec::len)
    };
    assert_eq!(found(&mut session), 1);

    fs::remove_file(dir.join("gadget.rb")).unwrap();
    git(&dir, &["add", "-A"]);
    session.notify(
        "workspace/didChangeWatchedFiles",
        serde_json::json!({"changes": [{"uri": uri_of(&dir, "gadget.rb"), "type": 3}]}),
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while found(&mut session) != 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "Gadget was never forgotten"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// `db:migrate` rewrites `db/structure.sql`, and the watcher reports it: the
/// column's new type reaches what a call on it resolves to, not only a
/// hover, which reads the dump as it is.
#[test]
fn a_rewritten_structure_sql_reported_by_the_watcher_retypes_its_columns() {
    let (dir, db) = scratch("structure-sql");
    fs::create_dir_all(dir.join("db")).unwrap();
    fs::write(
        dir.join("stubs.rb"),
        "module ActiveRecord\n  class Base\n  end\nend\n",
    )
    .unwrap();
    let dump = |kind: &str| {
        let text = format!(
            "CREATE TABLE public.widgets (\n    id bigint NOT NULL,\n    code {kind}\n);\n"
        );
        fs::write(dir.join("db/structure.sql"), text).unwrap();
    };
    dump("integer");
    ruby_repo(
        &dir,
        &db,
        "class Widget < ActiveRecord::Base\n  def shout\n    code.upcase\n  end\nend\n",
    );

    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);
    let text = hover_at(&mut session, &dir, 3, 4);
    assert!(
        text.contains("Column `widgets.code`: `integer`"),
        "the column, from the dump: {text}"
    );
    assert!(!hover_at(&mut session, &dir, 3, 9).contains("String#upcase"));

    dump("text");
    session.notify(
        "workspace/didChangeWatchedFiles",
        serde_json::json!({"changes": [{"uri": uri_of(&dir, "db/structure.sql"), "type": 2}]}),
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !hover_at(&mut session, &dir, 3, 9).contains("String#upcase") {
        assert!(
            std::time::Instant::now() < deadline,
            "the column was never retyped"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

const SHOP: &str = concat!(
    "module Shop\n",                 // 1
    "  class Widget\n",              // 2
    "    LIMIT = 3\n",               // 3
    "    def save\n",                // 4
    "    end\n",                     // 5
    "    def self.build\n",          // 6
    "    end\n",                     // 7
    "    private\n",                 // 8
    "    def secret\n",              // 9
    "    end\n",                     // 10
    "  end\n",                       // 11
    "end\n",                         // 12
    "class Gadget < Shop::Widget\n", // 13
    "  def polish(level)\n",         // 14
    "    count = 1\n",               // 15
    "  end\n",                       // 16
    "end\n",                         // 17
);

/// A completion answered from the checkout's member listing. Until the
/// listing is built — longer than the server waits for it, on a loaded
/// machine — completion answers without it and says `isIncomplete`, and the
/// client asks again (DEC-323); so does this. Only for an answer that is
/// complete once listed: a truncated, untyped or ambiguous one never is.
fn listed(session: &mut Session, params: serde_json::Value) -> serde_json::Value {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let answer = session.request("textDocument/completion", params.clone());
        if !answer["result"]["isIncomplete"].as_bool().unwrap_or(false) {
            return answer;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "never listed: {answer}"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// `indexed_session` for completion: its diagnostics read, and the member
/// listing in, so each test's first answer is the one it asserts on. The
/// listing is the checkout's, and an edit to the buffer does not move it.
fn completion_session(label: &str, source: &str) -> (PathBuf, Session) {
    let (dir, _db, mut session) = indexed_session(label, source);
    session.read();
    // A word nothing is named: incomplete only while the listing is not in.
    let word = "zz_unnamed";
    session.notify(
        "textDocument/didChange",
        serde_json::json!({
            "textDocument": {"uri": uri_of(&dir, "app.rb"), "version": 1},
            "contentChanges": [{"text": format!("{source}{word}\n")}],
        }),
    );
    listed(
        &mut session,
        serde_json::json!({
            "textDocument": {"uri": uri_of(&dir, "app.rb")},
            "position": {"line": source.lines().count(), "character": word.len()},
        }),
    );
    (dir, session)
}

/// Complete at the end of `line` after replacing the file's text with
/// `source`, returning (labels in rank order, isIncomplete).
fn complete(
    session: &mut Session,
    dir: &Path,
    source: &str,
    line: u32,
    version: i32,
) -> (Vec<String>, bool) {
    let character = source.lines().nth(line as usize).unwrap().len() as u32;
    session.notify(
        "textDocument/didChange",
        serde_json::json!({
            "textDocument": {"uri": uri_of(dir, "app.rb"), "version": version},
            "contentChanges": [{"text": source}],
        }),
    );
    let answer = session.request(
        "textDocument/completion",
        serde_json::json!({
            "textDocument": {"uri": uri_of(dir, "app.rb")},
            "position": {"line": line, "character": character},
        }),
    );
    let mut items: Vec<(String, String)> = answer["result"]["items"]
        .as_array()
        .expect("a completion list")
        .iter()
        .map(|i| {
            (
                i["sortText"].as_str().unwrap().to_string(),
                i["label"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    items.sort();
    (
        items.into_iter().map(|(_, label)| label).collect(),
        answer["result"]["isIncomplete"].as_bool().unwrap(),
    )
}

/// Completion is receiver-aware: after a typed receiver it lists that type's
/// methods — own first, then inherited — and never the class-side ones, nor
/// private ones on an explicit receiver. After the class itself it lists the
/// class-side ones.
#[test]
fn completion_after_a_dot_lists_the_receivers_methods_in_lookup_order() {
    let (dir, mut session) = completion_session("complete-dot", SHOP);

    let instance = format!("{SHOP}w = Shop::Widget.new\nw.\n");
    let (labels, _) = complete(&mut session, &dir, &instance, 18, 2);
    assert_eq!(
        labels.first().map(String::as_str),
        Some("save"),
        "{labels:?}"
    );
    assert!(
        !labels.contains(&"build".to_string()),
        "a class method is not an instance's"
    );
    assert!(
        !labels.contains(&"secret".to_string()),
        "private, and the receiver is not self"
    );
    assert!(
        labels.contains(&"object_id".to_string()),
        "inherited from core, ranked after"
    );
    // Kernel's module functions are private, as `private :name` makes any
    // method: bare only.
    assert!(!labels.contains(&"puts".to_string()), "{labels:?}");
    assert!(
        !labels.contains(&"method_missing".to_string()),
        "{labels:?}"
    );

    let class_side = format!("{SHOP}Shop::Widget.b\n");
    let (labels, _) = complete(&mut session, &dir, &class_side, 17, 3);
    assert_eq!(
        labels.first().map(String::as_str),
        Some("build"),
        "{labels:?}"
    );
    assert!(!labels.contains(&"save".to_string()));

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn completion_after_a_scope_lists_its_constants() {
    let (dir, mut session) = completion_session("complete-scope", SHOP);
    let (labels, _) = complete(&mut session, &dir, &format!("{SHOP}Shop::\n"), 17, 2);
    assert_eq!(labels, ["Widget"]);
    let (labels, _) = complete(
        &mut session,
        &dir,
        &format!("{SHOP}Shop::Widget::\n"),
        17,
        3,
    );
    assert_eq!(labels, ["LIMIT"]);
    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// A bare word inside a method: locals and parameters first, then the
/// enclosing class's methods up its chain — inherited ones included, private
/// ones too, since the receiver is self.
#[test]
fn completion_of_a_bare_word_offers_locals_then_the_classs_methods() {
    let (dir, mut session) = completion_session("complete-bare", SHOP);
    let source = SHOP.replace("    count = 1\n", "    count = 1\n    \n");
    let (labels, _) = complete(&mut session, &dir, &source, 15, 2);
    let at = |name: &str| labels.iter().position(|l| l == name);
    assert!(at("count").is_some() && at("level").is_some(), "{labels:?}");
    assert!(at("count") < at("polish"), "locals before methods");
    assert!(at("polish") < at("save"), "own before inherited");
    assert!(at("secret").is_some(), "private is callable on self");
    assert!(at("puts").is_some(), "and so is Kernel's");

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// In a module, `self` is whatever mixes it in: a bare word in its `included
/// do` block is offered the includer's class methods, and one in its method
/// the includer's instance methods, after the module's own (DEC-127).
#[test]
fn completion_in_a_module_offers_what_its_includers_have() {
    let source = concat!(
        "module ActiveSupport\n",                      // 0
        "  module Concern\n",                          // 1
        "    def included(base = nil, &block); end\n", // 2
        "  end\n",                                     // 3
        "end\n",                                       // 4
        "module Trackable\n",                          // 5
        "  extend ActiveSupport::Concern\n",           // 6
        "  included do\n",                             // 7
        "    \n",                                      // 8
        "  end\n",                                     // 9
        "  def track\n",                               // 10
        "    \n",                                      // 11
        "  end\n",                                     // 12
        "end\n",                                       // 13
        "class Widget\n",                              // 14
        "  include Trackable\n",                       // 15
        "  def self.tracked_scope; end\n",             // 16
        "  def stamp!; end\n",                         // 17
        "end\n",                                       // 18
    );
    let (dir, mut session) = completion_session("complete-mixed", source);
    let at_line = |line: usize, word: &str| {
        let mut lines: Vec<String> = source.lines().map(str::to_string).collect();
        lines[line] = format!("    {word}");
        lines.join("\n") + "\n"
    };
    let (labels, _) = complete(&mut session, &dir, &at_line(8, "track"), 8, 2);
    assert!(labels.contains(&"tracked_scope".to_string()), "{labels:?}");
    assert!(
        !labels.contains(&"stamp!".to_string()),
        "an instance method: {labels:?}"
    );
    let (labels, _) = complete(&mut session, &dir, &at_line(11, "sta"), 11, 3);
    assert!(labels.contains(&"stamp!".to_string()), "{labels:?}");
    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// An `on_load` block runs on the class that runs the hook, and a `def` in
/// it on that class's instances, so a bare word there completes from the
/// class's methods, not `Object`'s (DEC-214).
#[test]
fn completion_in_an_on_load_block_offers_the_hooked_classs_methods() {
    let source = concat!(
        "module ActiveSupport\n",                                // 0
        "  def self.on_load(name, options = {}, &block); end\n", // 1
        "  def self.run_load_hooks(name, base = Object); end\n", // 2
        "end\n",                                                 // 3
        "class Record\n",                                        // 4
        "  def self.establish_link(config); end\n",              // 5
        "  def persist!; end\n",                                 // 6
        "  ActiveSupport.run_load_hooks(:record, self)\n",       // 7
        "end\n",                                                 // 8
        "ActiveSupport.on_load(:record) do\n",                   // 9
        "  \n",                                                  // 10
        "  def touch_later\n",                                   // 11
        "    \n",                                                // 12
        "  end\n",                                               // 13
        "end\n",                                                 // 14
    );
    let (dir, mut session) = completion_session("complete-on-load", source);
    let at_line = |line: usize, word: &str| {
        let mut lines: Vec<String> = source.lines().map(str::to_string).collect();
        lines[line] = format!("    {word}");
        lines.join("\n") + "\n"
    };
    let (labels, _) = complete(&mut session, &dir, &at_line(10, "estab"), 10, 2);
    assert!(labels.contains(&"establish_link".to_string()), "{labels:?}");
    let (labels, _) = complete(&mut session, &dir, &at_line(12, "pers"), 12, 3);
    assert!(labels.contains(&"persist!".to_string()), "{labels:?}");
    assert!(
        !labels.contains(&"establish_link".to_string()),
        "a class method, in an instance's `def`: {labels:?}"
    );
    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// A listing cut at its cap is the same listing every time: the names that
/// sort first, as the client will show them. It was whichever members a hash
/// map happened to yield first, which changed from one process to the next.
#[test]
fn a_truncated_completion_keeps_the_names_that_sort_first() {
    // More methods than one answer carries, declared out of order.
    let mut names: Vec<String> = (0..320).map(|i| format!("m{i:03}")).collect();
    names.reverse();
    let body: String = names.iter().map(|n| format!("  def {n}; end\n")).collect();
    let source = format!("class Crowd\n{body}end\n");
    let (dir, mut session) = completion_session("complete-cap", &source);
    let asking = format!("{source}w = Crowd.new\nw.\n");
    let line = asking.lines().count() as u32 - 1;
    let (labels, incomplete) = complete(&mut session, &dir, &asking, line, 2);
    let expected: Vec<String> = (0..300).map(|i| format!("m{i:03}")).collect();
    assert_eq!(labels, expected);
    assert!(incomplete, "cut at the cap, so the client asks again");
    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// An untyped receiver gets a short list of names that fit the prefix,
/// marked incomplete — and nothing at all before a prefix is typed.
#[test]
fn completion_on_an_untyped_receiver_is_short_and_disclosed() {
    let (dir, mut session) = completion_session("complete-untyped", SHOP);
    let (labels, incomplete) = complete(
        &mut session,
        &dir,
        &format!("{SHOP}def go(x)\n  x.sa\n"),
        18,
        2,
    );
    assert!(labels.contains(&"save".to_string()), "{labels:?}");
    assert!(incomplete, "guesses are never the whole answer");
    let (labels, incomplete) = complete(
        &mut session,
        &dir,
        &format!("{SHOP}def go(x)\n  x.\n"),
        18,
        3,
    );
    assert!(labels.is_empty() && incomplete, "{labels:?}");

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// A chain typed by what every definition of the previous name returns
/// completes as go-to-definition resolves it, though the next name is not
/// written yet — and says it is not the whole answer when a definition
/// declared nothing.
#[test]
fn completion_after_a_chain_lists_what_the_chain_returns() {
    let (dir, mut session) = completion_session("complete-chain", SHOP);
    let (labels, incomplete) = complete(
        &mut session,
        &dir,
        &format!("{SHOP}def go(x)\n  x.lines.\n"),
        18,
        2,
    );
    assert!(labels.contains(&"flatten".to_string()), "{labels:?}");
    assert!(
        !incomplete,
        "every definition agreed, so the listing is whole"
    );

    session.stop();
    let _ = fs::remove_dir_all(&dir);

    // Label#strip declares nothing, so String is one reading of two.
    let labelled = format!("{SHOP}class Label\n  def strip; end\nend\n");
    let (dir, mut session) = completion_session("complete-chain-split", &labelled);
    let (labels, incomplete) = complete(
        &mut session,
        &dir,
        &format!("{labelled}def go(x)\n  x.strip.\n"),
        21,
        2,
    );
    assert!(labels.contains(&"upcase".to_string()), "{labels:?}");
    assert!(
        incomplete,
        "a guess among competitors is not the whole answer"
    );

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// Ruby core is written out as files a definition can land in. When that
/// fails, every landing in core answers nothing, and the log says why.
#[test]
fn a_core_directory_that_cannot_be_written_is_logged() {
    let (dir, _) = scratch("core-unwritable");
    // A store of its own: the core directory is beside it, and a file in its
    // place is what makes the write fail.
    let home = dir.with_extension("store");
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join("t.core"), "not a directory").unwrap();
    let db = home.join("t.db");
    ruby_repo(&dir, &db, "class Widget\nend\n");
    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);
    session.stop();
    let failed = log_lines(&db)
        .into_iter()
        .find(|line| line["event"] == "core_files_failed");
    assert!(failed.is_some(), "{:?}", log_lines(&db));
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&home);
}

/// A workspace opened through a symlink is answered in the client's spelling.
/// The store is canonical; sending canonical paths back made the editor open
/// the same file a second time under its other name.
#[cfg(unix)]
#[test]
fn locations_come_back_in_the_spelling_the_client_used() {
    let source = concat!(
        "class Widget\n",   // 1
        "  def save\n",     // 2
        "  end\n",          // 3
        "end\n",            // 4
        "w = Widget.new\n", // 5
        "w.save\n",         // 6
    );
    let (dir, db) = scratch("spelling");
    ruby_repo(&dir, &db, source);
    let link = dir.with_extension("link");
    let _ = fs::remove_file(&link);
    std::os::unix::fs::symlink(&dir, &link).unwrap();

    let mut session = Session::start(&db, &link);
    session.initialize(&link);
    let answer = session.request(
        "textDocument/definition",
        serde_json::json!({
            "textDocument": {"uri": uri_of(&link, "app.rb")},
            "position": {"line": 5, "character": 3},
        }),
    );
    let uri = answer["result"][0]["uri"].as_str().expect("a location");
    assert_eq!(
        uri,
        uri_of(&link, "app.rb"),
        "the link, not what it points at"
    );
    // Hover's "Defined in" is a link inside markdown, and follows suit.
    let hover = session.request(
        "textDocument/hover",
        serde_json::json!({
            "textDocument": {"uri": uri_of(&link, "app.rb")},
            "position": {"line": 5, "character": 3},
        }),
    );
    let text = hover["result"]["contents"]["value"]
        .as_str()
        .expect("a hover");
    assert!(text.contains(&uri_of(&link, "app.rb")), "{text}");

    session.stop();
    let _ = fs::remove_file(&link);
    let _ = fs::remove_dir_all(&dir);
}

/// A checkout requiring its own files and a vendored gem's. `shelf` is both
/// the checkout's `lib/shelf.rb` and the gem's, so it has two answers.
fn require_repo(dir: &Path, db: &Path) {
    git(dir, &["init", "-q"]);
    let gem = dir.join("vendor/bundle/ruby/3.3.0/gems/shelf-1.0.0/lib");
    fs::create_dir_all(gem.join("shelf")).unwrap();
    fs::write(gem.join("shelf.rb"), "module Shelf\nend\n").unwrap();
    fs::write(
        gem.join("shelf/rack.rb"),
        "module Shelf\n  class Rack\n  end\nend\n",
    )
    .unwrap();
    fs::write(dir.join(".gitignore"), "vendor/\n").unwrap();
    fs::write(
        dir.join("Gemfile.lock"),
        "GEM\n  remote: https://rubygems.org/\n  specs:\n    shelf (1.0.0)\n\nDEPENDENCIES\n  shelf\n",
    )
    .unwrap();
    fs::create_dir_all(dir.join("lib/widget")).unwrap();
    fs::write(dir.join("lib/widget.rb"), "class Widget\nend\n").unwrap();
    fs::write(dir.join("lib/widget/gear.rb"), "class Widget::Gear\nend\n").unwrap();
    fs::write(dir.join("lib/shelf.rb"), "module Shelf\nend\n").unwrap();
    fs::write(dir.join("app.rb"), REQUIRES).unwrap();
    commit_all(dir);
    let indexed = trekr()
        .args(["--index"])
        .current_dir(dir)
        .env("TREKR_DB", db)
        .output()
        .unwrap();
    assert!(indexed.status.success());
}

const REQUIRES: &str = concat!(
    "require_relative \"lib/widget/gear\"\n", // 1
    "require \"widget\"\n",                   // 2
    "require \"shelf/rack\"\n",               // 3
    "require \"shelf\"\n",                    // 4
    "require \"nowhere\"\n",                  // 5
);

fn definition_at(
    session: &mut Session,
    dir: &Path,
    line: u32,
    character: u32,
) -> serde_json::Value {
    session.request(
        "textDocument/definition",
        serde_json::json!({
            "textDocument": {"uri": uri_of(dir, "app.rb")},
            "position": {"line": line - 1, "character": character},
        }),
    )["result"]
        .clone()
}

fn require_session(label: &str, capabilities: serde_json::Value) -> (PathBuf, Session) {
    let (dir, db) = scratch(label);
    require_repo(&dir, &db);
    let mut session = Session::start(&db, &dir);
    session.initialize_with(&dir, capabilities);
    (dir, session)
}

/// The whole string is what was clicked, wherever in it the cursor is, and
/// the answer is the file.
#[test]
fn definition_on_a_require_string_opens_the_required_file() {
    let links = serde_json::json!({"textDocument": {"definition": {"linkSupport": true}}});
    let (dir, mut session) = require_session("require-def", links);

    let answer = definition_at(&mut session, &dir, 1, 24);
    let [link] = answer.as_array().expect("links").as_slice() else {
        panic!("one file: {answer}");
    };
    assert!(
        link["targetUri"]
            .as_str()
            .unwrap()
            .ends_with("/lib/widget/gear.rb"),
        "{link}"
    );
    assert_eq!(
        link["targetRange"]["start"],
        serde_json::json!({"line": 0, "character": 0})
    );
    assert_eq!(
        link["originSelectionRange"],
        serde_json::json!({"start": {"line": 0, "character": 17}, "end": {"line": 0, "character": 34}}),
        "the string literal, quotes included"
    );

    for (line, file) in [
        (2, "/lib/widget.rb"),
        (3, "/gems/shelf-1.0.0/lib/shelf/rack.rb"),
    ] {
        let answer = definition_at(&mut session, &dir, line, 10);
        assert!(
            answer[0]["targetUri"].as_str().unwrap().ends_with(file),
            "line {line}: {answer}"
        );
    }

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// Several files on the load path are all offered, the checkout's first; a
/// require nothing satisfies answers nothing rather than a nearby file.
#[test]
fn a_require_with_several_files_lists_them_and_one_with_none_answers_nothing() {
    let (dir, mut session) = require_session("require-many", serde_json::json!({}));

    let answer = definition_at(&mut session, &dir, 4, 10);
    let uris: Vec<&str> = answer
        .as_array()
        .expect("plain locations without linkSupport")
        .iter()
        .map(|location| location["uri"].as_str().unwrap())
        .collect();
    assert_eq!(uris.len(), 2, "{answer}");
    assert!(uris[0].ends_with(&format!(
        "{}/lib/shelf.rb",
        dir.file_name().unwrap().to_string_lossy()
    )));
    assert!(uris[1].ends_with("/gems/shelf-1.0.0/lib/shelf.rb"));

    assert!(definition_at(&mut session, &dir, 5, 10).is_null());

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn require_strings_with_one_file_behind_them_are_links() {
    let (dir, mut session) = require_session("require-links", serde_json::json!({}));
    let answer = session.request(
        "textDocument/documentLink",
        serde_json::json!({"textDocument": {"uri": uri_of(&dir, "app.rb")}}),
    );
    let links = answer["result"].as_array().expect("links");
    let lines: Vec<u64> = links
        .iter()
        .map(|link| link["range"]["start"]["line"].as_u64().unwrap())
        .collect();
    assert_eq!(
        lines,
        vec![0, 1, 2],
        "the ambiguous and the missing are not links: {answer}"
    );
    assert!(
        links[2]["target"]
            .as_str()
            .unwrap()
            .ends_with("/shelf/rack.rb")
    );
    assert_eq!(links[2]["tooltip"], "lib/shelf/rack.rb (gem shelf-1.0.0)");

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn hover_on_a_require_names_the_file_and_its_gem() {
    let (dir, mut session) = require_session("require-hover", serde_json::json!({}));

    let text = hover_at(&mut session, &dir, 3, 12);
    assert!(
        text.starts_with("Loads [`lib/shelf/rack.rb`](file://")
            && text.ends_with(" · gem `shelf-1.0.0`"),
        "{text}"
    );
    let text = hover_at(&mut session, &dir, 4, 10);
    assert!(text.starts_with("2 files match `shelf`"), "{text}");

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// A checkout of many files, indexed, and a session on it whose client set
/// `referenceLimit`. `files` maps a relative path to its source.
fn many_file_session(label: &str, files: &[(String, String)], limit: u64) -> (PathBuf, Session) {
    let (dir, db) = scratch(label);
    git(&dir, &["init", "-q"]);
    for (path, source) in files {
        let path = dir.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    }
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
            "init",
        ],
    );
    trekr()
        .args(["--index"])
        .current_dir(&dir)
        .env("TREKR_DB", &db)
        .output()
        .unwrap();
    let mut session = Session::start(&db, &dir);
    session.request(
        "initialize",
        serde_json::json!({
            "processId": null,
            "rootUri": format!("file://{}", dir.display()),
            "capabilities": {},
            "initializationOptions": { "index": false, "referenceLimit": limit },
        }),
    );
    session.notify("initialized", serde_json::json!({}));
    (dir, session)
}

/// Send a request and read up to its answer, keeping the notifications that
/// came before it — partial results and messages arrive that way.
fn request_with_notes(
    session: &mut Session,
    method: &str,
    params: serde_json::Value,
) -> (serde_json::Value, Vec<serde_json::Value>) {
    session.next_id += 1;
    let id = session.next_id;
    session.send(serde_json::json!({
        "jsonrpc": "2.0", "id": id, "method": method, "params": params
    }));
    let mut notes = Vec::new();
    loop {
        let message = session.read();
        if message.get("id").and_then(|v| v.as_i64()) == Some(id) {
            return (message, notes);
        }
        notes.push(message);
    }
}

fn shown_messages(notes: &[serde_json::Value]) -> Vec<String> {
    notes
        .iter()
        .filter(|n| n["method"] == "window/showMessage")
        .map(|n| n["params"]["message"].as_str().unwrap().to_string())
        .collect()
}

fn batches(notes: &[serde_json::Value], token: &str) -> Vec<Vec<(String, u64)>> {
    notes
        .iter()
        .filter(|n| n["method"] == "$/progress" && n["params"]["token"] == token)
        .map(|n| places(&n["params"]["value"]))
        .collect()
}

/// (file name, 1-based line) for each location.
fn places(locations: &serde_json::Value) -> Vec<(String, u64)> {
    locations
        .as_array()
        .expect("locations")
        .iter()
        .map(|l| {
            let uri = l["uri"].as_str().unwrap();
            let file = uri.rsplit('/').next().unwrap().to_string();
            (file, l["range"]["start"]["line"].as_u64().unwrap() + 1)
        })
        .collect()
}

/// A widget, one call whose receiver resolves to it, and one that could be
/// anything — in a file that sorts first, so path order alone would lead with
/// the weaker evidence.
fn evidence_files() -> Vec<(String, String)> {
    vec![
        (
            "app/widget.rb".into(),
            "class Widget\n  def save\n  end\nend\n".into(),
        ),
        (
            "app/a_guess.rb".into(),
            "def guess(thing)\n  thing.save\nend\n".into(),
        ),
        (
            "app/z_sure.rb".into(),
            "def sure\n  w = Widget.new\n  w.save\nend\n".into(),
        ),
    ]
}

fn on_save(dir: &Path, declarations: bool) -> serde_json::Value {
    serde_json::json!({
        "textDocument": {"uri": uri_of(dir, "app/widget.rb")},
        "position": {"line": 1, "character": 6},
        "context": {"includeDeclaration": declarations},
    })
}

/// The cut keeps the confirmed caller over the possible one, and says so.
#[test]
fn a_capped_answer_keeps_the_confirmed_caller_and_says_what_it_left_out() {
    let (dir, mut session) = many_file_session("refs-cap", &evidence_files(), 1);
    let (answer, notes) = request_with_notes(
        &mut session,
        "textDocument/references",
        on_save(&dir, false),
    );
    assert_eq!(places(&answer["result"]), [("z_sure.rb".to_string(), 3)]);
    let said = shown_messages(&notes);
    assert_eq!(said.len(), 1, "{notes:?}");
    assert!(
        said[0].contains("1 of 2 references to `save`"),
        "{}",
        said[0]
    );
    assert!(
        said[0].contains("trekr --refs 'Widget#save'"),
        "{}",
        said[0]
    );

    // Uncut, both are listed, confirmed first, and nothing is said.
    session.stop();
    let rows = usage_rows(&scratch_db("refs-cap"));
    assert_eq!(
        counted(
            &rows,
            // The session's first request, mapping the tree its index prepared.
            &[("feature", "references".into()), ("flags", "cut".into())]
        ),
        1,
        "a cap hit is counted, so `--usage` shows how often the limit bites: {rows:?}"
    );
    let (dir, mut session) = many_file_session("refs-uncut", &evidence_files(), 10);
    let (answer, notes) =
        request_with_notes(&mut session, "textDocument/references", on_save(&dir, true));
    assert_eq!(
        places(&answer["result"]),
        [
            ("widget.rb".to_string(), 2),
            ("z_sure.rb".to_string(), 3),
            ("a_guess.rb".to_string(), 2),
        ]
    );
    assert!(shown_messages(&notes).is_empty(), "{notes:?}");
    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// With a `partialResultToken` the locations arrive as `$/progress`, the
/// definition first, and the response carries none of them.
#[test]
fn references_stream_as_partial_results_when_the_client_asks() {
    let (dir, mut session) = many_file_session("refs-stream", &evidence_files(), 10);
    let mut params = on_save(&dir, true);
    params["partialResultToken"] = "refs-1".into();
    let (answer, notes) = request_with_notes(&mut session, "textDocument/references", params);
    assert_eq!(answer["result"], serde_json::json!([]), "{answer}");
    let streamed = batches(&notes, "refs-1");
    assert_eq!(
        streamed[0],
        [("widget.rb".to_string(), 2)],
        "the definition leads"
    );
    assert_eq!(
        streamed[1..].concat(),
        [("z_sure.rb".to_string(), 3), ("a_guess.rb".to_string(), 2)],
        "each batch is ordered by evidence: {streamed:?}"
    );
    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// Many files, each with a call nothing narrows.
fn crowd(files: usize, calls_per_file: usize) -> Vec<(String, String)> {
    let mut crowd = vec![(
        "app/widget.rb".to_string(),
        "class Widget\n  def save\n  end\nend\n".to_string(),
    )];
    let body: String = (0..calls_per_file)
        .map(|i| format!("  thing.save if thing.ready?({i})\n"))
        .collect();
    for n in 0..files {
        crowd.push((
            format!("lib/crowd/c{n:04}.rb"),
            format!("def call{n}(thing)\n{body}end\n"),
        ));
    }
    crowd
}

/// A stream stops reading at the limit, and says how far it got.
#[test]
fn a_streamed_answer_stops_at_the_limit_and_says_how_far_it_read() {
    let (dir, mut session) = many_file_session("refs-stream-cap", &crowd(300, 1), 5);
    let mut params = on_save(&dir, false);
    params["partialResultToken"] = "cap".into();
    let (answer, notes) = request_with_notes(&mut session, "textDocument/references", params);
    assert_eq!(answer["result"], serde_json::json!([]));
    assert_eq!(batches(&notes, "cap").concat().len(), 5);
    let said = shown_messages(&notes);
    assert_eq!(said.len(), 1, "{notes:?}");
    assert!(
        said[0].contains("5 references to `save`, from 128 of the 300 files"),
        "{}",
        said[0]
    );
    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// Cancelled mid-stream, the scan stops: no further batch, and the answer is
/// `RequestCancelled`, not a result.
#[test]
fn a_cancelled_stream_stops_sending() {
    let files = 128 * 6;
    let (dir, mut session) = many_file_session("refs-cancel", &crowd(files, 50), 10_000_000);
    let mut params = on_save(&dir, false);
    params["partialResultToken"] = "cancel".into();
    session.next_id += 1;
    let id = session.next_id;
    session.send(serde_json::json!({
        "jsonrpc": "2.0", "id": id, "method": "textDocument/references", "params": params
    }));
    // Withdraw it as soon as the first batch shows the scan is under way.
    let mut batches = 0;
    let answer = loop {
        let message = session.read();
        if message.get("id").and_then(|v| v.as_i64()) == Some(id) {
            break message;
        }
        if message["method"] == "$/progress" {
            batches += 1;
            if batches == 1 {
                session.notify("$/cancelRequest", serde_json::json!({ "id": id }));
            }
        }
    };
    assert_eq!(answer["error"]["code"], -32800, "{answer}");
    assert!(
        batches < files / 128,
        "{batches} batches: the scan ran on after the cancel"
    );
    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// A call whose receiver never resolved asks about every method of that name,
/// so it reads only as many files as the limit takes, and says it stopped.
#[test]
fn a_bare_name_reads_only_as_far_as_the_limit() {
    let (dir, mut session) = many_file_session("refs-bare", &crowd(300, 1), 5);
    let (answer, notes) = request_with_notes(
        &mut session,
        "textDocument/references",
        serde_json::json!({
            "textDocument": {"uri": uri_of(&dir, "lib/crowd/c0000.rb")},
            "position": {"line": 1, "character": 9},
            "context": {"includeDeclaration": false},
        }),
    );
    assert_eq!(places(&answer["result"]).len(), 5, "{answer}");
    let said = shown_messages(&notes);
    assert_eq!(said.len(), 1, "{notes:?}");
    assert!(
        said[0].contains("5 references to `save`, from the first 128 files")
            && said[0].contains("receiver's type is unknown")
            && said[0].contains("trekr --refs 'save'"),
        "{}",
        said[0]
    );
    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// `--usage` sees the editor too: each request is counted by operation, caller
/// and outcome once its answer is sent — the session opener apart from the
/// rest, and a method trekr does not serve as the error it was.
#[test]
fn requests_are_counted_by_operation_caller_and_outcome() {
    let (dir, db) = scratch("usage");
    let source = repo(&dir);
    let indexed = trekr()
        .args(["--index"])
        .current_dir(&dir)
        .env("TREKR_DB", &db)
        .output()
        .unwrap();
    assert!(indexed.status.success());

    let mut session = Session::start(&db, &dir);
    session.request(
        "initialize",
        serde_json::json!({
            "processId": null,
            "rootUri": format!("file://{}", dir.display()),
            "capabilities": {},
            "clientInfo": { "name": "Widget Editor" },
        }),
    );
    session.notify("initialized", serde_json::json!({}));
    session.notify(
        "textDocument/didOpen",
        serde_json::json!({"textDocument": {
            "uri": uri_of(&dir, "app.rb"), "languageId": "ruby", "version": 1, "text": source
        }}),
    );
    let at = |line: u32, character: u32| {
        serde_json::json!({
            "textDocument": {"uri": uri_of(&dir, "app.rb")},
            "position": {"line": line, "character": character},
        })
    };
    // `w.save`, twice: the first opens the session, the second is warm.
    session.request("textDocument/definition", at(7, 6));
    session.request("textDocument/definition", at(7, 6));
    // A blank line: nothing there.
    session.request("textDocument/hover", at(9, 0));
    session.request("textDocument/semanticTokens/full", at(0, 0));
    session.stop();

    let rows = usage_rows(&db);
    let def = |outcome: &str, cold: bool| {
        counted(
            &rows,
            &[
                ("feature", "definition".into()),
                ("outcome", outcome.into()),
                ("cold", cold.into()),
            ],
        )
    };
    assert_eq!(def("hit", true), 1, "{rows:?}");
    assert_eq!(def("hit", false), 1, "{rows:?}");
    assert_eq!(
        counted(
            &rows,
            &[("feature", "hover".into()), ("outcome", "empty".into())]
        ),
        1,
        "{rows:?}"
    );
    assert_eq!(
        counted(
            &rows,
            &[
                ("feature", "semanticTokens/full".into()),
                ("outcome", "error:unsupported".into())
            ]
        ),
        1,
        "{rows:?}"
    );
    assert_eq!(counted(&rows, &[("feature", "session".into())]), 1);
    // No agent in the environment, so the editor names the caller.
    assert!(
        rows.iter()
            .filter(|r| r["surface"] == "lsp")
            .all(|r| r["origin"] == "widgeteditor"),
        "{rows:?}"
    );
    // Counts, not content: nothing the request named is kept.
    let text = serde_json::to_string(&rows).unwrap();
    assert!(!text.contains("app.rb") && !text.contains(&dir.display().to_string()));

    let _ = fs::remove_dir_all(&dir);
}

/// A checkout of several files, committed and indexed, with `open` open in the
/// session.
fn files_session(label: &str, files: &[(&str, &str)], open: &str) -> (PathBuf, Session) {
    let (dir, db) = scratch(label);
    git(&dir, &["init", "-q"]);
    for (name, source) in files {
        let path = dir.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    }
    commit_all(&dir);
    let indexed = trekr()
        .args(["--index"])
        .current_dir(&dir)
        .env("TREKR_DB", &db)
        .output()
        .unwrap();
    assert!(indexed.status.success());
    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);
    let text = files.iter().find(|(name, _)| *name == open).unwrap().1;
    session.notify(
        "textDocument/didOpen",
        serde_json::json!({"textDocument": {
            "uri": uri_of(&dir, open), "languageId": "ruby", "version": 1, "text": text
        }}),
    );
    (dir, session)
}

/// `file:line` (1-based) of each location an answer holds; `[]` for null.
fn sites_in(answer: &serde_json::Value) -> Vec<String> {
    answer["result"]
        .as_array()
        .map(|locations| {
            locations
                .iter()
                .map(|l| {
                    let uri = l["uri"].as_str().unwrap();
                    let file = uri.rsplit('/').next().unwrap();
                    format!(
                        "{file}:{}",
                        l["range"]["start"]["line"].as_u64().unwrap() + 1
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

fn ask(
    session: &mut Session,
    dir: &Path,
    method: &str,
    file: &str,
    line: u32,
    character: u32,
) -> serde_json::Value {
    session.request(
        method,
        serde_json::json!({
            "textDocument": {"uri": uri_of(dir, file)},
            "position": {"line": line, "character": character},
            "context": {"includeDeclaration": true},
        }),
    )
}

#[test]
fn a_local_goes_to_the_assignments_its_value_can_come_from() {
    let source = concat!(
        "def total(items, rate:)\n", // 1
        "  sum = 0\n",               // 2
        "  if items.empty?\n",       // 3
        "    sum = 1\n",             // 4
        "  end\n",                   // 5
        "  sum * rate\n",            // 6
        "end\n",                     // 7
    );
    let (dir, mut session) = files_session("local", &[("app.rb", source)], "app.rb");
    let sum = ask(
        &mut session,
        &dir,
        "textDocument/definition",
        "app.rb",
        5,
        3,
    );
    assert_eq!(
        sites_in(&sum),
        ["app.rb:2", "app.rb:4"],
        "both branches reach"
    );
    let rate = ask(
        &mut session,
        &dir,
        "textDocument/definition",
        "app.rb",
        5,
        9,
    );
    assert_eq!(sites_in(&rate), ["app.rb:1"], "a keyword parameter");

    let hover = ask(&mut session, &dir, "textDocument/hover", "app.rb", 5, 3);
    assert_eq!(
        hover["result"]["contents"]["value"],
        "local `sum` · assigned at line 2 (and 1 more)"
    );
    let marks = ask(
        &mut session,
        &dir,
        "textDocument/documentHighlight",
        "app.rb",
        5,
        3,
    );
    let kinds: Vec<u64> = marks["result"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["kind"].as_u64().unwrap())
        .collect();
    assert_eq!(kinds, [3, 3, 2], "two writes, then the read");

    // An unsaved edit moves the answer.
    let edited = source.replace("  sum * rate\n", "  sum = 5\n  sum * rate\n");
    session.notify(
        "textDocument/didChange",
        serde_json::json!({
            "textDocument": {"uri": uri_of(&dir, "app.rb"), "version": 2},
            "contentChanges": [{"text": edited}],
        }),
    );
    let sum = ask(
        &mut session,
        &dir,
        "textDocument/definition",
        "app.rb",
        6,
        3,
    );
    assert_eq!(sites_in(&sum), ["app.rb:6"]);
    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

fn ivar_files() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "base.rb",
            concat!(
                "class Base\n",        // 1
                "  def setup\n",       // 2
                "    @color = :red\n", // 3
                "  end\n",             // 4
                "end\n",               // 5
            ),
        ),
        (
            "widget.rb",
            concat!(
                "class Widget < Base\n",                   // 1
                "  attr_accessor :size\n",                 // 2
                "  def initialize(name)\n",                // 3
                "    @name = name\n",                      // 4
                "  end\n",                                 // 5
                "  def label\n",                           // 6
                "    [@name, @color, @size, @greeting]\n", // 7
                "  end\n",                                 // 8
                "end\n",                                   // 9
            ),
        ),
        (
            "widget_rename.rb",
            concat!(
                "class Widget\n",     // 1
                "  def rename(to)\n", // 2
                "    @name = to\n",   // 3
                "  end\n",            // 4
                "end\n",              // 5
            ),
        ),
        (
            "greeting.rb",
            concat!(
                "module Greeting\n",      // 1
                "  def greet\n",          // 2
                "    @greeting\n",        // 3
                "  end\n",                // 4
                "end\n",                  // 5
                "class A\n",              // 6
                "  include Greeting\n",   // 7
                "  def initialize\n",     // 8
                "    @greeting = 'hi'\n", // 9
                "  end\n",                // 10
                "end\n",                  // 11
                "class B\n",              // 12
                "  include Greeting\n",   // 13
                "  def initialize\n",     // 14
                "    @greeting = 'yo'\n", // 15
                "  end\n",                // 16
                "end\n",                  // 17
            ),
        ),
    ]
}

#[test]
fn an_ivar_goes_to_where_its_class_sets_it() {
    let (dir, mut session) = files_session("ivar", &ivar_files(), "widget.rb");
    let mut definition = |character| {
        let answer = ask(
            &mut session,
            &dir,
            "textDocument/definition",
            "widget.rb",
            6,
            character,
        );
        sites_in(&answer)
    };
    assert_eq!(
        definition(6),
        ["widget.rb:4", "widget_rename.rb:3"],
        "`initialize` first, then the reopened class"
    );
    assert_eq!(definition(13), ["base.rb:3"], "set in the superclass");
    assert_eq!(definition(21), ["widget.rb:2"], "set by attr_accessor");
    assert_eq!(
        definition(28),
        Vec::<String>::new(),
        "not set by Widget or its ancestors: a guess is not an answer"
    );

    let hover = ask(&mut session, &dir, "textDocument/hover", "widget.rb", 6, 6);
    assert_eq!(
        hover["result"]["contents"]["value"],
        "ivar `@name` · set in `initialize` (and 1 more)"
    );
    let references = ask(
        &mut session,
        &dir,
        "textDocument/references",
        "widget.rb",
        6,
        6,
    );
    assert_eq!(
        sites_in(&references),
        ["widget.rb:4", "widget.rb:7", "widget_rename.rb:3"]
    );
    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// A module's ivar is set by whatever includes it — here, two classes. Which
/// one is the object at runtime is not knowable, so there is no answer.
#[test]
fn an_ivar_in_a_module_mixed_into_several_classes_goes_nowhere() {
    let (dir, mut session) = files_session("ivar-module", &ivar_files(), "greeting.rb");
    let answer = ask(
        &mut session,
        &dir,
        "textDocument/definition",
        "greeting.rb",
        2,
        6,
    );
    assert_eq!(sites_in(&answer), Vec::<String>::new());
    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// A session reading an early store (DEC-332) written by the index running
/// as `writer`: the store has `app.rb`, the early store `widget.rb` too, and
/// `app.rb`'s `Widget` is answered from the early store. The checkout, the
/// store, the early store's directory, and the session.
fn early_session(label: &str, writer: u32) -> (PathBuf, PathBuf, PathBuf, Session) {
    let (dir, db) = scratch(label);
    git(&dir, &["init", "-q"]);
    let source = "class Job\n  def run\n    Widget.new\n  end\nend\n";
    fs::write(dir.join("app.rb"), source).unwrap();
    let commit = |dir: &Path| {
        git(dir, &["add", "-A"]);
        git(
            dir,
            &[
                "-c",
                "user.email=t@e.st",
                "-c",
                "user.name=test",
                "commit",
                "-qm",
                "c",
            ],
        );
    };
    let index = |db: &Path| {
        trekr()
            .args(["--index"])
            .current_dir(&dir)
            .env("TREKR_DB", db)
            .output()
            .unwrap();
    };
    commit(&dir);
    index(&db);
    // The early store: what the index has written there, Widget's file among it.
    let mut name = db.file_name().unwrap().to_os_string();
    name.push(format!(".early-{writer}"));
    let beside = db.with_file_name(name);
    fs::create_dir_all(&beside).unwrap();
    let early = beside.join(db.file_name().unwrap());
    fs::write(dir.join("widget.rb"), "class Widget\nend\n").unwrap();
    commit(&dir);
    index(&early);
    let key = format!(
        "warming {}",
        fs::canonicalize(&dir).unwrap().to_string_lossy()
    );
    for store in [&db, &early] {
        rusqlite::Connection::open(store)
            .unwrap()
            .execute(
                "INSERT INTO meta (key, value) VALUES (?1, ?2)",
                [key.clone(), format!("{writer} 1 2")],
            )
            .unwrap();
    }

    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);
    session.notify(
        "textDocument/didOpen",
        serde_json::json!({"textDocument": {
            "uri": uri_of(&dir, "app.rb"), "languageId": "ruby", "version": 1, "text": source
        }}),
    );
    let answer = ask(
        &mut session,
        &dir,
        "textDocument/definition",
        "app.rb",
        2,
        6,
    );
    assert_eq!(sites_in(&answer).len(), 1, "from the early store: {answer}");
    (dir, db, beside, session)
}

/// A file the editor opens while the first index's bulk write holds the
/// store is answered from the early store that index writes (DEC-332), and
/// from the store again once the index removes the early store.
#[test]
fn an_early_store_answers_until_its_index_removes_it() {
    let mut writer = Command::new("sleep").arg("60").spawn().unwrap();
    let (dir, db, beside, mut session) = early_session("early", writer.id());

    // Once the server is idle, so nothing reads a tree before the loop's
    // next look at its store.
    let idle = std::time::Instant::now();
    while logged_events(&db, "warm").len() < 2 && idle.elapsed().as_secs() < 10 {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // As the index removes it: renamed aside, then emptied.
    let mut gone = beside.clone().into_os_string();
    gone.push(".gone");
    fs::rename(&beside, &gone).unwrap();
    // The server may still touch the WAL of the copy it has open, as the
    // index's own removal allows for; by name, nothing reaches it now.
    for _ in 0..3 {
        if fs::remove_dir_all(&gone).is_ok() {
            break;
        }
    }
    assert!(!Path::new(&gone).exists());
    // And ends: its mark cleared, its process gone.
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute("DELETE FROM meta WHERE key LIKE 'warming %'", [])
        .unwrap();
    writer.kill().unwrap();
    writer.wait().unwrap();
    // A message that reads no tree: the loop looks for a replaced store
    // before anything follows the early store back to the store.
    session.notify("$/setTrace", serde_json::json!({"value": "off"}));
    let answer = ask(
        &mut session,
        &dir,
        "textDocument/definition",
        "app.rb",
        2,
        6,
    );
    assert_eq!(
        sites_in(&answer),
        Vec::<String>::new(),
        "from the store: {answer}"
    );

    session.stop();
    assert_eq!(
        logged_events(&db, "store_reopened"),
        Vec::<serde_json::Value>::new(),
        "an early store's removal is the index's own cleanup, not a replaced store"
    );
    assert_eq!(
        logged_events(&db, "index_start"),
        Vec::<serde_json::Value>::new(),
        "nor is an index that ended one cut short"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// An early store whose index died is not read on: the server goes back to
/// the store, removes the early store, and finishes the index that was cut
/// short, as it does for one found cut short at start (DEC-320).
#[test]
fn an_early_store_whose_index_died_is_left_and_the_index_finished() {
    let mut writer = Command::new("sleep").arg("60").spawn().unwrap();
    let (dir, db, beside, mut session) = early_session("early-dead", writer.id());
    writer.kill().unwrap();
    writer.wait().unwrap();

    // Written and saved since: the store has it, the early store never will.
    let probe = "class Probe\nend\nProbe\n";
    fs::write(dir.join("probe.rb"), probe).unwrap();
    session.notify(
        "textDocument/didOpen",
        serde_json::json!({"textDocument": {
            "uri": uri_of(&dir, "probe.rb"), "languageId": "ruby", "version": 1, "text": probe
        }}),
    );
    session.notify(
        "textDocument/didSave",
        serde_json::json!({"textDocument": {"uri": uri_of(&dir, "probe.rb")}}),
    );
    let asked = std::time::Instant::now();
    let found = loop {
        let answer = ask(
            &mut session,
            &dir,
            "textDocument/definition",
            "probe.rb",
            2,
            1,
        );
        if !sites_in(&answer).is_empty() || asked.elapsed().as_secs() >= 10 {
            break sites_in(&answer);
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    assert_eq!(
        found.len(),
        1,
        "answered from the store, not the dead early store"
    );
    // Removed by a sweep: the server's own, or the resumed index's. Which
    // runs first is not a contract, so this waits for either.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while beside.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(!beside.exists(), "a dead index's early store is removed");

    session.stop();
    assert!(
        !logged_events(&db, "index_start").is_empty(),
        "the index that was cut short is run again"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// While a checkout's first index is still filling the store (DEC-320), a
/// hover says how much is read, a definition is told once (DEC-331),
/// completion is marked incomplete, and references rule nothing out — then,
/// once it ends, none of that.
/// A file opened while another process's first index fills the checkout —
/// an agent's query got there first — is handed to that index to read next,
/// as the server's own index would have been told it (DEC-512).
#[test]
fn a_file_opened_behind_anothers_first_index_is_handed_to_it() {
    let (dir, db) = scratch("handed");
    let source = repo(&dir);
    trekr()
        .args(["--index"])
        .current_dir(&dir)
        .env("TREKR_DB", &db)
        .output()
        .unwrap();
    let root = fs::canonicalize(&dir).unwrap();
    let mut other = Command::new("sleep").arg("30").spawn().unwrap();
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
            [
                format!("warming {}", root.to_string_lossy()),
                format!("{} 1 4", other.id()),
            ],
        )
        .unwrap();
    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);
    session.notify(
        "textDocument/didOpen",
        serde_json::json!({"textDocument": {
            "uri": uri_of(&dir, "app.rb"), "languageId": "ruby", "version": 1, "text": source
        }}),
    );
    // Answered after the open is handled.
    session.request(
        "textDocument/documentSymbol",
        serde_json::json!({"textDocument": {"uri": uri_of(&dir, "app.rb")}}),
    );
    let file = PathBuf::from(format!("{}.hints-{}", db.display(), other.id()));
    let said = fs::read_to_string(&file).unwrap_or_default();
    session.stop();
    other.kill().unwrap();
    other.wait().unwrap();
    assert_eq!(said.trim(), root.join("app.rb").display().to_string());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn answers_from_a_partial_index_say_so_and_rule_nothing_out() {
    let (dir, db) = scratch("warming");
    git(&dir, &["init", "-q"]);
    let source = concat!(
        "class Widget\n",       // 1
        "  def save\n",         // 2
        "  end\n",              // 3
        "end\n",                // 4
        "class Gadget\n",       // 5
        "  def save\n",         // 6
        "  end\n",              // 7
        "end\n",                // 8
        "class Job\n",          // 9
        "  def run\n",          // 10
        "    w = Widget.new\n", // 11
        "    w.save\n",         // 12
        "    g = Gadget.new\n", // 13
        "    g.save\n",         // 14
        "  end\n",              // 15
        "end\n",                // 16
    );
    fs::write(dir.join("app.rb"), source).unwrap();
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
            "init",
        ],
    );
    trekr()
        .args(["--index"])
        .current_dir(&dir)
        .env("TREKR_DB", &db)
        .output()
        .unwrap();
    let key = format!(
        "warming {}",
        fs::canonicalize(&dir).unwrap().to_string_lossy()
    );
    let store = rusqlite::Connection::open(&db).unwrap();
    store
        .execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)",
            [key.clone(), format!("{} 1 4", std::process::id())],
        )
        .unwrap();

    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);
    session.notify(
        "textDocument/didOpen",
        serde_json::json!({"textDocument": {
            "uri": uri_of(&dir, "app.rb"), "languageId": "ruby", "version": 1, "text": source
        }}),
    );
    let refs = |session: &mut Session| -> Vec<u64> {
        let answer = ask(session, &dir, "textDocument/references", "app.rb", 5, 6);
        answer["result"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["range"]["start"]["line"].as_u64().unwrap() + 1)
            .filter(|line| *line > 8)
            .collect()
    };

    // A definition has no field to say it, so the server says it once.
    let define = serde_json::json!({
        "textDocument": {"uri": uri_of(&dir, "app.rb")},
        "position": {"line": 11, "character": 7},
    });
    let (_, notes) = request_with_notes(&mut session, "textDocument/definition", define.clone());
    let said = shown_messages(&notes);
    assert_eq!(said.len(), 1, "{notes:?}");
    assert!(
        said[0].contains("still indexing this checkout (1 of 4 files read"),
        "{said:?}"
    );
    let (_, notes) = request_with_notes(&mut session, "textDocument/definition", define);
    assert!(shown_messages(&notes).is_empty(), "said once: {notes:?}");

    let hover = hover_at(&mut session, &dir, 11, 9);
    assert!(
        hover.contains("still indexing this checkout (1 of 4 files read"),
        "{hover}"
    );
    // The index moves on: a hover says where it is now, not where it was when
    // the tree answering was built — as the progress and another session do.
    store
        .execute(
            "UPDATE meta SET value = ?2 WHERE key = ?1",
            [key.clone(), format!("{} 3 4", std::process::id())],
        )
        .unwrap();
    let hover = hover_at(&mut session, &dir, 11, 9);
    assert!(hover.contains("(3 of 4 files read"), "{hover}");
    let listed = session.request(
        "textDocument/completion",
        serde_json::json!({
            "textDocument": {"uri": uri_of(&dir, "app.rb")},
            "position": {"line": 11, "character": 6},
            "context": {"triggerKind": 1},
        }),
    );
    assert_eq!(listed["result"]["isIncomplete"], true, "{listed}");
    assert_eq!(
        refs(&mut session),
        vec![14, 12],
        "Widget's call is not ruled out against a partial index — listed after Gadget's"
    );

    store
        .execute("DELETE FROM meta WHERE key = ?1", [key])
        .unwrap();
    let hover = hover_at(&mut session, &dir, 11, 9);
    assert!(!hover.contains("still indexing"), "{hover}");
    assert_eq!(refs(&mut session), vec![14]);

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// An example group's `let` is read by Ruby's lookup at runtime: Find
/// References on one lists a shared group's body that reads it, in another
/// file, and Go to Definition on that read lands on each includer's `let`
/// (DEC-490).
#[test]
fn a_let_and_a_shared_groups_read_of_it_find_each_other() {
    let (dir, db) = scratch("let-refs");
    git(&dir, &["init", "-q"]);
    let shared = concat!(
        "RSpec.shared_examples \"a named thing\" do\n", // 1
        "  it { expect(name).to eq(\"x\") }\n",         // 2
        "end\n",                                        // 3
    );
    let alpha = concat!(
        "RSpec.describe \"Alpha\" do\n",         // 1
        "  let(:name) { \"x\" }\n",              // 2
        "\n",                                    // 3
        "  it_behaves_like \"a named thing\"\n", // 4
        "  it { expect(name).to be }\n",         // 5
        "end\n",                                 // 6
    );
    let beta = concat!(
        "RSpec.describe \"Beta\" do\n",           // 1
        "  let(:name) { \"x\" }\n",               // 2
        "\n",                                     // 3
        "  include_examples \"a named thing\"\n", // 4
        "end\n",                                  // 5
    );
    fs::create_dir_all(dir.join("spec/support")).unwrap();
    fs::write(dir.join("spec/support/shared.rb"), shared).unwrap();
    fs::write(dir.join("spec/alpha_spec.rb"), alpha).unwrap();
    fs::write(dir.join("spec/beta_spec.rb"), beta).unwrap();
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
            "init",
        ],
    );
    trekr()
        .args(["--index"])
        .current_dir(&dir)
        .env("TREKR_DB", &db)
        .output()
        .unwrap();

    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);
    let places = |answer: &serde_json::Value| -> Vec<(String, u64)> {
        let mut found: Vec<(String, u64)> = answer["result"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| {
                let uri = l["uri"].as_str().unwrap();
                let file = uri.rsplit('/').next().unwrap().to_string();
                (file, l["range"]["start"]["line"].as_u64().unwrap() + 1)
            })
            .collect();
        found.sort();
        found
    };

    // On Alpha's `let(:name)`: its own example, and the shared body's read.
    let answer = session.request(
        "textDocument/references",
        serde_json::json!({
            "textDocument": {"uri": uri_of(&dir, "spec/alpha_spec.rb")},
            "position": {"line": 1, "character": 8},
            "context": {"includeDeclaration": false},
        }),
    );
    assert_eq!(
        places(&answer),
        vec![
            ("alpha_spec.rb".to_string(), 5),
            ("shared.rb".to_string(), 2)
        ],
        "the example's read and the shared group's, not Beta's"
    );

    // On the shared body's `name`: each includer's `let`.
    let answer = session.request(
        "textDocument/definition",
        serde_json::json!({
            "textDocument": {"uri": uri_of(&dir, "spec/support/shared.rb")},
            "position": {"line": 1, "character": 16},
        }),
    );
    assert_eq!(
        places(&answer),
        vec![
            ("alpha_spec.rb".to_string(), 2),
            ("beta_spec.rb".to_string(), 2)
        ],
        "both includers' `let(:name)`"
    );

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// An ERB template is answered in the editor as a Ruby file is: definition
/// and hover on a helper it calls, the template among the helper's
/// references, and no syntax error for markup — each at the template's own
/// position, past a multibyte character in the markup (DEC-520, DEC-521).
#[test]
fn a_document_that_is_neither_ruby_nor_a_template_gets_no_diagnostics() {
    let (dir, db) = scratch("not-ruby");
    repo(&dir);
    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);
    // An ERB language id on a file not named `.erb`: its markup is no Ruby.
    session.notify(
        "textDocument/didOpen",
        serde_json::json!({"textDocument": {
            "uri": uri_of(&dir, "app/views/notes.html"), "languageId": "html.erb",
            "version": 1, "text": "<p>hi <%= 1 %></p>\n"
        }}),
    );
    session.notify(
        "textDocument/didOpen",
        serde_json::json!({"textDocument": {
            "uri": uri_of(&dir, "broken.rb"), "languageId": "ruby",
            "version": 1, "text": "def x(\n"
        }}),
    );
    let published = session.read();
    assert_eq!(published["method"], "textDocument/publishDiagnostics");
    assert!(
        published["params"]["uri"]
            .as_str()
            .is_some_and(|uri| uri.ends_with("broken.rb")),
        "nothing is published for the HTML: {published}"
    );
    session.stop();
}

#[test]
fn an_erb_template_is_answered_at_its_own_positions() {
    let (dir, db) = scratch("erb");
    git(&dir, &["init", "-q"]);
    fs::create_dir_all(dir.join("app/helpers")).unwrap();
    fs::create_dir_all(dir.join("app/views/widgets")).unwrap();
    let helper = concat!(
        "module WidgetsHelper\n", // 1
        "  # The widget's badge.\n",
        "  def badge(widget)\n", // 3
        "    widget\n",
        "  end\n",
        "end\n",
    );
    let template = concat!(
        "<h1>Widgets</h1>\n",
        "<p>é — <%= badge(1) %></p>\n",
        "<% if true %><%= yield %><% end %>\n",
        "<%= render \"row\" %>\n",
        "<h1><%= @title %></h1>\n", // 5
        "<%= badg %>\n",
    );
    fs::write(dir.join("app/helpers/widgets_helper.rb"), helper).unwrap();
    fs::write(dir.join("app/views/widgets/_row.html.erb"), "<li></li>\n").unwrap();
    fs::create_dir_all(dir.join("app/controllers")).unwrap();
    fs::write(
        dir.join("app/controllers/widgets_controller.rb"),
        "class WidgetsController\n  def show\n    @title = \"w\"\n  end\nend\n",
    )
    .unwrap();
    fs::write(dir.join("app/views/widgets/show.html.erb"), template).unwrap();
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
            "init",
        ],
    );
    trekr()
        .args(["--index"])
        .current_dir(&dir)
        .env("TREKR_DB", &db)
        .output()
        .unwrap();

    let view = "app/views/widgets/show.html.erb";
    let mut session = Session::start(&db, &dir);
    session.initialize(&dir);
    session.notify(
        "textDocument/didOpen",
        serde_json::json!({"textDocument": {
            "uri": uri_of(&dir, view), "languageId": "erb", "version": 1, "text": template
        }}),
    );
    let published = session.read();
    assert_eq!(published["method"], "textDocument/publishDiagnostics");
    assert_eq!(
        published["params"]["diagnostics"],
        serde_json::json!([]),
        "markup and a layout's `yield` are no syntax errors"
    );

    // `badge` is at UTF-16 character 11 of line 2, byte 14: `é` and `—`
    // are one unit each, and two and three bytes.
    let at = serde_json::json!({
        "textDocument": {"uri": uri_of(&dir, view)},
        "position": {"line": 1, "character": 11},
    });
    let answer = session.request("textDocument/definition", at.clone());
    let locations = answer["result"].as_array().expect("an array of locations");
    assert_eq!(locations.len(), 1, "{answer}");
    assert!(
        locations[0]["uri"]
            .as_str()
            .unwrap()
            .ends_with("widgets_helper.rb")
    );
    assert_eq!(locations[0]["range"]["start"]["line"], 2);

    // A `render`'s name opens the partial it renders (DEC-524).
    let answer = session.request(
        "textDocument/definition",
        serde_json::json!({
            "textDocument": {"uri": uri_of(&dir, view)},
            "position": {"line": 3, "character": 13},
        }),
    );
    let locations = answer["result"].as_array().expect("an array of locations");
    assert_eq!(locations.len(), 1, "{answer}");
    assert!(
        locations[0]["uri"]
            .as_str()
            .unwrap()
            .ends_with("widgets/_row.html.erb")
    );

    let hover = session.request("textDocument/hover", at);
    let text = hover["result"]["contents"]["value"]
        .as_str()
        .expect("markdown");
    assert!(text.contains("badge"), "{text}");

    let answer = session.request(
        "textDocument/references",
        serde_json::json!({
            "textDocument": {"uri": uri_of(&dir, "app/helpers/widgets_helper.rb")},
            "position": {"line": 2, "character": 6},
            "context": {"includeDeclaration": false},
        }),
    );
    let found: Vec<(String, u64, u64)> = answer["result"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| {
            let uri = l["uri"].as_str().unwrap();
            (
                uri.rsplit('/').next().unwrap().to_string(),
                l["range"]["start"]["line"].as_u64().unwrap(),
                l["range"]["start"]["character"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(found, vec![("show.html.erb".to_string(), 1, 11)]);

    // A bare `@title` is set by the controller that renders the template
    // (DEC-522).
    let answer = session.request(
        "textDocument/definition",
        serde_json::json!({
            "textDocument": {"uri": uri_of(&dir, view)},
            "position": {"line": 4, "character": 9},
        }),
    );
    let locations = answer["result"].as_array().expect("an array of locations");
    assert_eq!(locations.len(), 1, "{answer}");
    assert!(
        locations[0]["uri"]
            .as_str()
            .unwrap()
            .ends_with("widgets_controller.rb")
    );
    assert_eq!(locations[0]["range"]["start"]["line"], 2);

    // A bare word in a tag completes from the view context: the helpers.
    let answer = listed(
        &mut session,
        serde_json::json!({
            "textDocument": {"uri": uri_of(&dir, view)},
            "position": {"line": 5, "character": 8},
        }),
    );
    let labels: Vec<&str> = answer["result"]["items"]
        .as_array()
        .or(answer["result"].as_array())
        .expect("completion items")
        .iter()
        .filter_map(|item| item["label"].as_str())
        .collect();
    assert!(labels.contains(&"badge"), "{labels:?}");

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// A template's answers that read another file — a partial's local, from the
/// `render` that hands it; a view's `@ivar`, from its controller — read the
/// editor's unsaved copy of that file, as every other answer does. They read
/// the disk, and pointed at lines the buffer had moved.
#[test]
fn a_templates_answers_read_the_open_buffers_of_the_files_they_follow() {
    let controller = concat!(
        "class WidgetsController\n", // 1
        "  def list\n",
        "  end\n",
        "\n",
        "  def show\n", // 5
        "    @count = 3\n",
        "  end\n",
        "end\n",
    );
    let files = [
        ("app/controllers/widgets_controller.rb", controller),
        (
            "app/views/widgets/list.html.erb",
            "<%= render \"shared/card\", card: 1 %>\n",
        ),
        ("app/views/shared/_card.html.erb", "<%= card %>\n"),
        ("app/views/widgets/show.html.erb", "<%= @count %>\n"),
    ];
    let (dir, mut session) = files_session("buffers", &files, "app/views/shared/_card.html.erb");
    let open = |session: &mut Session, file: &str, text: &str| {
        session.notify(
            "textDocument/didOpen",
            serde_json::json!({"textDocument": {
                "uri": uri_of(&dir, file), "languageId": "erb", "version": 2, "text": text
            }}),
        );
    };
    let card = "app/views/shared/_card.html.erb";
    let definition = |session: &mut Session, file: &str, line, character| {
        sites_in(&ask(
            session,
            &dir,
            "textDocument/definition",
            file,
            line,
            character,
        ))
    };
    assert_eq!(definition(&mut session, card, 0, 4), ["list.html.erb:1"]);

    // The render moves down three lines, and hands a local only the buffer has.
    open(
        &mut session,
        "app/views/widgets/list.html.erb",
        "\n\n\n<%= render \"shared/card\", card: 1, extra: 2 %>\n",
    );
    open(&mut session, card, "<%= card %> <%= extra %>\n");
    assert_eq!(definition(&mut session, card, 0, 4), ["list.html.erb:4"]);
    assert_eq!(definition(&mut session, card, 0, 16), ["list.html.erb:4"]);

    // An `@ivar` only the controller's buffer sets.
    open(
        &mut session,
        "app/controllers/widgets_controller.rb",
        &controller.replace("    @count = 3\n", "    @count = 3\n    @fresh = 4\n"),
    );
    let show = "app/views/widgets/show.html.erb";
    open(&mut session, show, "<%= @count %>\n<%= @fresh %>\n");
    assert_eq!(
        definition(&mut session, show, 1, 5),
        ["widgets_controller.rb:7"]
    );
    session.stop();
    let _ = fs::remove_dir_all(&dir);
}
