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
//! (`path:line`, matched on the path's tail), and `candidate1` (the top
//! candidate's owner). Keys for `refs` are the `counts` object, `status` and
//! `resolves_to`. Unknown keys fail loudly rather than passing silently — a
//! typo in an expectation is a test that proves nothing.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(dir: &Path, args: &[&str]) {
    // Run from a git hook or `rebase --exec`, these are set to the outer repo,
    // and `init`/`commit` here would write into it instead of the scratch dir.
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("run git");
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// Stage one case as a real checkout with its own database.
fn stage(case: &Path, label: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir();
    let dir = base.join(format!("trekr-bed-{}-{label}", std::process::id()));
    let db = base.join(format!("trekr-bed-{}-{label}.db", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    for suffix in ["", "-wal", "-shm"] {
        let _ = fs::remove_file(format!("{}{suffix}", db.display()));
    }
    let _ = fs::remove_dir_all(db.with_extension("trees"));
    let _ = fs::remove_dir_all(db.with_extension("core"));
    fs::create_dir_all(&dir).unwrap();

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
        .args(["--index"])
        .env("TREKR_DB", &db)
        .output()
        .expect("index the case");
    assert!(
        indexed.status.success(),
        "indexing {label} failed: {}",
        String::from_utf8_lossy(&indexed.stderr)
    );
    (dir, db)
}

/// Where a staged case's Ruby is installed, when it has one.
fn home_of(dir: &Path) -> PathBuf {
    dir.with_extension("home")
}

/// The binary, run in a staged case: with its own home when it stages a Ruby.
fn trekr_in(dir: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_trekr"));
    command.current_dir(dir);
    if home_of(dir).is_dir() {
        command.env("HOME", home_of(dir));
    }
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
    use std::io::{BufRead, BufReader, Write};
    let (file, line, col) = {
        let mut bits = target.rsplitn(3, ':');
        let col: u32 = bits.next().unwrap_or("1").parse().unwrap_or(1);
        let line: u32 = bits.next().unwrap_or("1").parse().unwrap_or(1);
        (bits.next().unwrap_or_default().to_string(), line, col)
    };
    let path = dir.join(&file);
    let uri = format!("file://{}", path.display());
    let text = fs::read_to_string(&path).unwrap_or_default();

    let mut child = trekr_in(dir)
        .arg("--lsp")
        .env("TREKR_DB", db)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("lsp");
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    // A pipe read has no timeout, and a server that never answers would hang
    // CI rather than fail it — which is exactly how the retirement bug reached
    // main, passing on macOS and parking forever on Linux. Bound the wait so
    // the worst case is a red test.
    let watchdog = child.id();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(30));
        // Harmless if it already exited: the id is reaped by `wait` below.
        let _ = Command::new("kill")
            .arg("-9")
            .arg(watchdog.to_string())
            .status();
    });

    let mut send = |value: serde_json::Value| {
        let body = serde_json::to_vec(&value).unwrap();
        let _ = stdin.write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes());
        let _ = stdin.write_all(&body);
        let _ = stdin.flush();
    };
    send(
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize",
        "params":{"rootUri": format!("file://{}", dir.display()), "capabilities":{}}}),
    );
    send(serde_json::json!({"jsonrpc":"2.0","method":"initialized","params":{}}));
    send(
        serde_json::json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{
        "textDocument":{"uri":uri,"languageId":"ruby","version":1,"text":text}}}),
    );
    send(
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"textDocument/hover","params":{
        "textDocument":{"uri":uri},
        "position":{"line": line - 1, "character": col - 1}}}),
    );

    let mut found = String::new();
    for _ in 0..16 {
        let mut length = 0usize;
        loop {
            let mut header = String::new();
            if stdout.read_line(&mut header).unwrap_or(0) == 0 {
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
        if std::io::Read::read_exact(&mut stdout, &mut body).is_err() {
            break;
        }
        let message: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
        if message["id"] == serde_json::json!(2) {
            found = message["result"]["contents"]["value"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            break;
        }
    }
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    found
}

fn trekr(db: &Path, dir: &Path, args: &[&str]) -> (serde_json::Value, i32) {
    let out = trekr_in(dir)
        .args(args)
        .env("TREKR_DB", db)
        .output()
        .expect("run trekr");
    let code = out.status.code().unwrap_or(-1);
    let parsed = serde_json::from_slice(&out.stdout).unwrap_or(serde_json::Value::Null);
    (parsed, code)
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
            "site" => {
                let site = &answer["definition"][0];
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
        let matched = if key == "site" {
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

fn check_refs(case: &str, line: &str, answer: &serde_json::Value, failures: &mut Vec<String>) {
    for (key, want) in pairs(line) {
        let got = match key.as_str() {
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
        // it has none.
        let (want, caveat) = match want.split_once('~') {
            Some((tier, word)) => (tier.to_string(), Some(word.to_string())),
            None => (want.clone(), None),
        };
        // A capitalised name with no `#` is a class, module or constant, by
        // its whole name (`Admin::Widget=unreferenced`).
        let constant =
            !method.contains('#') && method.starts_with(|c: char| c.is_ascii_uppercase());
        let row = rows.iter().find(|row| {
            if constant {
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
            Some(word) => said.contains(word),
        };
        if !fits {
            failures.push(format!(
                "{case}: {line}\n      {method}: expected a caveat with `{}`, got `{said}`",
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
        .collect();
    cases.sort();
    assert!(!cases.is_empty(), "no cases in {}", root.display());

    let mut failures: Vec<String> = Vec::new();
    let mut checks = 0usize;
    for case in &cases {
        let label = case.file_name().unwrap().to_string_lossy().into_owned();
        let expectations = fs::read_to_string(case.join("expected"))
            .unwrap_or_else(|_| panic!("{label} has no `expected` file"));
        let (dir, db) = stage(case, &label);

        for line in expectations.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            checks += 1;
            let Some((verb, rest)) = line.split_once(char::is_whitespace) else {
                failures.push(format!("{label}: cannot parse `{line}`"));
                continue;
            };
            let target = rest.split_whitespace().next().unwrap_or_default();
            match verb {
                "def" => {
                    let (answer, code) = trekr(&db, &dir, &["--def", target, "--json"]);
                    check_def(&label, line, &answer, code, &mut failures);
                }
                "card" => {
                    let (answer, code) = trekr(&db, &dir, &[target, "--json"]);
                    check_def(&label, line, &answer, code, &mut failures);
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
                    let (answer, _) = trekr(&db, &dir, &["--refs", target, "--json"]);
                    check_refs(&label, line, &answer, &mut failures);
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
                            failures
                                .push(format!("{label}: {line}\n      unknown key {assertion}"));
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
                    check_dead(&label, line, &answer, &mut failures);
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
                }
                other => failures.push(format!("{label}: unknown verb `{other}`")),
            }
        }
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(home_of(&dir));
    }

    assert!(
        failures.is_empty(),
        "{} of {checks} testbed checks failed across {} cases:\n\n  {}\n",
        failures.len(),
        cases.len(),
        failures.join("\n\n  ")
    );
}
