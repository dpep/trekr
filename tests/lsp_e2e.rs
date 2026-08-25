//! End-to-end: a scripted LSP session against the built binary over stdio.
//!
//! Same isolation as the CLI suite — a temp git repo and its own database — so
//! this runs in CI without touching anything real.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

fn scratch(label: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir();
    let dir = base.join(format!("trekr-lsp-{}-{label}", std::process::id()));
    let db = base.join(format!("trekr-lsp-{}-{label}.db", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    for suffix in ["", "-wal", "-shm"] {
        let _ = fs::remove_file(format!("{}{suffix}", db.display()));
    }
    fs::create_dir_all(&dir).unwrap();
    (dir, db)
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git");
    assert!(out.status.success(), "git {args:?}");
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
        let mut child = Command::new(env!("CARGO_BIN_EXE_trekr"))
            .arg("--serve")
            .current_dir(dir)
            .env("TREKR_DB", db)
            // Its own log, or every test in this file appends to one shared
            // file beside the temp dir and they read each other's lines.
            .env("TREKR_LOG", log_path(db))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start trekr --serve");
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
        let uri = format!("file://{}", dir.display());
        let result = self.request(
            "initialize",
            serde_json::json!({
                "processId": null,
                "rootUri": uri,
                "capabilities": {},
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
    ] {
        assert!(!caps[provider].is_null(), "{provider} is announced");
    }
    // Never: these are not what an agent uses, and claiming them would invite
    // an editor to route work here that this engine has no business doing.
    for absent in [
        "completionProvider",
        "renameProvider",
        "documentFormattingProvider",
        "semanticTokensProvider",
    ] {
        assert!(caps[absent].is_null(), "{absent} must not be announced");
    }
    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn go_to_definition_answers_from_the_resolved_receiver() {
    let (dir, db) = scratch("def");
    let source = repo(&dir);
    // The index has to exist; the server reads it, it does not build it.
    let indexed = Command::new(env!("CARGO_BIN_EXE_trekr"))
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
    Command::new(env!("CARGO_BIN_EXE_trekr"))
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
    Command::new(env!("CARGO_BIN_EXE_trekr"))
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
    assert!(text.contains("Residue"), "hover says it guessed: {text}");
    assert!(text.contains("confidence: 0.00"), "and how much: {text}");

    session.stop();
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
    Command::new(env!("CARGO_BIN_EXE_trekr"))
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
    assert!(uri.ends_with("core.rb"), "lands in the core stub: {uri}");
    let path = uri.strip_prefix("file://").unwrap();
    assert!(
        fs::read_to_string(path).unwrap().contains("def puts"),
        "and the file is really there and really readable"
    );

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn hover_discloses_the_rung_and_the_confidence() {
    let (dir, db) = scratch("hover");
    let source = repo(&dir);
    Command::new(env!("CARGO_BIN_EXE_trekr"))
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
        "textDocument/hover",
        serde_json::json!({
            "textDocument": {"uri": uri_of(&dir, "app.rb")},
            "position": {"line": 7, "character": 6},
        }),
    );
    let text = answer["result"]["contents"]["value"]
        .as_str()
        .expect("markdown");
    // LSP has no confidence field, so hover is where the disclosure lives.
    assert!(text.contains("local:new"), "names the rung: {text}");
    assert!(text.contains("Widget"), "names the receiver's type: {text}");
    assert!(
        text.contains("confidence"),
        "and how sure that makes it: {text}"
    );

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
    assert_eq!(requests[0]["answered"], 2, "Fresh and added");
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
    let names: Vec<&str> = answer["result"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Fresh", "added"]);

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
    Command::new(env!("CARGO_BIN_EXE_trekr"))
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
    let names: Vec<&str> = symbols["result"]
        .as_array()
        .expect("an outline, not null")
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Widget", "save", "Job", "run"]);

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
    let loose = std::env::temp_dir().join(format!("trekr-lsp-{}-loose", std::process::id()));
    let _ = fs::remove_dir_all(&loose);
    fs::create_dir_all(&loose).unwrap();
    fs::write(loose.join("app.rb"), "module Loose\n  def go\n  end\nend\n").unwrap();

    let mut session = Session::start(&db, &root);
    session.initialize(&root);
    let answer = session.request(
        "textDocument/documentSymbol",
        serde_json::json!({"textDocument": {"uri": uri_of(&loose, "app.rb")}}),
    );
    let names: Vec<&str> = answer["result"]
        .as_array()
        .expect("an outline, not null")
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Loose", "go"]);

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
        Command::new(env!("CARGO_BIN_EXE_trekr"))
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
