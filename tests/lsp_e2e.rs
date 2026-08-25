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
