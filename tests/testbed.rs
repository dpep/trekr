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
//! ```
//!
//! Keys for `def` are fields of `--def --json`: `status`, `owner`, `via`
//! (`resolved_via`), `name`, `confidence`, `candidates` (a count), `site`
//! (`path:line`, matched on the path's tail), and `candidate1` (the top
//! candidate's owner). Keys for `refs` are the `counts` object. Unknown keys
//! fail loudly rather than passing silently — a typo in an expectation is a
//! test that proves nothing.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
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
    fs::create_dir_all(&dir).unwrap();

    // Everything but the expectations file is source.
    for entry in fs::read_dir(case).unwrap().flatten() {
        let name = entry.file_name();
        if name == "expected" || name == "README.md" {
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
    let indexed = Command::new(env!("CARGO_BIN_EXE_trekr"))
        .args(["--index"])
        .current_dir(&dir)
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

fn trekr(db: &Path, dir: &Path, args: &[&str]) -> (serde_json::Value, i32) {
    let out = Command::new(env!("CARGO_BIN_EXE_trekr"))
        .args(args)
        .current_dir(dir)
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
            "site" => {
                let site = &answer["sites"][0];
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
        // site matches on its tail. Everything else is exact.
        let matched = if key == "site" {
            got.ends_with(&want)
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
        let got = answer["counts"][&key]
            .as_i64()
            .map(|n| n.to_string())
            .unwrap_or_else(|| "<none>".into());
        if got != want {
            failures.push(format!(
                "{case}: {line}\n      counts.{key}: expected `{want}`, got `{got}`"
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
                "refs" => {
                    let (answer, _) = trekr(&db, &dir, &["--refs", target, "--json"]);
                    check_refs(&label, line, &answer, &mut failures);
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
    }

    assert!(
        failures.is_empty(),
        "{} of {checks} testbed checks failed across {} cases:\n\n  {}\n",
        failures.len(),
        cases.len(),
        failures.join("\n\n  ")
    );
}
