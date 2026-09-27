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

/// A command with git's repository-locating variables cleared. A gate run
/// under `git rebase --exec` exports `GIT_DIR`, and a fixture's `git init`
/// then writes into the real repository — as does any git the binary under
/// test runs.
fn isolated(program: &str) -> Command {
    let mut command = Command::new(program);
    command
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    command
}

fn trekr() -> Command {
    isolated(env!("CARGO_BIN_EXE_trekr"))
}

fn git(dir: &Path, args: &[&str]) {
    let out = isolated("git")
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
        let mut child = trekr()
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
    assert!(uri.ends_with("core.rb"), "lands in the core stub: {uri}");
    let path = uri.strip_prefix("file://").unwrap();
    assert!(
        fs::read_to_string(path).unwrap().contains("def puts"),
        "and the file is really there and really readable"
    );

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
        text.starts_with("```ruby\ndef Widget#save(force = false, *rest, key: nil)\n```"),
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
            "```ruby\ndef Widget#plain\n```\n\nDefined in [`app.rb:18`](file://{}/app.rb#L18)",
            std::fs::canonicalize(&dir).unwrap().display()
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
    assert!(text.contains("def Widget#save(force"), "{text}");
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
    assert!(text.contains("def Gadget#save"), "the pick: {text}");
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
        text.starts_with("```ruby\ndef Shelf.stack(*items)\n```"),
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
    assert!(text.contains("def Widget#save(force"), "{text}");
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
    let list = session.request(
        "textDocument/completion",
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
        item["detail"], "def Widget#save(force = false, *rest, key: nil)",
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

/// Start a server from `binary` — the test's own copy, at a path the test can
/// then replace, which is the whole subject of the hot-reload tests.
fn start_from(binary: &Path, db: &Path, dir: &Path) -> Session {
    let spawn = || {
        isolated(binary.to_str().unwrap())
            .arg("--lsp")
            .current_dir(dir)
            .env("TREKR_DB", db)
            .env("TREKR_LOG", log_path(db))
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
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&bin);
}

/// `brew upgrade` does not touch the running file: it installs into a new
/// Cellar directory and re-points the symlink the editor launched. The link is
/// what is watched, so the relink is the upgrade.
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
    let (dir, _db, mut session) = indexed_session("complete-cap", &source);
    session.read();
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

/// A checkout requiring its own files and a vendored gem's. `shelf` is both
/// the checkout's `lib/shelf.rb` and the gem's, so it has two answers.
fn require_repo(dir: &Path) {
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
        .env("TREKR_DB", dir.with_extension("db"))
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
    let (dir, _) = scratch(label);
    require_repo(&dir);
    let mut session = Session::start(&dir.with_extension("db"), &dir);
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
