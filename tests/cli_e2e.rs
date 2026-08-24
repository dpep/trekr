//! End-to-end: the built binary, an isolated database, a real git repo.
//!
//! Behavior gets checked here rather than by hand-running `trekr`, so a
//! regression fails CI instead of being noticed later.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A scratch repo and database for one test, cleaned before use so a crashed
/// prior run cannot poison this one.
fn scratch(label: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir();
    let dir = base.join(format!("trekr-e2e-{}-{label}", std::process::id()));
    let db = base.join(format!("trekr-e2e-{}-{label}.db", std::process::id()));
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
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// A git repo holding one fixture-sized Ruby file.
fn repo(dir: &Path) {
    git(dir, &["init", "-q"]);
    fs::write(
        dir.join("widget.rb"),
        "class Widget < Base\n  include Trackable\n\n  attr_reader :name\n\n  \
         def resize(width, height = 1)\n    helper\n  end\n\n  private\n\n  \
         def helper\n  end\nend\n",
    )
    .unwrap();
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
}

fn trekr(db: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_trekr"))
        .args(args)
        .current_dir(cwd)
        .env("TREKR_DB", db)
        .output()
        .expect("run trekr")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn json(out: &Output) -> serde_json::Value {
    serde_json::from_str(&stdout(out)).expect("structured output must be valid JSON")
}

#[test]
fn indexes_reports_and_outlines_through_the_cli() {
    let (dir, db) = scratch("basics");
    repo(&dir);

    let indexed = json(&trekr(&db, &dir, &["--index", "--json"]));
    assert_eq!(indexed["indexed"]["files"], 1);
    assert_eq!(indexed["indexed"]["parsed"], 1);
    assert!(
        indexed["indexed"]["defs"].as_i64().unwrap() >= 4,
        "class, attr_reader, and both methods are definitions: {indexed}"
    );

    let status = json(&trekr(&db, &dir, &["--status", "--json"]));
    assert_eq!(status["checkouts"][0]["files"], 1);
    assert_eq!(status["totals"]["blobs"], 1);

    let symbols = json(&trekr(&db, &dir, &["--symbols", "widget.rb", "--json"]));
    let names: Vec<&str> = symbols
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["Widget", "name", "resize", "helper"],
        "an outline follows the source, not the alphabet"
    );
    assert_eq!(symbols[3]["visibility"], "private");
    assert_eq!(symbols[2]["params"][1], "opt:height");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn reindexing_an_unchanged_checkout_parses_nothing() {
    let (dir, db) = scratch("noop");
    repo(&dir);

    trekr(&db, &dir, &["--index"]);
    let again = json(&trekr(&db, &dir, &["--index", "--json"]));
    assert_eq!(
        again["indexed"]["parsed"], 0,
        "same bytes, same blob, no work — the reason facts are OID-keyed"
    );
    assert_eq!(again["indexed"]["files"], 1, "the map is still complete");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn an_uncommitted_edit_is_indexed_like_any_other_content() {
    let (dir, db) = scratch("dirty");
    repo(&dir);
    trekr(&db, &dir, &["--index"]);

    fs::write(
        dir.join("widget.rb"),
        "class Widget\n  def added\n  end\nend\n",
    )
    .unwrap();
    let after = json(&trekr(&db, &dir, &["--index", "--json"]));
    assert_eq!(
        after["indexed"]["parsed"], 1,
        "the working tree is the truth, not HEAD"
    );

    let symbols = json(&trekr(&db, &dir, &["--symbols", "widget.rb", "--json"]));
    let names: Vec<&str> = symbols
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Widget", "added"]);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_second_worktree_of_the_same_content_costs_no_parsing() {
    let (dir, db) = scratch("worktree");
    repo(&dir);
    trekr(&db, &dir, &["--index"]);

    let clone = dir.with_extension("clone");
    let _ = fs::remove_dir_all(&clone);
    git(&dir, &["clone", "-q", ".", clone.to_str().unwrap()]);

    let second = json(&trekr(&db, &clone, &["--index", "--json"]));
    assert_eq!(
        second["indexed"]["parsed"], 0,
        "identical bytes are identical blobs, wherever they are checked out"
    );
    let status = json(&trekr(&db, &dir, &["--status", "--json"]));
    assert_eq!(status["checkouts"].as_array().unwrap().len(), 2);
    assert_eq!(
        status["totals"]["blobs"], 1,
        "two checkouts, one copy of the facts"
    );

    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&clone);
}

#[test]
fn every_command_speaks_ndjson_as_well_as_json() {
    let (dir, db) = scratch("ndjson");
    repo(&dir);
    trekr(&db, &dir, &["--index"]);

    for args in [
        vec!["--index", "--ndjson"],
        vec!["--status", "--ndjson"],
        vec!["--symbols", "widget.rb", "--ndjson"],
        vec!["--refs", "helper", "--ndjson"],
        vec!["--def", "widget.rb:1:7", "--ndjson"],
    ] {
        let out = trekr(&db, &dir, &args);
        for line in stdout(&out).lines() {
            serde_json::from_str::<serde_json::Value>(line)
                .unwrap_or_else(|e| panic!("{args:?} emitted a non-JSON line: {line} ({e})"));
        }
    }

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn refs_disclose_the_receiver_rather_than_guessing_at_it() {
    let (dir, db) = scratch("refs");
    repo(&dir);
    trekr(&db, &dir, &["--index"]);

    let refs = json(&trekr(&db, &dir, &["--refs", "helper", "--json"]));
    let seen: Vec<(&str, Option<&str>)> = refs
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["role"].as_str().unwrap(), r["recv"].as_str()))
        .collect();
    assert_eq!(
        seen,
        [("call", Some("implicit")), ("definition", None)],
        "source order, and every mention says what sort it is"
    );

    // A name-level answer includes the mixin's constant reference.
    let trackable = json(&trekr(&db, &dir, &["--refs", "Trackable", "--json"]));
    assert_eq!(trackable[0]["role"], "constant");

    assert_eq!(
        trekr(&db, &dir, &["--refs", "Absent"]).status.code(),
        Some(1),
        "a name nobody mentions is a definitive no"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// A repo whose namespace has something to resolve *through*.
fn nested_repo(dir: &Path) {
    git(dir, &["init", "-q"]);
    // Written a line at a time: a `\` continuation inside one string literal
    // is what `cargo fmt` reflows, and a silently renumbered fixture makes
    // every position in these tests wrong at once.
    let source = concat!(
        "module Shop\n",           //  1
        "  class Base\n",          //  2
        "    SIZE = 1\n",          //  3
        "    def helper\n",        //  4
        "    end\n",               //  5
        "  end\n",                 //  6
        "  class Widget < Base\n", //  7
        "    def go\n",            //  8
        "      SIZE\n",            //  9
        "      helper\n",          // 10
        "      thing.save\n",      // 11
        "    end\n",               // 12
        "  end\n",                 // 13
        "end\n",                   // 14
    );
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
}

#[test]
fn def_resolves_a_constant_through_the_ancestor_chain() {
    let (dir, db) = scratch("def");
    nested_repo(&dir);
    trekr(&db, &dir, &["--index"]);

    // `SIZE` on line 9 is not in Widget, but it is in Widget's superclass.
    let answer = json(&trekr(&db, &dir, &["--def", "app.rb:9:7", "--json"]));
    assert_eq!(answer["status"], "resolved");
    assert_eq!(answer["fqn"], "Shop::Base::SIZE");
    assert_eq!(answer["resolved_via"], "ancestor");
    assert_eq!(answer["confidence"], 1.0);
    assert_eq!(answer["sites"][0]["line"], 3);
    assert_eq!(
        trekr(&db, &dir, &["--def", "app.rb:9:7"]).status.code(),
        Some(0)
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn def_on_a_declaration_answers_with_the_declaration_itself() {
    let (dir, db) = scratch("defself");
    nested_repo(&dir);
    trekr(&db, &dir, &["--index"]);

    let answer = json(&trekr(&db, &dir, &["--def", "app.rb:7:9", "--json"]));
    assert_eq!(answer["under"], "definition");
    assert_eq!(answer["name"], "Widget");
    assert_eq!(answer["resolved_via"], "definition");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn def_resolves_an_implicit_call_through_the_ancestor_chain() {
    let (dir, db) = scratch("defcall");
    nested_repo(&dir);
    trekr(&db, &dir, &["--index"]);

    // `helper` on line 10 has no receiver, so Widget is the receiver, and
    // Widget inherits helper from Base.
    let answer = json(&trekr(&db, &dir, &["--def", "app.rb:10:7", "--json"]));
    assert_eq!(answer["under"], "call");
    assert_eq!(answer["status"], "resolved");
    assert_eq!(answer["resolved_via"], "self");
    assert_eq!(answer["receiver_type"], "Shop::Widget");
    assert_eq!(answer["owner"], "Shop::Base");
    assert_eq!(answer["sites"][0]["line"], 4);
    assert_eq!(answer["confidence"], 1.0);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn def_on_an_unknown_receiver_is_residue_that_still_offers_candidates() {
    let (dir, db) = scratch("defresidue");
    nested_repo(&dir);
    trekr(&db, &dir, &["--index"]);

    // `thing.save` on line 11: `thing` is a call, so the receiver is unknown.
    let answer = json(&trekr(&db, &dir, &["--def", "app.rb:11:13", "--json"]));
    assert_eq!(answer["under"], "call");
    assert_eq!(answer["name"], "save");
    assert_eq!(answer["status"], "residue");
    assert_eq!(
        answer["receiver"], "other",
        "the shape the ladder stalled on travels with the honest 'no'"
    );
    assert_eq!(
        trekr(&db, &dir, &["--def", "app.rb:11:13"]).status.code(),
        Some(1),
        "residue is a definitive answer, not an error"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn def_reads_the_working_tree_rather_than_the_index() {
    let (dir, db) = scratch("defdirty");
    nested_repo(&dir);
    trekr(&db, &dir, &["--index"]);

    // Push everything down a line; the answer must move with it.
    let source = fs::read_to_string(dir.join("app.rb")).unwrap();
    fs::write(dir.join("app.rb"), format!("# added\n{source}")).unwrap();
    let answer = json(&trekr(&db, &dir, &["--def", "app.rb:10:7", "--json"]));
    assert_eq!(
        answer["fqn"], "Shop::Base::SIZE",
        "the file is reparsed, so an unindexed edit does not shift the answer"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn ancestors_linearize_in_rubys_order() {
    let (dir, db) = scratch("anc");
    git(&dir, &["init", "-q"]);
    fs::write(
        dir.join("app.rb"),
        "module P\nend\nmodule I\nend\nclass Base\nend\n\
         class C < Base\n  include I\n  prepend P\nend\n",
    )
    .unwrap();
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
    trekr(&db, &dir, &["--index"]);

    let answer = json(&trekr(&db, &dir, &["--ancestors", "C", "--json"]));
    assert_eq!(
        answer["ancestors"].as_array().unwrap(),
        &vec!["P", "C", "I", "Base"]
    );
    assert_eq!(
        trekr(&db, &dir, &["--ancestors", "Nope"]).status.code(),
        Some(1)
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn nothing_to_report_is_an_exit_code_not_an_error() {
    let (dir, db) = scratch("empty");
    repo(&dir);

    // Never indexed: a definitive "no", distinct from a failure to serve.
    assert_eq!(trekr(&db, &dir, &["--status"]).status.code(), Some(1));
    assert_eq!(
        trekr(&db, &dir, &["--symbols", "widget.rb"]).status.code(),
        Some(1)
    );

    trekr(&db, &dir, &["--index"]);
    assert_eq!(trekr(&db, &dir, &["--status"]).status.code(), Some(0));

    // Dropping forgets the map but keeps the blobs another worktree may share.
    assert_eq!(trekr(&db, &dir, &["--drop"]).status.code(), Some(0));
    assert_eq!(
        trekr(&db, &dir, &["--drop"]).status.code(),
        Some(1),
        "dropping what is already gone did nothing"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_request_that_cannot_be_served_is_distinct_from_an_empty_answer() {
    let (dir, db) = scratch("notrepo");
    // A plain directory, deliberately not `git init`ed.
    let out = trekr(&db, &dir, &["--index"]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "'not a repository' is not the same answer as 'nothing here'"
    );

    let _ = fs::remove_dir_all(&dir);
}
