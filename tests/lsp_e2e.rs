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

/// A server whose binary has been replaced must retire itself.
///
/// Refreshing the installed binary left the running server answering with the
/// old build until somebody remembered to kill it — silent staleness, and the
/// editor owns the lifecycle, so exiting cleanly *is* the fix: the client
/// spawns the new build on its next request.
#[test]
fn serve_retires_when_its_binary_is_replaced() {
    let (dir, db) = scratch("retire");
    repo(&dir);

    // Its own copy, so replacing it cannot disturb the other tests.
    let binary = dir.join("trekr-under-test");
    fs::copy(env!("CARGO_BIN_EXE_trekr"), &binary).unwrap();
    let spawn = || {
        Command::new(&binary)
            .arg("--lsp")
            .current_dir(&dir)
            .env("TREKR_DB", &db)
            .env("TREKR_LOG", log_path(&db))
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
    let mut session = Session {
        stdin: Some(child.stdin.take().unwrap()),
        stdout: BufReader::new(child.stdout.take().unwrap()),
        child,
        next_id: 0,
    };
    session.initialize(&dir);

    // Still current: the server answers and stays up.
    let before = session.request(
        "textDocument/documentSymbol",
        serde_json::json!({"textDocument": {"uri": uri_of(&dir, "app.rb")}}),
    );
    assert!(before["result"].is_array(), "answers while current");

    // Replace it with a newer file, the way `rm && cp` does. Written rather
    // than `fs::copy`d: on macOS that preserves the *source's* mtime, so the
    // replacement would look no newer than what it replaced.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    fs::remove_file(&binary).unwrap();
    fs::write(&binary, fs::read(env!("CARGO_BIN_EXE_trekr")).unwrap()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    }

    // It answers this one, then goes.
    let after = session.request(
        "textDocument/documentSymbol",
        serde_json::json!({"textDocument": {"uri": uri_of(&dir, "app.rb")}}),
    );
    assert!(
        after["result"].is_array(),
        "the in-flight request is answered"
    );

    // Wait **without** closing stdin. Closing it makes any server exit, so a
    // test that closes first cannot tell retirement from ordinary shutdown —
    // and this one did not, which is how a retirement that logged its event
    // and then hung forever passed for two sessions. An editor holds stdin
    // open, so this is also the real situation.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let status = loop {
        match session.child.try_wait().expect("poll the child") {
            Some(status) => break status,
            None if std::time::Instant::now() > deadline => {
                let _ = session.child.kill();
                panic!("the server never exited: it logged retirement and kept running");
            }
            None => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    };
    assert!(status.success(), "and cleanly: {status:?}");
    assert!(
        log_lines(&db).iter().any(|l| l["event"] == "retire"),
        "and says why, so --usage can count restarts"
    );
    session.stdin.take();

    let _ = fs::remove_dir_all(&dir);
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
    Command::new(env!("CARGO_BIN_EXE_trekr"))
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
    assert_eq!(seen, ["create", "begin", "end"]);

    let answer = definition_eventually(&mut session, &uri_of(&dir, "app.rb"), 5, 3);
    assert_eq!(
        answer[0]["range"]["start"]["line"], 1,
        "Widget#save, from the new index"
    );

    session.stop();
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
    Command::new(env!("CARGO_BIN_EXE_trekr"))
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
    let (dir, _db, mut session) = indexed_session("complete-dot", SHOP);
    session.read();

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
    let (dir, _db, mut session) = indexed_session("complete-scope", SHOP);
    session.read();
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
    let (dir, _db, mut session) = indexed_session("complete-bare", SHOP);
    session.read();
    let source = SHOP.replace("    count = 1\n", "    count = 1\n    \n");
    let (labels, _) = complete(&mut session, &dir, &source, 15, 2);
    let at = |name: &str| labels.iter().position(|l| l == name);
    assert!(at("count").is_some() && at("level").is_some(), "{labels:?}");
    assert!(at("count") < at("polish"), "locals before methods");
    assert!(at("polish") < at("save"), "own before inherited");
    assert!(at("secret").is_some(), "private is callable on self");

    session.stop();
    let _ = fs::remove_dir_all(&dir);
}

/// An untyped receiver gets a short list of names that fit the prefix,
/// marked incomplete — and nothing at all before a prefix is typed.
#[test]
fn completion_on_an_untyped_receiver_is_short_and_disclosed() {
    let (dir, _db, mut session) = indexed_session("complete-untyped", SHOP);
    session.read();
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

    session.stop();
    let _ = fs::remove_file(&link);
    let _ = fs::remove_dir_all(&dir);
}
